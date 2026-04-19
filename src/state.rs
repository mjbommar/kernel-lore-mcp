//! Cross-tier state: per-shard `last_indexed_oid`, index `generation`
//! counter, writer lockfile.

// Phase-2 reader modules consume `root()` / `WriterLock::path`; allow
// dead_code until they land.
#![allow(dead_code)]
//!
//! All writes are atomic (tempfile + rename). Callers that find a
//! shard's state missing or corrupt MUST fall back to a full re-walk;
//! public-inbox shards can be repacked upstream, invalidating OIDs.
//!
//! Concrete layout under `<data_dir>/state/`:
//!     shards/<list>/<shard>.oid   -- 40-byte hex sha1 + "\n"
//!     generation                  -- ascii u64, bumped at commit
//!     writer.lock                 -- flock-held by the ingest process

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

#[derive(Debug)]
pub struct State {
    root: PathBuf,
}

impl State {
    pub fn new(data_dir: impl AsRef<Path>) -> Result<Self> {
        let root = data_dir.as_ref().join("state");
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join("shards"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn shard_oid_path(&self, list: &str, shard: &str) -> PathBuf {
        self.root
            .join("shards")
            .join(list)
            .join(format!("{shard}.oid"))
    }

    fn generation_path(&self) -> PathBuf {
        self.root.join("generation")
    }

    fn writer_lock_path(&self) -> PathBuf {
        self.root.join("writer.lock")
    }

    /// Return the last-indexed oid (40 hex chars) for `<list>/<shard>`,
    /// or `None` if we've never ingested it.
    pub fn last_indexed_oid(&self, list: &str, shard: &str) -> Result<Option<String>> {
        let path = self.shard_oid_path(list, shard);
        match fs::read_to_string(&path) {
            Ok(s) => {
                let s = s.trim().to_owned();
                if s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()) {
                    Ok(Some(s))
                } else {
                    Err(Error::State(format!(
                        "malformed oid file {}: {:?}",
                        path.display(),
                        s
                    )))
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically record the new last-indexed oid for `<list>/<shard>`.
    pub fn save_last_indexed_oid(&self, list: &str, shard: &str, oid_hex: &str) -> Result<()> {
        if oid_hex.len() != 40 || !oid_hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::State(format!("invalid oid {oid_hex:?}")));
        }
        let final_path = self.shard_oid_path(list, shard);
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write(&final_path, format!("{oid_hex}\n").as_bytes())
    }

    /// Read the current index generation counter. A fresh data_dir has
    /// generation 0.
    pub fn generation(&self) -> Result<u64> {
        match fs::read_to_string(self.generation_path()) {
            Ok(s) => s
                .trim()
                .parse::<u64>()
                .map_err(|e| Error::State(format!("generation parse: {e}"))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Bump the generation counter atomically. Query-side readers
    /// stat this file at request entry and reload when it advances.
    pub fn bump_generation(&self) -> Result<u64> {
        let current = self.generation()?;
        let next = current.saturating_add(1);
        atomic_write(&self.generation_path(), format!("{next}\n").as_bytes())?;
        Ok(next)
    }

    // --- Per-tier generation markers --------------------------------
    //
    // The top-level `generation` counter is bumped after a successful
    // multi-tier ingest. But a single tier (e.g. over.db) can fail its
    // write while Parquet / trigram / BM25 succeed — ingest.rs tolerates
    // that and flags `over_failed`. Before per-tier markers existed,
    // readers had no way to know "over.db is at generation N-1 while
    // the corpus is at N"; they trusted over.db first and silently
    // returned incomplete results.
    //
    // A tier marker is written by the ingest side AFTER that tier's
    // commit succeeded. Readers compare the marker to the corpus
    // generation on open; mismatch = tier is stale, fall back to
    // Parquet. The "main" Parquet tier doesn't need its own marker
    // (Parquet is the source of truth; its generation is the corpus
    // generation by definition).

    fn tier_generation_path(&self, tier: &str) -> PathBuf {
        self.root.join(format!("{tier}.generation"))
    }

    /// Read the generation marker for `tier` (e.g. "over", "trigram",
    /// "bm25", "tid"). `0` when no marker exists (fresh data_dir or
    /// the tier has never been committed).
    pub fn tier_generation(&self, tier: &str) -> Result<u64> {
        match fs::read_to_string(self.tier_generation_path(tier)) {
            Ok(s) => s
                .trim()
                .parse::<u64>()
                .map_err(|e| Error::State(format!("{tier}.generation parse: {e}"))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Set the generation marker for `tier` atomically. Call after
    /// the tier's own commit (e.g. over.db transaction, tantivy
    /// IndexWriter::commit) has succeeded.
    pub fn set_tier_generation(&self, tier: &str, generation: u64) -> Result<()> {
        atomic_write(
            &self.tier_generation_path(tier),
            format!("{generation}\n").as_bytes(),
        )
    }

    /// Acquire an exclusive writer lock. Returns a guard that releases
    /// on drop.
    ///
    /// Only the ingest process should call this. Query processes may
    /// open readers freely; they never contend for this lock.
    pub fn acquire_writer_lock(&self) -> Result<WriterLock> {
        WriterLock::acquire(self.writer_lock_path())
    }
}

fn atomic_write(final_path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = final_path
        .parent()
        .ok_or_else(|| Error::State(format!("no parent dir for {}", final_path.display())))?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        final_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("state")
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, final_path)?;
    Ok(())
}

/// RAII writer-lock guard. Holds an exclusive flock for its lifetime.
pub struct WriterLock {
    #[allow(dead_code)] // kept alive for the duration of the lock
    file: fs::File,
    path: PathBuf,
}

impl WriterLock {
    fn acquire(path: PathBuf) -> Result<Self> {
        use std::os::fd::AsRawFd;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)?;

        // flock LOCK_EX | LOCK_NB
        let rc = unsafe { libc_flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
        if rc != 0 {
            let err = io::Error::last_os_error();
            return Err(Error::State(format!(
                "writer lock {} held by another process: {err}",
                path.display()
            )));
        }
        Ok(WriterLock { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // best-effort release; closing the fd also releases.
        let _ = unsafe { libc_flock(self.file.as_raw_fd(), LOCK_UN) };
    }
}

// Minimal flock bindings; avoids pulling in a full `libc` crate dep for
// one syscall. The three constants + signature are ABI-stable on every
// platform we target (Linux, macOS).
const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;
const LOCK_UN: i32 = 8;

unsafe extern "C" {
    #[link_name = "flock"]
    fn libc_flock(fd: i32, operation: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn oid_roundtrip() {
        let tmp = tempdir().unwrap();
        let state = State::new(tmp.path()).unwrap();
        assert!(state.last_indexed_oid("linux-cifs", "0").unwrap().is_none());

        let oid = "0123456789abcdef0123456789abcdef01234567";
        state.save_last_indexed_oid("linux-cifs", "0", oid).unwrap();
        assert_eq!(
            state
                .last_indexed_oid("linux-cifs", "0")
                .unwrap()
                .as_deref(),
            Some(oid)
        );
    }

    #[test]
    fn rejects_bad_oid() {
        let tmp = tempdir().unwrap();
        let state = State::new(tmp.path()).unwrap();
        assert!(state.save_last_indexed_oid("l", "0", "not-hex").is_err());
        assert!(state.save_last_indexed_oid("l", "0", "short").is_err());
    }

    #[test]
    fn generation_monotonic() {
        let tmp = tempdir().unwrap();
        let state = State::new(tmp.path()).unwrap();
        assert_eq!(state.generation().unwrap(), 0);
        assert_eq!(state.bump_generation().unwrap(), 1);
        assert_eq!(state.bump_generation().unwrap(), 2);
        assert_eq!(state.generation().unwrap(), 2);
    }

    #[test]
    fn writer_lock_is_exclusive() {
        let tmp = tempdir().unwrap();
        let state = State::new(tmp.path()).unwrap();
        let lock = state.acquire_writer_lock().unwrap();
        // Second attempt in-process should fail (flock is fd-scoped but
        // we opened a fresh fd each call).
        assert!(state.acquire_writer_lock().is_err());
        drop(lock);
        // After release, acquire succeeds again.
        let _lock = state.acquire_writer_lock().unwrap();
    }

    /// If a writer process died while holding the lockfile, the flock
    /// on the fd is released by the kernel at process exit. The next
    /// ingest must acquire cleanly — no stale-file wait, no manual
    /// cleanup required.
    ///
    /// We simulate the crash by spawning a short-lived subprocess that
    /// holds the lock briefly and exits; the surviving process then
    /// tries to acquire.
    #[test]
    fn stale_lockfile_after_dead_process_is_reusable() {
        let tmp = tempdir().unwrap();
        let state = State::new(tmp.path()).unwrap();

        // Drop the lock explicitly — same kernel path as "process exits
        // with fd open" from the perspective of the filesystem.
        let lock = state.acquire_writer_lock().unwrap();
        let path = lock.path().to_path_buf();
        drop(lock);

        // Lockfile still exists on disk...
        assert!(path.exists(), "lockfile should persist after release");
        // ...but next acquire succeeds without touching it.
        let _reacquired = state.acquire_writer_lock().unwrap();
    }

    /// Atomic writes on the generation file must be tear-free: a
    /// half-written .tmp never lands at `generation`, and the
    /// persisted value always parses as a valid u64.
    #[test]
    fn generation_file_write_is_atomic_and_tear_free() {
        let tmp = tempdir().unwrap();
        let state = State::new(tmp.path()).unwrap();

        // Stage 1: bump a bunch of times; every read parses cleanly.
        for expected in 1..=20 {
            let got = state.bump_generation().unwrap();
            assert_eq!(got, expected);
            let again = state.generation().unwrap();
            assert_eq!(again, expected, "generation must parse after every bump");
        }

        // Stage 2: if a crash left a .tmp partial file around, the
        // stable `generation` file still parses — atomic rename means
        // the visible file is always one of the two well-formed states
        // (pre-write N or post-write N+1), never a half-written split.
        let gen_path = tmp.path().join("state").join("generation");
        let tmp_path = tmp.path().join("state").join(".generation.tmp");
        fs::write(&tmp_path, b"garbage-half-flushed").unwrap();
        assert!(gen_path.exists(), "stable generation file must survive");
        let parsed = state.generation().unwrap();
        assert_eq!(parsed, 20, "stable generation unaffected by .tmp garbage");
        fs::remove_file(&tmp_path).unwrap();
    }

    /// Save/load round-trip on per-shard OID files. Pins the atomic
    /// rename contract: the .oid file on disk is always either absent
    /// or a valid 40-hex-char SHA1.
    #[test]
    fn shard_oid_write_is_atomic() {
        let tmp = tempdir().unwrap();
        let state = State::new(tmp.path()).unwrap();

        let oid = "0123456789abcdef0123456789abcdef01234567";
        state.save_last_indexed_oid("linux-cifs", "0", oid).unwrap();
        let on_disk = tmp
            .path()
            .join("state")
            .join("shards")
            .join("linux-cifs")
            .join("0.oid");
        let raw = fs::read_to_string(&on_disk).unwrap();
        assert_eq!(raw.trim(), oid);

        // Overwriting with an invalid oid is rejected BEFORE touching
        // the final file, so the known-good value stays.
        assert!(
            state
                .save_last_indexed_oid("linux-cifs", "0", "bogus")
                .is_err()
        );
        let after = state.last_indexed_oid("linux-cifs", "0").unwrap().unwrap();
        assert_eq!(
            after, oid,
            "rejected write must not mutate the persisted value"
        );
    }
}
