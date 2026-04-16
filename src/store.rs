//! Compressed raw message store — source of truth.

// list_root() accessor lands on the reader in a later phase.
#![allow(dead_code)]
//!
//! Append-only segment files, one zstd frame per message. Metadata
//! tier carries `(list, shard, segment_id, offset, length)` crosswalk;
//! given those, we can random-access-decompress any message.
//!
//! Layout under `<data_dir>/store/<list>/`:
//!     dict.zstd               (v2; unused in v1 — per-message zstd only)
//!     segment-NNNNNN.zst      append-only, multiple zstd frames
//!
//! v1 decisions we intentionally defer:
//!   * per-list zstd-dict training (Phase 1.5)
//!   * segment compaction (Phase 2)
//!   * per-message sha256 stored externally only (in metadata tier),
//!     not inline — the store is opaque bytes indexed by offset.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Roll to a new segment when the active one exceeds this size on disk.
/// 1 GB matches public-inbox shards roughly; keeps random access fast.
const SEGMENT_ROLL_BYTES: u64 = 1 << 30;

/// Per-message zstd level. 19 is heavy but bodies compress well and
/// we decompress at query-confirm time, where throughput matters more
/// than compress-time CPU.
const ZSTD_LEVEL: i32 = 6;

/// Handle on one list's compressed store. Append-only writer + random
/// reader share the same segment files but via distinct fd paths.
pub struct Store {
    list_root: PathBuf,
    writer: Mutex<SegmentWriter>,
}

/// Logical pointer into the store: `(segment_id, byte_offset, zstd_frame_length)`.
#[derive(Debug, Clone, Copy)]
pub struct StoreOffset {
    pub segment_id: u32,
    pub offset: u64,
    pub length: u64,
}

/// Result of an append: the logical pointer + sha256 of the *uncompressed*
/// payload. sha256 is what the metadata tier records.
#[derive(Debug, Clone)]
pub struct Appended {
    pub ptr: StoreOffset,
    pub body_sha256: [u8; 32],
    pub body_length: u64,
}

impl Store {
    pub fn open(data_dir: impl AsRef<Path>, list: &str) -> Result<Self> {
        if list.is_empty() || list.contains('/') || list.contains("..") {
            return Err(Error::State(format!("invalid list name {list:?}")));
        }
        let list_root = data_dir.as_ref().join("store").join(list);
        fs::create_dir_all(&list_root)?;
        let writer = SegmentWriter::open(&list_root)?;
        Ok(Self {
            list_root,
            writer: Mutex::new(writer),
        })
    }

    pub fn list_root(&self) -> &Path {
        &self.list_root
    }

    /// Compress + append a single message body. Returns where it lives
    /// plus the sha256 of the raw (pre-compression) bytes.
    pub fn append(&self, body: &[u8]) -> Result<Appended> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| Error::State("store writer mutex poisoned".to_owned()))?;
        w.append(body)
    }

    /// Random-access read a previously-appended message.
    ///
    /// The `length` on the `StoreOffset` is the compressed frame size
    /// (what `append` returned). It's used when the caller knows it;
    /// prefer `read_at(segment_id, offset)` when the caller only
    /// carries the offset (e.g., the metadata tier, which records the
    /// uncompressed length for display and the compressed frame
    /// self-delimits via zstd's stream format).
    pub fn read(&self, ptr: StoreOffset) -> Result<Vec<u8>> {
        if ptr.length == 0 {
            return self.read_at(ptr.segment_id, ptr.offset);
        }
        let seg_path = segment_path(&self.list_root, ptr.segment_id);
        let mut f = File::open(&seg_path)?;
        f.seek(SeekFrom::Start(ptr.offset))?;
        let mut framed = vec![0u8; ptr.length as usize];
        f.read_exact(&mut framed)?;
        Ok(zstd::decode_all(io::Cursor::new(framed))?)
    }

    /// Read one message given only its segment + offset. zstd frames
    /// are self-delimiting; `Decoder::single_frame()` tells the
    /// streaming decoder to stop after the first frame instead of
    /// concatenating every subsequent message in the segment.
    pub fn read_at(&self, segment_id: u32, offset: u64) -> Result<Vec<u8>> {
        let seg_path = segment_path(&self.list_root, segment_id);
        let mut f = File::open(&seg_path)?;
        f.seek(SeekFrom::Start(offset))?;
        let mut out = Vec::new();
        let mut decoder = zstd::Decoder::new(f)?.single_frame();
        decoder.read_to_end(&mut out)?;
        Ok(out)
    }

    /// fsync the active segment. Call after a batch of appends before
    /// committing the metadata rows that reference them; otherwise a
    /// crash leaves dangling offsets.
    pub fn flush(&self) -> Result<()> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| Error::State("store writer mutex poisoned".to_owned()))?;
        w.flush()
    }
}

struct SegmentWriter {
    list_root: PathBuf,
    segment_id: u32,
    file: File,
    position: u64,
}

impl SegmentWriter {
    fn open(list_root: &Path) -> Result<Self> {
        let (segment_id, file, position) = open_active_segment(list_root)?;
        Ok(Self {
            list_root: list_root.to_owned(),
            segment_id,
            file,
            position,
        })
    }

    fn append(&mut self, body: &[u8]) -> Result<Appended> {
        if self.position >= SEGMENT_ROLL_BYTES {
            self.roll()?;
        }
        let mut hasher = Sha256::new();
        hasher.update(body);
        let sha: [u8; 32] = hasher.finalize().into();

        let compressed = zstd::encode_all(io::Cursor::new(body), ZSTD_LEVEL)?;

        let offset = self.position;
        self.file.write_all(&compressed)?;
        let length = compressed.len() as u64;
        self.position += length;

        Ok(Appended {
            ptr: StoreOffset {
                segment_id: self.segment_id,
                offset,
                length,
            },
            body_sha256: sha,
            body_length: body.len() as u64,
        })
    }

    fn flush(&mut self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }

    fn roll(&mut self) -> Result<()> {
        self.file.sync_all()?;
        self.segment_id = self
            .segment_id
            .checked_add(1)
            .ok_or_else(|| Error::State("segment id overflow".to_owned()))?;
        let path = segment_path(&self.list_root, self.segment_id);
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .read(true)
            .open(&path)?;
        self.file = file;
        self.position = 0;
        Ok(())
    }
}

fn segment_path(list_root: &Path, segment_id: u32) -> PathBuf {
    list_root.join(format!("segment-{segment_id:06}.zst"))
}

fn open_active_segment(list_root: &Path) -> Result<(u32, File, u64)> {
    // Scan for existing segments; pick the highest id. If none, create
    // segment-000000.zst.
    let mut highest: Option<u32> = None;
    for entry in fs::read_dir(list_root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(id) = parse_segment_id(name) {
            highest = Some(highest.map_or(id, |h| h.max(id)));
        }
    }
    let id = highest.unwrap_or(0);
    let path = segment_path(list_root, id);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&path)?;
    let position = file.metadata()?.len();
    Ok((id, file, position))
}

fn parse_segment_id(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("segment-")?;
    let num = rest.strip_suffix(".zst")?;
    num.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn append_then_read_roundtrip() {
        let tmp = tempdir().unwrap();
        let store = Store::open(tmp.path(), "linux-cifs").unwrap();

        let msg = b"From: alice@example.com\nSubject: hi\n\nbody here";
        let a = store.append(msg).unwrap();
        store.flush().unwrap();
        assert_eq!(a.body_length as usize, msg.len());

        let got = store.read(a.ptr).unwrap();
        assert_eq!(got, msg);
    }

    #[test]
    fn many_messages_recover_independently() {
        let tmp = tempdir().unwrap();
        let store = Store::open(tmp.path(), "lkml").unwrap();

        let msgs: Vec<Vec<u8>> = (0..64)
            .map(|i| format!("msg-{i:03}: lorem ipsum dolor sit amet").into_bytes())
            .collect();
        let ptrs: Vec<_> = msgs.iter().map(|m| store.append(m).unwrap().ptr).collect();
        store.flush().unwrap();

        for (m, ptr) in msgs.iter().zip(ptrs) {
            assert_eq!(store.read(ptr).unwrap(), *m);
        }
    }

    #[test]
    fn rejects_bad_list_name() {
        let tmp = tempdir().unwrap();
        assert!(Store::open(tmp.path(), "../etc").is_err());
        assert!(Store::open(tmp.path(), "a/b").is_err());
        assert!(Store::open(tmp.path(), "").is_err());
    }

    #[test]
    fn reopening_resumes_in_active_segment() {
        let tmp = tempdir().unwrap();
        let first = {
            let store = Store::open(tmp.path(), "l").unwrap();
            let a = store.append(b"first message").unwrap();
            store.flush().unwrap();
            a.ptr
        };
        let store = Store::open(tmp.path(), "l").unwrap();
        let second = store.append(b"second message").unwrap().ptr;
        store.flush().unwrap();

        assert_eq!(first.segment_id, second.segment_id);
        assert!(second.offset > first.offset);

        assert_eq!(store.read(first).unwrap(), b"first message");
        assert_eq!(store.read(second).unwrap(), b"second message");
    }
}
