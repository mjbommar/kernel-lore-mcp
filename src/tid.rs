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
use crate::reader::Reader;

const SIDETABLE_FILENAME: &str = "tid.parquet";
const FALLBACK_WINDOW_NS: i64 = 30 * 24 * 3600 * 1_000_000_000; // 30 days
const PARQUET_BATCH_ROWS: usize = 100_000;

/// `(list, normalized_subject, from_addr) -> sorted Vec<(date_ns, mid)>`.
/// Hand-named so clippy stops complaining about the very-complex-type.
type SubjectBucket = BTreeMap<(String, String, String), Vec<(i64, String)>>;

/// Minimal projection of `MessageRow` carrying only the fields tid
/// computation needs. Avoids holding ~2-3 KB per row (trailers,
/// references, touched_files, etc.) across a 17.6M-row corpus —
/// which previously OOMed at ~44 GB RSS.
#[derive(Clone, Default)]
struct LiteRow {
    message_id: String,
    list: String,
    in_reply_to: Option<String>,
    references_first: Option<String>,
    subject_normalized: Option<String>,
    from_addr: Option<String>,
    date_unix_ns: Option<i64>,
    is_cover_letter: bool,
    /// Filled in by `assign_tids`. Empty before.
    tid: String,
}

/// Rebuild the tid side-table for `<data_dir>`. Returns the path
/// written + how many rows landed.
pub fn rebuild(data_dir: &Path) -> Result<(PathBuf, usize)> {
    let reader = Reader::new(data_dir);

    // Pass 1: stream lightweight rows. Drops trailers, touched_files,
    // etc. — ~10x smaller per row than MessageRow.
    let mut lite_rows: Vec<LiteRow> = Vec::new();
    reader.scan_streaming(None, |r| {
        lite_rows.push(LiteRow {
            message_id: r.message_id,
            list: r.list,
            in_reply_to: r.in_reply_to,
            references_first: r.references.into_iter().next().filter(|s| !s.is_empty()),
            subject_normalized: r.subject_normalized,
            from_addr: r.from_addr,
            date_unix_ns: r.date_unix_ns,
            is_cover_letter: r.is_cover_letter,
            tid: String::new(),
        });
        true
    })?;

    assign_tids(&mut lite_rows);

    // Pass 2: accumulate touched_files / touched_functions per
    // cover-letter tid. Other tids get nothing stored so the map
    // stays small even with a multi-million-row corpus.
    let cover_tids: HashSet<String> = lite_rows
        .iter()
        .filter(|r| r.is_cover_letter)
        .map(|r| r.tid.clone())
        .filter(|t| !t.is_empty())
        .collect();

    let tid_of_mid: HashMap<&str, &str> = lite_rows
        .iter()
        .map(|r| (r.message_id.as_str(), r.tid.as_str()))
        .collect();

    let mut propagated_files: HashMap<String, HashSet<String>> = HashMap::new();
    let mut propagated_funcs: HashMap<String, HashSet<String>> = HashMap::new();

    if !cover_tids.is_empty() {
        reader.scan_streaming(None, |row| {
            if let Some(&tid) = tid_of_mid.get(row.message_id.as_str()) {
                if cover_tids.contains(tid) {
                    let files = propagated_files.entry(tid.to_owned()).or_default();
                    for f in row.touched_files {
                        files.insert(f);
                    }
                    let funcs = propagated_funcs.entry(tid.to_owned()).or_default();
                    for f in row.touched_functions {
                        funcs.insert(f);
                    }
                }
            }
            true
        })?;
    }

    let out_dir = data_dir.join("tid");
    fs::create_dir_all(&out_dir)?;
    let final_path = out_dir.join(SIDETABLE_FILENAME);
    let tmp_path = out_dir.join(format!(".{SIDETABLE_FILENAME}.tmp"));

    let schema = sidetable_schema();
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).map_err(|e| Error::State(format!("zstd: {e}")))?,
        ))
        .build();
    let file = File::create(&tmp_path)?;
    let mut writer = ArrowWriter::try_new(file, schema.clone(), Some(props))?;

    // Interleave Parquet writes with over.db tid backfill so the big
    // `mid_tid: Vec<(String, String)>` never fully materializes. Old
    // code built a 17.6M-element Vec of (mid, tid) tuples — ~3 GB —
    // just to hand to `over.update_tids`. We now push chunks in as
    // we iterate lite_rows, reusing the same 50 k-row buffer.
    let over_path = data_dir.join("over.db");
    let mut over_conn = if over_path.exists() {
        Some(crate::over::OverDb::open(&over_path)?)
    } else {
        None
    };
    const OVER_CHUNK: usize = 50_000;
    let mut over_chunk: Vec<(String, String)> = if over_conn.is_some() {
        Vec::with_capacity(OVER_CHUNK)
    } else {
        Vec::new()
    };
    let mut over_updated: u64 = 0;

    let total = lite_rows.len();
    let mut chunk: Vec<TidRow> = Vec::with_capacity(PARQUET_BATCH_ROWS);
    for lite in &lite_rows {
        let (files, funcs) = if lite.is_cover_letter {
            let f = propagated_files
                .get(&lite.tid)
                .cloned()
                .unwrap_or_default();
            let fn_ = propagated_funcs
                .get(&lite.tid)
                .cloned()
                .unwrap_or_default();
            (sorted_vec(f), sorted_vec(fn_))
        } else {
            (Vec::new(), Vec::new())
        };
        chunk.push(TidRow {
            message_id: lite.message_id.clone(),
            tid: lite.tid.clone(),
            propagated_files: files,
            propagated_functions: funcs,
        });
        if chunk.len() >= PARQUET_BATCH_ROWS {
            writer.write(&build_batch(&schema, &chunk)?)?;
            chunk.clear();
        }
        if over_conn.is_some() {
            over_chunk.push((lite.message_id.clone(), lite.tid.clone()));
            if over_chunk.len() >= OVER_CHUNK
                && let Some(db) = over_conn.as_mut()
            {
                over_updated += db.update_tids(&over_chunk)?;
                over_chunk.clear();
            }
        }
    }
    if !chunk.is_empty() {
        writer.write(&build_batch(&schema, &chunk)?)?;
    }
    writer.close()?;
    fs::rename(&tmp_path, &final_path)?;

    if !over_chunk.is_empty()
        && let Some(db) = over_conn.as_mut()
    {
        over_updated += db.update_tids(&over_chunk)?;
        over_chunk.clear();
    }
    if over_conn.is_some() {
        tracing::info!(
            updated = over_updated,
            total = total,
            "over.db tid column backfilled"
        );
    }

    Ok((final_path, total))
}

/// Resolve a tid for every LiteRow in place.
fn assign_tids(rows: &mut [LiteRow]) {
    if rows.is_empty() {
        return;
    }

    let by_mid: HashMap<&str, usize> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.message_id.as_str(), i))
        .collect();

    let mut subj_bucket: SubjectBucket = BTreeMap::new();
    for r in rows.iter() {
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

    let resolved: Vec<String> = (0..rows.len())
        .map(|i| resolve_tid_lite(&rows[i], rows, &by_mid, &subj_bucket))
        .collect();
    for (r, tid) in rows.iter_mut().zip(resolved.into_iter()) {
        r.tid = tid;
    }
}

fn resolve_tid_lite(
    row: &LiteRow,
    rows: &[LiteRow],
    by_mid: &HashMap<&str, usize>,
    subj_bucket: &SubjectBucket,
) -> String {
    let mut current = row.message_id.clone();
    let mut visited = HashSet::new();
    visited.insert(current.clone());
    while let Some(&idx) = by_mid.get(current.as_str()) {
        let cur = &rows[idx];
        let parent_mid = cur
            .in_reply_to
            .clone()
            .or_else(|| cur.references_first.clone());
        let Some(parent) = parent_mid else {
            break;
        };
        if !by_mid.contains_key(parent.as_str()) {
            break;
        }
        if !visited.insert(parent.clone()) {
            break;
        }
        current = parent;
    }

    if current != row.message_id {
        return current;
    }

    if let (Some(subj), date) = (&row.subject_normalized, row.date_unix_ns) {
        let from = row.from_addr.clone().unwrap_or_default();
        let key = (row.list.clone(), subj.clone(), from);
        if let Some(bucket) = subj_bucket.get(&key) {
            let lo = date.unwrap_or(i64::MIN).saturating_sub(FALLBACK_WINDOW_NS);
            for (d, mid) in bucket {
                if *d >= lo && *d <= date.unwrap_or(i64::MAX) {
                    return mid.clone();
                }
            }
        }
    }

    row.message_id.clone()
}

/// One side-table row.
#[derive(Debug, Clone)]
pub struct TidRow {
    pub message_id: String,
    pub tid: String,
    pub propagated_files: Vec<String>,
    pub propagated_functions: Vec<String>,
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
