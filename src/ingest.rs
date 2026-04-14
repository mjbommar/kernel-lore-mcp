//! End-to-end ingest: walk one public-inbox shard, append bodies to the
//! compressed store, accumulate metadata rows, flush to Parquet.

#![allow(dead_code)]
//!
//! v0.5 scope:
//!   * single-shard walker (multi-shard / multi-list fanout is the CLI
//!     binary's job; we keep this function small and testable)
//!   * incremental via `state::last_indexed_oid` when present;
//!     full-walk fallback if the stored OID is dangling upstream
//!   * one Parquet file per shard per run
//!   * bump `state::generation` once after every commit of the writer
//!     to signal readers
//!
//! Deliberately NOT in scope yet:
//!   * tantivy + trigram writes (phases 2/3)
//!   * tid (thread-id) computation (needs cross-message join)
//!   * cover-letter → patch touched-file propagation (ditto)

use std::path::Path;

use gix::ObjectId;

use crate::error::{Error, Result};
use crate::metadata::{self, MetadataBatch, MetadataRow};
use crate::parse;
use crate::state::State;
use crate::store::Store;

/// Ingest one public-inbox shard end-to-end.
///
/// `shard_path` points at a bare git repo (the `N.git` directory for a
/// single public-inbox shard). `list` is the list name (e.g.
/// `linux-cifs`); `shard` is the shard number as a string.
pub fn ingest_shard(
    data_dir: &Path,
    shard_path: &Path,
    list: &str,
    shard: &str,
    run_id: &str,
) -> Result<IngestStats> {
    let state = State::new(data_dir)?;
    let _lock = state.acquire_writer_lock()?;
    let store = Store::open(data_dir, list)?;

    let repo = gix::open(shard_path)
        .map_err(|e| Error::Gix(format!("open {}: {e}", shard_path.display())))?;

    let head_id: ObjectId = repo
        .head_id()
        .map_err(|e| Error::Gix(format!("head_id: {e}")))?
        .detach();
    let head_hex = head_id.to_string();

    // Build the walk; use incremental when we have a last-indexed oid
    // and that oid is still reachable.
    let mut platform = repo.rev_walk([head_id]);
    let last = state.last_indexed_oid(list, shard)?;
    if let Some(ref oid_hex) = last {
        if let Ok(parsed) = oid_hex.parse::<ObjectId>() {
            if repo.find_object(parsed).is_ok() {
                platform = platform.with_hidden([parsed]);
            }
            // else: dangling (shard was repacked upstream); full walk.
        }
    }

    let walk = platform
        .all()
        .map_err(|e| Error::Gix(format!("rev_walk: {e}")))?;

    let mut batch = MetadataBatch::new();
    let mut stats = IngestStats::default();

    for info in walk {
        let info = info.map_err(|e| Error::Gix(format!("walk step: {e}")))?;
        let commit = info
            .object()
            .map_err(|e| Error::Gix(format!("commit object: {e}")))?;
        let tree = commit
            .tree()
            .map_err(|e| Error::Gix(format!("commit tree: {e}")))?;
        let Some(m) = tree.find_entry("m") else {
            stats.skipped_no_m += 1;
            continue;
        };
        let blob = m
            .object()
            .map_err(|e| Error::Gix(format!("blob object: {e}")))?;
        let data = &blob.data;
        if data.is_empty() {
            stats.skipped_empty += 1;
            continue;
        }

        let parsed = parse::parse_message(data);
        if parsed.message_id.is_none() {
            stats.skipped_no_mid += 1;
            continue;
        }

        let appended = store.append(data)?;
        let row = MetadataRow {
            list,
            shard,
            commit_oid: &info.id.to_string(),
            offset: appended.ptr,
            body_sha256_hex: hex(&appended.body_sha256),
            body_length: appended.body_length,
            parsed,
        };
        batch.push(row);
        stats.ingested += 1;
    }

    store.flush()?;

    if batch.is_empty() {
        // Nothing new; still advance the oid so we don't re-walk.
        state.save_last_indexed_oid(list, shard, &head_hex)?;
        return Ok(stats);
    }

    let rb = batch.finish()?;
    let parquet_path = metadata::write_parquet(data_dir, list, run_id, &rb)?;
    stats.parquet_path = Some(parquet_path.display().to_string());
    state.save_last_indexed_oid(list, shard, &head_hex)?;
    state.bump_generation()?;

    Ok(stats)
}

/// Aggregate counters from a single `ingest_shard` invocation.
#[derive(Debug, Default, Clone)]
pub struct IngestStats {
    pub ingested: u64,
    pub skipped_no_m: u64,
    pub skipped_empty: u64,
    pub skipped_no_mid: u64,
    pub parquet_path: Option<String>,
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    /// Build a minimal "public-inbox-like" shard: a bare repo with one
    /// commit per message, each commit's tree containing a single blob
    /// named `m` holding the raw RFC822 message.
    fn make_synthetic_shard(shard_dir: &Path, messages: &[&[u8]]) {
        let run = |args: &[&str], cwd: &Path| {
            let out = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "tester")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "tester")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        };

        // Build in a working tree, then clone --bare into shard_dir.
        let work = tempdir().unwrap();
        run(&["init", "-q", "-b", "master", "."], work.path());
        for (i, msg) in messages.iter().enumerate() {
            std::fs::write(work.path().join("m"), msg).unwrap();
            run(&["add", "m"], work.path());
            run(&["commit", "-q", "-m", &format!("msg {i}")], work.path());
        }
        // Clone --bare into final location.
        if shard_dir.exists() {
            std::fs::remove_dir_all(shard_dir).unwrap();
        }
        run(
            &[
                "clone",
                "--bare",
                "-q",
                work.path().to_str().unwrap(),
                shard_dir.to_str().unwrap(),
            ],
            Path::new("/"),
        );
    }

    fn sample_messages() -> Vec<Vec<u8>> {
        vec![
            b"From: Alice <alice@example.com>\r\n\
Subject: [PATCH v2 1/2] ksmbd: tighten ACL bounds\r\n\
Date: Mon, 14 Apr 2026 12:00:00 +0000\r\n\
Message-ID: <m1@x>\r\n\
\r\n\
Prose.\r\n\
Signed-off-by: Alice <alice@example.com>\r\n\
---\r\n\
diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c\r\n\
--- a/fs/smb/server/smbacl.c\r\n\
+++ b/fs/smb/server/smbacl.c\r\n\
@@ -1,1 +1,2 @@ int smb_check_perm_dacl(struct ksmbd_conn *c)\r\n\
 a\r\n\
+b\r\n"
                .to_vec(),
            b"From: Bob <bob@example.com>\r\n\
Subject: [PATCH v2 2/2] ksmbd: fix follow-up\r\n\
Date: Mon, 14 Apr 2026 12:05:00 +0000\r\n\
Message-ID: <m2@x>\r\n\
In-Reply-To: <m1@x>\r\n\
References: <m1@x>\r\n\
\r\n\
More prose.\r\n\
Signed-off-by: Bob <bob@example.com>\r\n\
---\r\n\
diff --git a/fs/smb/server/smb2pdu.c b/fs/smb/server/smb2pdu.c\r\n\
--- a/fs/smb/server/smb2pdu.c\r\n\
+++ b/fs/smb/server/smb2pdu.c\r\n\
@@ -1,1 +1,2 @@ int smb2_create(struct ksmbd_conn *c)\r\n\
 a\r\n\
+b\r\n"
                .to_vec(),
        ]
    }

    #[test]
    fn end_to_end_ingest_synthetic_shard() {
        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let msgs = sample_messages();
        let msg_refs: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
        make_synthetic_shard(&shard_dir, &msg_refs);

        let data = tempdir().unwrap();
        let stats = ingest_shard(data.path(), &shard_dir, "linux-cifs", "0", "run-0001").unwrap();

        assert_eq!(stats.ingested, 2);
        assert!(stats.parquet_path.is_some());

        // State recorded.
        let state = State::new(data.path()).unwrap();
        assert!(state.last_indexed_oid("linux-cifs", "0").unwrap().is_some());
        assert_eq!(state.generation().unwrap(), 1);

        // Parquet file exists and is non-trivial.
        let p = data.path().join("metadata/linux-cifs/run-0001.parquet");
        assert!(p.exists());
        assert!(p.metadata().unwrap().len() > 500);

        // Store has segment-000000.zst with both messages.
        let seg = data.path().join("store/linux-cifs/segment-000000.zst");
        assert!(seg.exists());
        assert!(seg.metadata().unwrap().len() > 0);
    }

    #[test]
    fn incremental_skip_on_second_run() {
        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let msgs = sample_messages();
        let msg_refs: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
        make_synthetic_shard(&shard_dir, &msg_refs);

        let data = tempdir().unwrap();
        let first = ingest_shard(data.path(), &shard_dir, "l", "0", "a").unwrap();
        assert_eq!(first.ingested, 2);
        let second = ingest_shard(data.path(), &shard_dir, "l", "0", "b").unwrap();
        assert_eq!(second.ingested, 0);
        assert!(second.parquet_path.is_none());
        let state = State::new(data.path()).unwrap();
        // Generation only bumps on new data.
        assert_eq!(state.generation().unwrap(), 1);
    }
}
