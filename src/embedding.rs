//! Embedding tier — HNSW ANN index over pre-computed f32 vectors.
//!
//! This module is intentionally model-agnostic: the Python side
//! embeds text via `fastembed` (or any other backend) and hands us
//! `Vec<f32>` blobs along with their message-ids. We persist the
//! HNSW index, the per-row vectors, and a side-table mapping
//! HNSW-internal docids → message-ids.
//!
//! Why this shape:
//!   * Decouples retrieval from any specific model. Swapping in the
//!     v1.1 kernel-tuned bi-encoder is a Python-only change.
//!   * No ML runtime in Rust. Avoids candle-core / ort transitive
//!     deps (the index is tiny by comparison).
//!   * `nearest_to_mid` lookups reuse the stored vector — no need
//!     to keep the original text or recompute on the fly.
//!
//! Layout under `<data_dir>/embedding/`:
//!     meta.json       — { "model": str, "dim": u32, "metric": "cosine",
//!                         "count": u32, "schema_version": u32 }
//!     vectors.f32     — `count * dim * 4` bytes, row-major, little-endian
//!     mid.tsv         — line N = message_id for vector N
//!     hnsw.bin        — bincode-serialized instant_distance::Hnsw
//!
//! Build is single-threaded and idempotent. Each rebuild overwrites
//! the four files atomically (write to .tmp + rename dir).

#![allow(dead_code)]

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use instant_distance::{Builder, HnswMap, Point, Search};
use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const SCHEMA_VERSION: u32 = 1;

/// On-disk metadata for an embedding index. Stored as JSON next to
/// the vectors so a reader can reject mismatched dim / model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingMeta {
    pub model: String,
    pub dim: u32,
    pub metric: String,
    pub count: u32,
    pub schema_version: u32,
}

/// One f32 vector + its `instant-distance` Point impl. Cosine
/// similarity is computed via the L2 distance the library exposes;
/// callers pass already-L2-normalized vectors.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vector(pub Vec<f32>);

impl Point for Vector {
    fn distance(&self, other: &Self) -> f32 {
        // L2 squared. Equivalent to (1 - cosine_sim) * 2 when both
        // vectors are L2-normalized, so smaller = closer either way.
        debug_assert_eq!(self.0.len(), other.0.len());
        let mut acc = 0.0_f32;
        for (a, b) in self.0.iter().zip(other.0.iter()) {
            let d = a - b;
            acc += d * d;
        }
        acc
    }
}

/// Streaming builder: `add()` writes each (mid, vector) pair to
/// `<data_dir>/.embedding.tmp/{mid.tsv,vectors.f32}` immediately,
/// so memory is O(buffer size) regardless of corpus scale.
///
/// The previous implementation accumulated `Vec<(String, Vec<f32>)>`
/// in RSS — ~54 GB at 17.6M × 768 × f32.
///
/// `finalize()` mmaps vectors.f32 to build the HNSW graph. The
/// HNSW build itself still requires `N × dim × 4` bytes of owned
/// `Vec<Vector>` because `instant_distance::Builder::build` takes
/// them by value — a library limitation. For corpora where that
/// final allocation won't fit, pass `build_hnsw = false` and serve
/// nearest-neighbour via mmap-backed brute force (future work).
pub struct EmbeddingBuilder {
    model: String,
    dim: u32,
    data_dir: PathBuf,
    tmp_dir: PathBuf,
    mid_writer: BufWriter<File>,
    vec_writer: BufWriter<File>,
    count: u32,
}

impl EmbeddingBuilder {
    pub fn new(data_dir: &Path, model: impl Into<String>, dim: u32) -> Result<Self> {
        let tmp_dir = data_dir.join(".embedding.tmp");
        if tmp_dir.exists() {
            fs::remove_dir_all(&tmp_dir)?;
        }
        fs::create_dir_all(&tmp_dir)?;
        let mid_writer = BufWriter::new(File::create(tmp_dir.join("mid.tsv"))?);
        let vec_writer = BufWriter::new(File::create(tmp_dir.join("vectors.f32"))?);
        Ok(Self {
            model: model.into(),
            dim,
            data_dir: data_dir.to_owned(),
            tmp_dir,
            mid_writer,
            vec_writer,
            count: 0,
        })
    }

    pub fn len(&self) -> usize {
        self.count as usize
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Append one normalized vector. Validates dim + message_id.
    /// Writes straight to disk — no in-memory accumulation.
    pub fn add(&mut self, message_id: &str, vector: &[f32]) -> Result<()> {
        if vector.len() != self.dim as usize {
            return Err(Error::State(format!(
                "vector dim mismatch: expected {} got {}",
                self.dim,
                vector.len()
            )));
        }
        if message_id.contains('\n') {
            return Err(Error::State(format!(
                "message_id contains newline: {message_id:?}"
            )));
        }
        self.mid_writer.write_all(message_id.as_bytes())?;
        self.mid_writer.write_all(b"\n")?;
        for f in vector {
            self.vec_writer.write_all(&f.to_le_bytes())?;
        }
        self.count += 1;
        Ok(())
    }

    /// Seal the tmp build, construct HNSW, and atomically rename into
    /// `<data_dir>/embedding/`.
    pub fn finalize(self) -> Result<EmbeddingMeta> {
        self.finalize_with_hnsw(true)
    }

    pub fn finalize_with_hnsw(mut self, build_hnsw: bool) -> Result<EmbeddingMeta> {
        // Flush + drop writers so mmap sees committed bytes.
        self.mid_writer.flush()?;
        self.vec_writer.flush()?;
        let EmbeddingBuilder {
            model,
            dim,
            data_dir,
            tmp_dir,
            count,
            ..
        } = self;

        if build_hnsw && count > 0 {
            let vec_path = tmp_dir.join("vectors.f32");
            let vec_file = File::open(&vec_path)?;
            let mmap = unsafe {
                Mmap::map(&vec_file).map_err(|e| Error::State(format!("mmap vectors.f32: {e}")))?
            };
            let d = dim as usize;
            let n = count as usize;
            let expected = n * d * 4;
            if mmap.len() != expected {
                return Err(Error::State(format!(
                    "vectors.f32 size mismatch: {} bytes, expected {}",
                    mmap.len(),
                    expected
                )));
            }
            let points: Vec<Vector> = (0..n)
                .map(|i| {
                    let off = i * d * 4;
                    let mut v = Vec::with_capacity(d);
                    for j in 0..d {
                        let p = off + j * 4;
                        v.push(f32::from_le_bytes([
                            mmap[p],
                            mmap[p + 1],
                            mmap[p + 2],
                            mmap[p + 3],
                        ]));
                    }
                    Vector(v)
                })
                .collect();
            let docids: Vec<u32> = (0..count).collect();
            let map: HnswMap<Vector, u32> = Builder::default().build(points, docids);
            let bytes = bincode::serialize(&map)
                .map_err(|e| Error::State(format!("hnsw bincode serialize: {e}")))?;
            let mut w = BufWriter::new(File::create(tmp_dir.join("hnsw.bin"))?);
            w.write_all(&bytes)?;
            w.flush()?;
        }

        let meta = EmbeddingMeta {
            model,
            dim,
            metric: "cosine".to_owned(),
            count,
            schema_version: SCHEMA_VERSION,
        };
        let json = serde_json::to_vec_pretty(&meta)
            .map_err(|e| Error::State(format!("meta json: {e}")))?;
        let mut w = BufWriter::new(File::create(tmp_dir.join("meta.json"))?);
        w.write_all(&json)?;
        w.flush()?;

        let dest = data_dir.join("embedding");
        let old = dest.with_extension("old");
        if dest.exists() {
            if old.exists() {
                fs::remove_dir_all(&old)?;
            }
            fs::rename(&dest, &old)?;
        }
        fs::rename(&tmp_dir, &dest)?;
        if old.exists() {
            let _ = fs::remove_dir_all(&old);
        }
        Ok(meta)
    }
}

/// Total-order wrapper around f32 so it can go in a BinaryHeap.
/// NaN is sorted as less than every finite value (safe: cosine of
/// L2-normalized vectors in [-1, 1] never produces NaN from finite
/// inputs; guard anyway).
#[derive(Copy, Clone, PartialEq, PartialOrd)]
struct OrdF32(f32);

impl Eq for OrdF32 {}
impl Ord for OrdF32 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .partial_cmp(&other.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

/// Read-only handle on a finalized embedding index.
///
/// Vectors are mmap'd, not read into RSS. At 17.6M × 768 × 4 bytes
/// the file is ~54 GB — an `fs::read` here would anonymous-alloc
/// that and OOM on every realistic box.
pub struct EmbeddingReader {
    meta: EmbeddingMeta,
    /// Present when `hnsw.bin` exists. Absent builds (e.g.
    /// full-corpus where the in-memory HNSW construction wouldn't
    /// fit) fall back to brute-force search over the mmap'd
    /// `vectors.f32`.
    hnsw: Option<HnswMap<Vector, u32>>,
    mids: Vec<String>,
    vectors_mmap: Mmap,
}

impl EmbeddingReader {
    pub fn open(data_dir: &Path) -> Result<Option<Self>> {
        let dir = data_dir.join("embedding");
        if !dir.join("meta.json").exists() {
            return Ok(None);
        }

        let meta: EmbeddingMeta = {
            let mut s = String::new();
            File::open(dir.join("meta.json"))?.read_to_string(&mut s)?;
            serde_json::from_str(&s).map_err(|e| Error::State(format!("meta parse: {e}")))?
        };
        if meta.schema_version != SCHEMA_VERSION {
            return Err(Error::State(format!(
                "embedding schema_version mismatch: {} vs {}",
                meta.schema_version, SCHEMA_VERSION
            )));
        }

        let mids: Vec<String> = {
            let mut s = String::new();
            File::open(dir.join("mid.tsv"))?.read_to_string(&mut s)?;
            s.lines().map(str::to_owned).collect()
        };
        if mids.len() != meta.count as usize {
            return Err(Error::State(format!(
                "mid.tsv has {} lines but meta.count is {}",
                mids.len(),
                meta.count
            )));
        }

        let vectors_file = File::open(dir.join("vectors.f32"))?;
        let vectors_mmap = unsafe {
            Mmap::map(&vectors_file).map_err(|e| Error::State(format!("mmap vectors.f32: {e}")))?
        };
        let bytes_per = meta.dim as usize * 4;
        let expected = bytes_per * meta.count as usize;
        if vectors_mmap.len() != expected {
            return Err(Error::State(format!(
                "vectors.f32 size mismatch: {} bytes, expected {}",
                vectors_mmap.len(),
                expected
            )));
        }

        let hnsw_path = dir.join("hnsw.bin");
        let hnsw: Option<HnswMap<Vector, u32>> = if hnsw_path.exists() {
            let bytes = fs::read(&hnsw_path)?;
            Some(
                bincode::deserialize(&bytes)
                    .map_err(|e| Error::State(format!("hnsw deserialize: {e}")))?,
            )
        } else {
            None
        };

        Ok(Some(Self {
            meta,
            hnsw,
            mids,
            vectors_mmap,
        }))
    }

    pub fn meta(&self) -> &EmbeddingMeta {
        &self.meta
    }

    pub fn dim(&self) -> usize {
        self.meta.dim as usize
    }

    pub fn count(&self) -> usize {
        self.mids.len()
    }

    pub fn model(&self) -> &str {
        &self.meta.model
    }

    /// Copy vector N out of the mmap into a fresh `Vec<f32>`.
    fn vector_at(&self, idx: usize) -> Vec<f32> {
        let dim = self.meta.dim as usize;
        let off = idx * dim * 4;
        let slice = &self.vectors_mmap[off..off + dim * 4];
        let mut v = Vec::with_capacity(dim);
        for j in 0..dim {
            let p = j * 4;
            v.push(f32::from_le_bytes([
                slice[p],
                slice[p + 1],
                slice[p + 2],
                slice[p + 3],
            ]));
        }
        v
    }

    /// Top-`k` nearest message-ids to `query`, paired with cosine
    /// similarity in `[-1.0, 1.0]`. `query` must be the same dim and
    /// have been L2-normalized by the caller.
    ///
    /// Uses the HNSW index when present, otherwise falls back to
    /// brute-force scan over the mmap'd vectors.
    pub fn nearest(&self, query: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
        if query.len() != self.dim() {
            return Err(Error::State(format!(
                "query dim {} != index dim {}",
                query.len(),
                self.dim()
            )));
        }
        if let Some(hnsw) = self.hnsw.as_ref() {
            let mut search = Search::default();
            let q = Vector(query.to_vec());
            let mut out = Vec::new();
            for item in hnsw.search(&q, &mut search).take(k) {
                let docid = *item.value as usize;
                let Some(mid) = self.mids.get(docid) else {
                    continue;
                };
                let cos = 1.0 - item.distance / 2.0;
                out.push((mid.clone(), cos));
            }
            return Ok(out);
        }
        self.nearest_bruteforce(query, k)
    }

    /// Brute-force nearest — scans every vector in the mmap'd
    /// vectors.f32, computes cosine similarity, keeps a bounded
    /// top-`k` min-heap. O(N × dim) CPU, O(dim + k) memory.
    ///
    /// Used automatically by `nearest()` when the HNSW index is
    /// absent (e.g. builds that skipped HNSW to avoid the library's
    /// ~54 GB peak on a full-corpus bootstrap).
    pub fn nearest_bruteforce(&self, query: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;

        if query.len() != self.dim() {
            return Err(Error::State(format!(
                "query dim {} != index dim {}",
                query.len(),
                self.dim()
            )));
        }
        if k == 0 || self.mids.is_empty() {
            return Ok(Vec::new());
        }

        let dim = self.dim();
        let row_bytes = dim * 4;
        // Min-heap keyed by cosine (higher = better); keep only top-k.
        // Store as Reverse<(OrderedF32, usize)> so pop() evicts the worst.
        let mut heap: BinaryHeap<Reverse<(OrdF32, u32)>> = BinaryHeap::with_capacity(k + 1);

        for i in 0..self.mids.len() {
            let off = i * row_bytes;
            let slice = &self.vectors_mmap[off..off + row_bytes];
            let mut dot = 0.0_f32;
            for j in 0..dim {
                let p = j * 4;
                let v = f32::from_le_bytes([slice[p], slice[p + 1], slice[p + 2], slice[p + 3]]);
                dot += v * query[j];
            }
            let cos = OrdF32(dot);
            if heap.len() < k {
                heap.push(Reverse((cos, i as u32)));
            } else if let Some(Reverse((min_cos, _))) = heap.peek() {
                if cos > *min_cos {
                    heap.pop();
                    heap.push(Reverse((cos, i as u32)));
                }
            }
        }

        let mut sorted: Vec<(OrdF32, u32)> = heap.into_iter().map(|Reverse(p)| p).collect();
        sorted.sort_by(|a, b| b.0.cmp(&a.0));
        let out = sorted
            .into_iter()
            .filter_map(|(cos, idx)| self.mids.get(idx as usize).map(|m| (m.clone(), cos.0)))
            .collect();
        Ok(out)
    }

    /// `nearest` seeded by an existing message-id (we look up its
    /// stored vector). Returns an extra hit for the seed itself
    /// unless the caller filters.
    pub fn nearest_to_mid(&self, mid: &str, k: usize) -> Result<Vec<(String, f32)>> {
        let Some(idx) = self.mids.iter().position(|m| m == mid) else {
            return Ok(Vec::new());
        };
        let v = self.vector_at(idx);
        self.nearest(&v, k)
    }
}

pub fn embedding_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("embedding")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn norm(mut v: Vec<f32>) -> Vec<f32> {
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        for x in &mut v {
            *x /= n;
        }
        v
    }

    #[test]
    fn build_and_query_roundtrip() {
        let tmp = tempdir().unwrap();
        let mut b = EmbeddingBuilder::new(tmp.path(), "test/dim-3", 3).unwrap();
        b.add("<m1@x>", &norm(vec![1.0, 0.0, 0.0])).unwrap();
        b.add("<m2@x>", &norm(vec![0.0, 1.0, 0.0])).unwrap();
        b.add("<m3@x>", &norm(vec![0.7, 0.7, 0.0])).unwrap();
        b.add("<m4@x>", &norm(vec![0.0, 0.0, 1.0])).unwrap();
        let meta = b.finalize().unwrap();
        assert_eq!(meta.count, 4);
        assert_eq!(meta.dim, 3);

        let r = EmbeddingReader::open(tmp.path()).unwrap().unwrap();
        // Query close to m1 should rank m1 first, then m3, then m2.
        let hits = r.nearest(&norm(vec![1.0, 0.05, 0.0]), 4).unwrap();
        assert_eq!(hits[0].0, "<m1@x>");
        let order: Vec<&str> = hits.iter().map(|(m, _)| m.as_str()).collect();
        let m3_pos = order.iter().position(|m| *m == "<m3@x>").unwrap();
        let m2_pos = order.iter().position(|m| *m == "<m2@x>").unwrap();
        let m4_pos = order.iter().position(|m| *m == "<m4@x>").unwrap();
        assert!(m3_pos < m2_pos);
        assert!(m2_pos < m4_pos);
        // Cosine similarity to itself is ~ 1.
        assert!(hits[0].1 > 0.99);
    }

    #[test]
    fn nearest_to_mid_uses_stored_vector() {
        let tmp = tempdir().unwrap();
        let mut b = EmbeddingBuilder::new(tmp.path(), "m", 2).unwrap();
        b.add("a", &norm(vec![1.0, 0.0])).unwrap();
        b.add("b", &norm(vec![0.99, 0.1])).unwrap();
        b.add("c", &norm(vec![-1.0, 0.0])).unwrap();
        b.finalize().unwrap();
        let r = EmbeddingReader::open(tmp.path()).unwrap().unwrap();
        let hits = r.nearest_to_mid("a", 3).unwrap();
        // First hit is the seed itself (sim ~ 1); next is "b".
        assert_eq!(hits[0].0, "a");
        assert_eq!(hits[1].0, "b");
        assert_eq!(hits[2].0, "c");
        // c is opposite, similarity should be near -1.
        assert!(hits[2].1 < 0.0);
    }

    #[test]
    fn dim_mismatch_rejected() {
        let tmp = tempdir().unwrap();
        let mut b = EmbeddingBuilder::new(tmp.path(), "m", 4).unwrap();
        assert!(b.add("a", &[1.0, 0.0, 0.0]).is_err());
    }

    #[test]
    fn open_returns_none_when_absent() {
        let tmp = tempdir().unwrap();
        assert!(EmbeddingReader::open(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn bruteforce_and_hnsw_agree_on_small_corpus() {
        let tmp = tempdir().unwrap();
        let mut b = EmbeddingBuilder::new(tmp.path(), "m", 3).unwrap();
        b.add("<m1@x>", &norm(vec![1.0, 0.0, 0.0])).unwrap();
        b.add("<m2@x>", &norm(vec![0.0, 1.0, 0.0])).unwrap();
        b.add("<m3@x>", &norm(vec![0.7, 0.7, 0.0])).unwrap();
        b.add("<m4@x>", &norm(vec![0.0, 0.0, 1.0])).unwrap();
        b.finalize().unwrap();
        let r = EmbeddingReader::open(tmp.path()).unwrap().unwrap();
        let q = norm(vec![1.0, 0.05, 0.0]);
        let hnsw_hits = r.nearest(&q, 4).unwrap();
        let bf_hits = r.nearest_bruteforce(&q, 4).unwrap();
        let hnsw_mids: Vec<&str> = hnsw_hits.iter().map(|(m, _)| m.as_str()).collect();
        let bf_mids: Vec<&str> = bf_hits.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(hnsw_mids, bf_mids);
    }

    #[test]
    fn reader_works_without_hnsw_bin() {
        let tmp = tempdir().unwrap();
        let mut b = EmbeddingBuilder::new(tmp.path(), "m", 3).unwrap();
        b.add("<m1@x>", &norm(vec![1.0, 0.0, 0.0])).unwrap();
        b.add("<m2@x>", &norm(vec![0.0, 1.0, 0.0])).unwrap();
        b.add("<m3@x>", &norm(vec![0.7, 0.7, 0.0])).unwrap();
        b.finalize_with_hnsw(false).unwrap();
        assert!(!tmp.path().join("embedding").join("hnsw.bin").exists());
        let r = EmbeddingReader::open(tmp.path()).unwrap().unwrap();
        let hits = r.nearest(&norm(vec![1.0, 0.05, 0.0]), 3).unwrap();
        assert_eq!(hits[0].0, "<m1@x>");
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn nearest_to_unknown_mid_returns_empty() {
        let tmp = tempdir().unwrap();
        let mut b = EmbeddingBuilder::new(tmp.path(), "m", 2).unwrap();
        b.add("a", &norm(vec![1.0, 0.0])).unwrap();
        b.finalize().unwrap();
        let r = EmbeddingReader::open(tmp.path()).unwrap().unwrap();
        assert!(r.nearest_to_mid("does-not-exist", 5).unwrap().is_empty());
    }
}
