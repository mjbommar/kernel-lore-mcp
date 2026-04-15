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

pub struct EmbeddingBuilder {
    model: String,
    dim: u32,
    items: Vec<(String, Vec<f32>)>,
}

impl EmbeddingBuilder {
    pub fn new(model: impl Into<String>, dim: u32) -> Self {
        Self {
            model: model.into(),
            dim,
            items: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Append one normalized vector. Validates dim.
    pub fn add(&mut self, message_id: &str, vector: Vec<f32>) -> Result<()> {
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
        self.items.push((message_id.to_owned(), vector));
        Ok(())
    }

    /// Build the HNSW index and write all four files atomically.
    /// `data_dir` is the project data root; we own `<data_dir>/embedding/`.
    pub fn finalize(self, data_dir: &Path) -> Result<EmbeddingMeta> {
        let count = self.items.len();
        let dim = self.dim;
        let model = self.model.clone();

        let dest = data_dir.join("embedding");
        let tmp = data_dir.join(".embedding.tmp");
        if tmp.exists() {
            fs::remove_dir_all(&tmp)?;
        }
        fs::create_dir_all(&tmp)?;

        // 1. mid.tsv
        {
            let mut w = BufWriter::new(File::create(tmp.join("mid.tsv"))?);
            for (mid, _) in &self.items {
                w.write_all(mid.as_bytes())?;
                w.write_all(b"\n")?;
            }
            w.flush()?;
        }

        // 2. vectors.f32 (row-major, little-endian)
        {
            let mut w = BufWriter::new(File::create(tmp.join("vectors.f32"))?);
            for (_, v) in &self.items {
                for f in v {
                    w.write_all(&f.to_le_bytes())?;
                }
            }
            w.flush()?;
        }

        // 3. hnsw.bin — HnswMap<Vector, u32> where value = docid into
        // mids[] / vectors.f32. We carry the docid so search results
        // map back to the right message-id without a parallel array.
        {
            let points: Vec<Vector> = self.items.iter().map(|(_, v)| Vector(v.clone())).collect();
            let docids: Vec<u32> = (0..points.len() as u32).collect();
            let map: HnswMap<Vector, u32> = Builder::default().build(points, docids);
            let bytes = bincode::serialize(&map)
                .map_err(|e| Error::State(format!("hnsw bincode serialize: {e}")))?;
            let mut w = BufWriter::new(File::create(tmp.join("hnsw.bin"))?);
            w.write_all(&bytes)?;
            w.flush()?;
        }

        // 4. meta.json (last — so a half-finished build is ignored).
        let meta = EmbeddingMeta {
            model,
            dim,
            metric: "cosine".to_owned(),
            count: count as u32,
            schema_version: SCHEMA_VERSION,
        };
        let json = serde_json::to_vec_pretty(&meta)
            .map_err(|e| Error::State(format!("meta json: {e}")))?;
        let mut w = BufWriter::new(File::create(tmp.join("meta.json"))?);
        w.write_all(&json)?;
        w.flush()?;

        // Atomic swap.
        if dest.exists() {
            fs::remove_dir_all(&dest)?;
        }
        fs::rename(&tmp, &dest)?;
        Ok(meta)
    }
}

/// Read-only handle on a finalized embedding index.
pub struct EmbeddingReader {
    meta: EmbeddingMeta,
    hnsw: HnswMap<Vector, u32>,
    mids: Vec<String>,
    vectors: Vec<Vec<f32>>,
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

        let vectors: Vec<Vec<f32>> = {
            let bytes = fs::read(dir.join("vectors.f32"))?;
            let dim = meta.dim as usize;
            let bytes_per = dim * 4;
            if bytes.len() != bytes_per * meta.count as usize {
                return Err(Error::State(format!(
                    "vectors.f32 size mismatch: {} bytes, expected {}",
                    bytes.len(),
                    bytes_per * meta.count as usize
                )));
            }
            (0..meta.count as usize)
                .map(|i| {
                    let off = i * bytes_per;
                    (0..dim)
                        .map(|j| {
                            let p = off + j * 4;
                            f32::from_le_bytes([bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]])
                        })
                        .collect()
                })
                .collect()
        };

        let hnsw_bytes = fs::read(dir.join("hnsw.bin"))?;
        let hnsw: HnswMap<Vector, u32> = bincode::deserialize(&hnsw_bytes)
            .map_err(|e| Error::State(format!("hnsw deserialize: {e}")))?;

        Ok(Some(Self {
            meta,
            hnsw,
            mids,
            vectors,
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

    /// Top-`k` nearest message-ids to `query`, paired with cosine
    /// similarity in `[-1.0, 1.0]`. `query` must be the same dim and
    /// have been L2-normalized by the caller.
    pub fn nearest(&self, query: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
        if query.len() != self.dim() {
            return Err(Error::State(format!(
                "query dim {} != index dim {}",
                query.len(),
                self.dim()
            )));
        }
        let mut search = Search::default();
        let q = Vector(query.to_vec());
        let mut out = Vec::new();
        for item in self.hnsw.search(&q, &mut search).take(k) {
            let docid = *item.value as usize;
            let Some(mid) = self.mids.get(docid) else {
                continue;
            };
            // distance is L2-squared on normalized vectors → cosine
            // similarity recovered as 1 - d/2.
            let cos = 1.0 - item.distance / 2.0;
            out.push((mid.clone(), cos));
        }
        Ok(out)
    }

    /// `nearest` seeded by an existing message-id (we look up its
    /// stored vector). Returns an extra hit for the seed itself
    /// unless the caller filters.
    pub fn nearest_to_mid(&self, mid: &str, k: usize) -> Result<Vec<(String, f32)>> {
        let Some(idx) = self.mids.iter().position(|m| m == mid) else {
            return Ok(Vec::new());
        };
        let v = self.vectors[idx].clone();
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
        let mut b = EmbeddingBuilder::new("test/dim-3", 3);
        b.add("<m1@x>", norm(vec![1.0, 0.0, 0.0])).unwrap();
        b.add("<m2@x>", norm(vec![0.0, 1.0, 0.0])).unwrap();
        b.add("<m3@x>", norm(vec![0.7, 0.7, 0.0])).unwrap();
        b.add("<m4@x>", norm(vec![0.0, 0.0, 1.0])).unwrap();
        let meta = b.finalize(tmp.path()).unwrap();
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
        let mut b = EmbeddingBuilder::new("m", 2);
        b.add("a", norm(vec![1.0, 0.0])).unwrap();
        b.add("b", norm(vec![0.99, 0.1])).unwrap();
        b.add("c", norm(vec![-1.0, 0.0])).unwrap();
        b.finalize(tmp.path()).unwrap();
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
        let mut b = EmbeddingBuilder::new("m", 4);
        assert!(b.add("a", vec![1.0, 0.0, 0.0]).is_err());
    }

    #[test]
    fn open_returns_none_when_absent() {
        let tmp = tempdir().unwrap();
        assert!(EmbeddingReader::open(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn nearest_to_unknown_mid_returns_empty() {
        let tmp = tempdir().unwrap();
        let mut b = EmbeddingBuilder::new("m", 2);
        b.add("a", norm(vec![1.0, 0.0])).unwrap();
        b.finalize(tmp.path()).unwrap();
        let r = EmbeddingReader::open(tmp.path()).unwrap().unwrap();
        assert!(r.nearest_to_mid("does-not-exist", 5).unwrap().is_empty());
    }
}
