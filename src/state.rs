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
}
