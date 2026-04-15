//! Thread-id (`tid`) computation + cover-letter propagation.
//!
//! After every ingest run, walk all metadata Parquet rows and:
//!   1. Compute a `tid` per message by following the In-Reply-To /
//!      References chain back to a thread root. Fallback when the
//!      chain breaks: subject_normalized + from_addr + 30-day window
//!      (the public-inbox heuristic).
//!   2. Propagate `touched_files[]` + `touched_functions[]` from
//!      sibling patches into cover-letter rows in the same `tid`.
//!
//! Output is a side-table parquet `<data_dir>/tid/tid.parquet` with
//! columns `(message_id, tid, propagated_files[], propagated_functions[])`.
//! The reader joins it on `message_id` at query time.
//!
//! Idempotent: each invocation rewrites the side-table from scratch.
//! Cheap relative to ingest because we don't touch the body store.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{ArrayRef, ListBuilder, RecordBatch, StringBuilder};
use arrow::datatypes::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use crate::error::{Error, Result};
use crate::reader::{MessageRow, Reader};

const SIDETABLE_FILENAME: &str = "tid.parquet";
const FALLBACK_WINDOW_NS: i64 = 30 * 24 * 3600 * 1_000_000_000; // 30 days

/// `(list, normalized_subject, from_addr) -> sorted Vec<(date_ns, mid)>`.
/// Hand-named so clippy stops complaining about the very-complex-type.
type SubjectBucket = BTreeMap<(String, String, String), Vec<(i64, String)>>;

/// Rebuild the tid side-table for `<data_dir>`. Returns the path
/// written + how many rows landed.
pub fn rebuild(data_dir: &Path) -> Result<(PathBuf, usize)> {
    let reader = Reader::new(data_dir);
    let rows = collect_all_rows(&reader)?;
    let computed = compute(&rows);

    let out_dir = data_dir.join("tid");
    fs::create_dir_all(&out_dir)?;
    let path = out_dir.join(SIDETABLE_FILENAME);

    let schema = sidetable_schema();
    let batch = build_batch(&schema, &computed)?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).map_err(|e| Error::State(format!("zstd: {e}")))?,
        ))
        .build();
    let file = File::create(&path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
    writer.write(&batch)?;
    writer.close()?;
    Ok((path, computed.len()))
}

/// One side-table row.
#[derive(Debug, Clone)]
pub struct TidRow {
    pub message_id: String,
    pub tid: String,
    pub propagated_files: Vec<String>,
    pub propagated_functions: Vec<String>,
}

fn collect_all_rows(reader: &Reader) -> Result<Vec<MessageRow>> {
    let mut all = Vec::new();
    reader.scan_all(&mut all)?;
    Ok(all)
}

/// Pure compute: given the full row set, return the side-table.
pub fn compute(rows: &[MessageRow]) -> Vec<TidRow> {
    if rows.is_empty() {
        return Vec::new();
    }

    // Index by message_id for parent lookups.
    let by_mid: HashMap<&str, &MessageRow> =
        rows.iter().map(|r| (r.message_id.as_str(), r)).collect();

    // Subject-fallback bucket: (list, normalized_subject, from_addr) →
    // sorted Vec<(date_ns, mid)>. Cheap pre-pass enables the 30-day
    // window heuristic when the reply graph is broken.
    let mut subj_bucket: SubjectBucket = BTreeMap::new();
    for r in rows {
        if let Some(subj) = &r.subject_normalized {
            let from = r.from_addr.clone().unwrap_or_default();
            let date = r.date_unix_ns.unwrap_or(0);
            subj_bucket
                .entry((r.list.clone(), subj.clone(), from))
                .or_default()
                .push((date, r.message_id.clone()));
        }
    }
    for v in subj_bucket.values_mut() {
        v.sort_by_key(|(d, _)| *d);
    }

    // Resolve tid per message_id by walking the parent chain.
    let mut tid_of: HashMap<String, String> = HashMap::new();
    for row in rows {
        let tid = resolve_tid(row, &by_mid, &subj_bucket);
        tid_of.insert(row.message_id.clone(), tid);
    }

    // Propagate touched_files / touched_functions per tid (cover
    // letters inherit from patches; non-covers keep their own).
    let mut by_tid: HashMap<&str, Vec<&MessageRow>> = HashMap::new();
    for r in rows {
        if let Some(tid) = tid_of.get(&r.message_id) {
            by_tid.entry(tid.as_str()).or_default().push(r);
        }
    }
    let mut propagated_files_per_tid: HashMap<&str, HashSet<String>> = HashMap::new();
    let mut propagated_funcs_per_tid: HashMap<&str, HashSet<String>> = HashMap::new();
    for (tid, members) in &by_tid {
        let mut files: HashSet<String> = HashSet::new();
        let mut funcs: HashSet<String> = HashSet::new();
        for m in members {
            for f in &m.touched_files {
                files.insert(f.clone());
            }
            for f in &m.touched_functions {
                funcs.insert(f.clone());
            }
        }
        propagated_files_per_tid.insert(tid, files);
        propagated_funcs_per_tid.insert(tid, funcs);
    }

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let tid = tid_of.get(&row.message_id).cloned().unwrap_or_default();
        // Only cover letters get propagation; everyone else keeps
        // their own touched_files. (We still write the columns so a
        // reader join doesn't have to special-case absence.)
        let (files, funcs) = if row.is_cover_letter {
            let f = propagated_files_per_tid
                .get(tid.as_str())
                .cloned()
                .unwrap_or_default();
            let fn_ = propagated_funcs_per_tid
                .get(tid.as_str())
                .cloned()
                .unwrap_or_default();
            (sorted_vec(f), sorted_vec(fn_))
        } else {
            (Vec::new(), Vec::new())
        };
        out.push(TidRow {
            message_id: row.message_id.clone(),
            tid,
            propagated_files: files,
            propagated_functions: funcs,
        });
    }
    out
}

fn resolve_tid(
    row: &MessageRow,
    by_mid: &HashMap<&str, &MessageRow>,
    subj_bucket: &SubjectBucket,
) -> String {
    // 1. Walk the parent chain via in_reply_to first; fall back to
    //    references[0] (typically the thread root in
    //    well-formed messages).
    let mut current = row.message_id.clone();
    let mut visited = HashSet::new();
    visited.insert(current.clone());
    while let Some(cur_row) = by_mid.get(current.as_str()).copied() {
        let parent_mid = cur_row.in_reply_to.clone().or_else(|| {
            cur_row
                .references
                .first()
                .cloned()
                .filter(|p| !p.is_empty())
        });
        let Some(parent) = parent_mid else {
            break;
        };
        if !by_mid.contains_key(parent.as_str()) {
            break; // dangling parent
        }
        if !visited.insert(parent.clone()) {
            break; // cycle (shouldn't happen, but be defensive)
        }
        current = parent;
    }

    // If we got past the seed, current is a valid root in our corpus.
    if current != row.message_id {
        return current;
    }

    // 2. Subject-normalized + from + 30-day window fallback.
    if let (Some(subj), date) = (&row.subject_normalized, row.date_unix_ns) {
        let from = row.from_addr.clone().unwrap_or_default();
        let key = (row.list.clone(), subj.clone(), from);
        if let Some(bucket) = subj_bucket.get(&key) {
            // Pick the earliest mid in the same window.
            let lo = date.unwrap_or(i64::MIN).saturating_sub(FALLBACK_WINDOW_NS);
            for (d, mid) in bucket {
                if *d >= lo && *d <= date.unwrap_or(i64::MAX) {
                    return mid.clone();
                }
            }
        }
    }

    // 3. Singleton: tid = self.
    row.message_id.clone()
}

fn sidetable_schema() -> Arc<Schema> {
    let item = DataType::List(Arc::new(Field::new("item", DataType::Utf8, true)));
    Arc::new(Schema::new(vec![
        Field::new("message_id", DataType::Utf8, false),
        Field::new("tid", DataType::Utf8, false),
        Field::new("propagated_files", item.clone(), true),
        Field::new("propagated_functions", item, true),
    ]))
}

fn build_batch(schema: &Arc<Schema>, rows: &[TidRow]) -> Result<RecordBatch> {
    let mut mid = StringBuilder::new();
    let mut tid = StringBuilder::new();
    let mut files = ListBuilder::new(StringBuilder::new());
    let mut funcs = ListBuilder::new(StringBuilder::new());
    for r in rows {
        mid.append_value(&r.message_id);
        tid.append_value(&r.tid);
        if r.propagated_files.is_empty() {
            files.append(false);
        } else {
            for f in &r.propagated_files {
                files.values().append_value(f);
            }
            files.append(true);
        }
        if r.propagated_functions.is_empty() {
            funcs.append(false);
        } else {
            for f in &r.propagated_functions {
                funcs.values().append_value(f);
            }
            funcs.append(true);
        }
    }
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(mid.finish()),
        Arc::new(tid.finish()),
        Arc::new(files.finish()),
        Arc::new(funcs.finish()),
    ];
    Ok(RecordBatch::try_new(schema.clone(), arrays)?)
}

fn sorted_vec(set: HashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::ingest_shard;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    fn make_shard(dir: &Path, msgs: &[&[u8]]) {
        let run = |args: &[&str], cwd: &Path| {
            let out = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let work = tempdir().unwrap();
        run(&["init", "-q", "-b", "master", "."], work.path());
        for (i, m) in msgs.iter().enumerate() {
            std::fs::write(work.path().join("m"), m).unwrap();
            run(&["add", "m"], work.path());
            run(&["commit", "-q", "-m", &format!("c{i}")], work.path());
        }
        if dir.exists() {
            std::fs::remove_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        run(
            &[
                "clone",
                "--bare",
                "-q",
                work.path().to_str().unwrap(),
                dir.to_str().unwrap(),
            ],
            Path::new("/"),
        );
    }

    /// Cover letter (m0) + two patches that point at m0 via
    /// in_reply_to. Compute should:
    ///   - assign tid = "m0@x" to all three.
    ///   - propagate the union of touched files/functions to m0.
    #[test]
    fn cover_letter_inherits_touched_files_and_functions() {
        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let msgs: [&[u8]; 3] = [
            b"From: Alice <a@x>\r\n\
Subject: [PATCH 0/2] ksmbd cleanup\r\n\
Date: Mon, 14 Apr 2026 12:00:00 +0000\r\n\
Message-ID: <m0@x>\r\n\
\r\n\
Cover letter body.\r\n",
            b"From: Alice <a@x>\r\n\
Subject: [PATCH 1/2] ksmbd: bound ACL size\r\n\
Date: Mon, 14 Apr 2026 12:01:00 +0000\r\n\
Message-ID: <m1@x>\r\n\
In-Reply-To: <m0@x>\r\n\
References: <m0@x>\r\n\
\r\n\
First patch.\r\n\
---\r\n\
diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c\r\n\
--- a/fs/smb/server/smbacl.c\r\n\
+++ b/fs/smb/server/smbacl.c\r\n\
@@ -1,1 +1,2 @@ int smb_check_perm_dacl(struct ksmbd_conn *c)\r\n\
 a\r\n\
+b\r\n",
            b"From: Alice <a@x>\r\n\
Subject: [PATCH 2/2] ksmbd: tighten path canon\r\n\
Date: Mon, 14 Apr 2026 12:02:00 +0000\r\n\
Message-ID: <m2@x>\r\n\
In-Reply-To: <m0@x>\r\n\
References: <m0@x>\r\n\
\r\n\
Second patch.\r\n\
---\r\n\
diff --git a/fs/smb/server/path.c b/fs/smb/server/path.c\r\n\
--- a/fs/smb/server/path.c\r\n\
+++ b/fs/smb/server/path.c\r\n\
@@ -1,1 +1,2 @@ int ksmbd_canonical_path(char *p)\r\n\
 a\r\n\
+b\r\n",
        ];
        make_shard(&shard_dir, &msgs);

        let data = tempdir().unwrap();
        ingest_shard(data.path(), &shard_dir, "linux-cifs", "0", "run-1").unwrap();

        let (path, n) = rebuild(data.path()).unwrap();
        assert!(path.exists());
        assert_eq!(n, 3);

        // Re-read via Reader::tid_lookup (added in this commit).
        let r = Reader::new(data.path());
        let tids = r.tid_lookup().unwrap();
        // All three share tid = "m0@x".
        assert_eq!(tids.get("m0@x").map(|s| s.as_str()), Some("m0@x"));
        assert_eq!(tids.get("m1@x").map(|s| s.as_str()), Some("m0@x"));
        assert_eq!(tids.get("m2@x").map(|s| s.as_str()), Some("m0@x"));

        // Cover letter inherits both files + both function names.
        let prop = r.propagated_lookup().unwrap();
        let cover = prop.get("m0@x").expect("cover propagation row");
        let files: HashSet<&str> = cover.0.iter().map(String::as_str).collect();
        let funcs: HashSet<&str> = cover.1.iter().map(String::as_str).collect();
        assert!(files.contains("fs/smb/server/smbacl.c"));
        assert!(files.contains("fs/smb/server/path.c"));
        assert!(funcs.contains("smb_check_perm_dacl"));
        assert!(funcs.contains("ksmbd_canonical_path"));

        // Non-cover messages do NOT carry propagated_files (they have
        // their own touched_files in the metadata tier).
        let m1 = prop.get("m1@x").expect("m1 row");
        assert!(m1.0.is_empty());
        assert!(m1.1.is_empty());
    }
}
