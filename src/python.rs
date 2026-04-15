//! PyO3 surface — thin wrappers around the pure-Rust core.
//!
//! Discipline:
//!   * Every heavy call releases the GIL via `Python::detach`.
//!   * No `anyhow::Error` crosses the boundary; `crate::Error`'s
//!     `impl From<Error> for PyErr` does the mapping.
//!   * Return shapes are plain Python dicts / lists of dicts so the
//!     Python layer can map them into pydantic models without
//!     duplicating schema in the Rust side.
//!
//! The Python package side (`src/kernel_lore_mcp/_core.pyi`) declares
//! these. Keep the two in lockstep.
//!
//! This module is `pub` so `#[pymodule]` in `lib.rs` can pick up the
//! free functions and classes.

use std::path::PathBuf;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::embedding::{EmbeddingBuilder, EmbeddingReader};
use crate::ingest;
use crate::reader::{DiffMode, EqField, MessageRow, Reader as CoreReader, RegexField};
use crate::router;
use crate::tid;

/// Ingest one public-inbox shard. Releases the GIL for the whole walk.
///
/// Returns a dict:
///   {
///     "ingested": u64,
///     "skipped_no_m": u64,
///     "skipped_empty": u64,
///     "skipped_no_mid": u64,
///     "parquet_path": Optional[str],
///   }
#[pyfunction]
#[pyo3(name = "ingest_shard")]
#[pyo3(signature = (data_dir, shard_path, list, shard, run_id))]
pub fn py_ingest_shard<'py>(
    py: Python<'py>,
    data_dir: PathBuf,
    shard_path: PathBuf,
    list: String,
    shard: String,
    run_id: String,
) -> PyResult<Bound<'py, PyDict>> {
    let stats =
        py.detach(|| ingest::ingest_shard(&data_dir, &shard_path, &list, &shard, &run_id))?;
    let d = PyDict::new(py);
    d.set_item("ingested", stats.ingested)?;
    d.set_item("skipped_no_m", stats.skipped_no_m)?;
    d.set_item("skipped_empty", stats.skipped_empty)?;
    d.set_item("skipped_no_mid", stats.skipped_no_mid)?;
    d.set_item("parquet_path", stats.parquet_path)?;
    Ok(d)
}

/// Rebuild the tid side-table from the metadata tier. Returns the
/// dest path + row count.
#[pyfunction]
#[pyo3(name = "rebuild_tid")]
pub fn py_rebuild_tid<'py>(py: Python<'py>, data_dir: PathBuf) -> PyResult<Bound<'py, PyDict>> {
    let (path, n) = py.detach(|| tid::rebuild(&data_dir))?;
    let d = PyDict::new(py);
    d.set_item("path", path.display().to_string())?;
    d.set_item("rows", n)?;
    Ok(d)
}

/// Build (or rebuild) the embedding index. Caller passes parallel
/// lists of message-ids and L2-normalized f32 vectors (one row each).
/// The Python side runs the actual embedding model (fastembed) and
/// hands the resulting `numpy.ndarray.astype(np.float32)` here.
///
/// Idempotent — overwrites `<data_dir>/embedding/` atomically.
#[pyfunction]
#[pyo3(name = "build_embedding_index")]
#[pyo3(signature = (data_dir, model, dim, message_ids, vectors))]
pub fn py_build_embedding_index<'py>(
    py: Python<'py>,
    data_dir: PathBuf,
    model: String,
    dim: u32,
    message_ids: Vec<String>,
    vectors: Vec<Vec<f32>>,
) -> PyResult<Bound<'py, PyDict>> {
    if message_ids.len() != vectors.len() {
        return Err(crate::error::Error::State(format!(
            "build_embedding_index: {} message-ids vs {} vectors",
            message_ids.len(),
            vectors.len()
        ))
        .into());
    }
    let meta = py.detach(move || -> Result<_, crate::error::Error> {
        let mut b = EmbeddingBuilder::new(model, dim);
        for (mid, v) in message_ids.into_iter().zip(vectors.into_iter()) {
            b.add(&mid, v)?;
        }
        b.finalize(&data_dir)
    })?;
    let d = PyDict::new(py);
    d.set_item("model", &meta.model)?;
    d.set_item("dim", meta.dim)?;
    d.set_item("metric", &meta.metric)?;
    d.set_item("count", meta.count)?;
    d.set_item("schema_version", meta.schema_version)?;
    Ok(d)
}

/// Read the embedding index metadata. Returns `None` if no index
/// has been built yet.
#[pyfunction]
#[pyo3(name = "embedding_meta")]
pub fn py_embedding_meta<'py>(
    py: Python<'py>,
    data_dir: PathBuf,
) -> PyResult<Option<Bound<'py, PyDict>>> {
    let reader = py.detach(|| EmbeddingReader::open(&data_dir))?;
    let Some(reader) = reader else {
        return Ok(None);
    };
    let m = reader.meta();
    let d = PyDict::new(py);
    d.set_item("model", &m.model)?;
    d.set_item("dim", m.dim)?;
    d.set_item("metric", &m.metric)?;
    d.set_item("count", m.count)?;
    d.set_item("schema_version", m.schema_version)?;
    Ok(Some(d))
}

/// Handle on a `<data_dir>` that exposes all v0.5 reader methods.
#[pyclass(name = "Reader", module = "kernel_lore_mcp._core")]
pub struct PyReader {
    inner: CoreReader,
}

#[pymethods]
impl PyReader {
    #[new]
    fn new(data_dir: PathBuf) -> Self {
        Self {
            inner: CoreReader::new(data_dir),
        }
    }

    fn fetch_message<'py>(
        &self,
        py: Python<'py>,
        message_id: String,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        let row = py.detach(|| self.inner.fetch_message(&message_id))?;
        row.map(|r| row_to_pydict(py, &r)).transpose()
    }

    #[pyo3(signature = (file=None, function=None, since_unix_ns=None, list=None, limit=100))]
    fn activity<'py>(
        &self,
        py: Python<'py>,
        file: Option<String>,
        function: Option<String>,
        since_unix_ns: Option<i64>,
        list: Option<String>,
        limit: usize,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let rows = py.detach(|| {
            self.inner.activity(
                file.as_deref(),
                function.as_deref(),
                since_unix_ns,
                list.as_deref(),
                limit,
            )
        })?;
        rows.iter().map(|r| row_to_pydict(py, r)).collect()
    }

    fn series_timeline<'py>(
        &self,
        py: Python<'py>,
        message_id: String,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let rows = py.detach(|| self.inner.series_timeline(&message_id))?;
        rows.iter().map(|r| row_to_pydict(py, r)).collect()
    }

    #[pyo3(signature = (token, limit=25))]
    fn expand_citation<'py>(
        &self,
        py: Python<'py>,
        token: String,
        limit: usize,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let rows = py.detach(|| self.inner.expand_citation(&token, limit))?;
        rows.iter().map(|r| row_to_pydict(py, r)).collect()
    }

    /// Run the full query router (lei-compatible subset) and return
    /// fused results across metadata, trigram, and BM25 tiers via
    /// reciprocal rank fusion. Each row carries `_score` (fused),
    /// `_tier_provenance` (list of tier names), and
    /// `_is_exact_match` (bool).
    #[pyo3(signature = (query, limit=25))]
    fn router_search<'py>(
        &self,
        py: Python<'py>,
        query: String,
        limit: usize,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let hits = py.detach(|| -> Result<Vec<router::RankedHit>, crate::error::Error> {
            let parsed = router::parse_query(&query)?;
            router::dispatch(&self.inner, &parsed, limit)
        })?;
        hits.iter()
            .map(|h| {
                let d = row_to_pydict(py, &h.row)?;
                d.set_item("_score", h.fused_score)?;
                d.set_item("_tier_provenance", &h.tier_provenance)?;
                d.set_item("_is_exact_match", h.is_exact_match)?;
                Ok(d)
            })
            .collect()
    }

    /// Walk the reply graph from `message_id` and return every
    /// message in the same conversation ordered by date.
    #[pyo3(signature = (message_id, max_messages=200))]
    fn thread<'py>(
        &self,
        py: Python<'py>,
        message_id: String,
        max_messages: usize,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let rows = py.detach(|| self.inner.thread(&message_id, max_messages))?;
        rows.iter().map(|r| row_to_pydict(py, r)).collect()
    }

    /// Free-text BM25 search over prose bodies + subjects. Returns
    /// `[{..., "_score": f32}, ...]` (score attached inside the row
    /// dict under the `_score` key).
    #[pyo3(signature = (query, limit=25))]
    fn prose_search<'py>(
        &self,
        py: Python<'py>,
        query: String,
        limit: usize,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let scored = py.detach(|| self.inner.prose_search(&query, limit))?;
        scored
            .iter()
            .map(|(row, score)| {
                let d = row_to_pydict(py, row)?;
                d.set_item("_score", *score)?;
                Ok(d)
            })
            .collect()
    }

    /// Substring search over patch bodies via the trigram tier.
    ///
    /// Returns a list of row dicts (same shape as `fetch_message`).
    /// `limit` is enforced after confirmation; newest-first.
    #[pyo3(signature = (needle, list=None, limit=100))]
    fn patch_search<'py>(
        &self,
        py: Python<'py>,
        needle: String,
        list: Option<String>,
        limit: usize,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let rows = py.detach(|| self.inner.patch_search(&needle, list.as_deref(), limit))?;
        rows.iter().map(|r| row_to_pydict(py, r)).collect()
    }

    // ---- low-level retrieval primitives (Phase 7) ----------------

    /// Exact-equality scan on a structured column.
    /// `field` ∈ {message_id, list, from_addr, in_reply_to, tid,
    /// commit_oid, body_sha256, subject_normalized,
    /// touched_files, touched_functions, references, subject_tags,
    /// signed_off_by, reviewed_by, acked_by, tested_by,
    /// co_developed_by, reported_by, fixes, link, closes, cc_stable}.
    #[pyo3(signature = (field, value, since_unix_ns=None, list=None, limit=100))]
    fn eq<'py>(
        &self,
        py: Python<'py>,
        field: String,
        value: String,
        since_unix_ns: Option<i64>,
        list: Option<String>,
        limit: usize,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let f = EqField::from_name(&field)
            .ok_or_else(|| crate::error::Error::QueryParse(format!("unknown field {field:?}")))?;
        let rows = py.detach(|| {
            self.inner
                .eq(f, &value, since_unix_ns, list.as_deref(), limit)
        })?;
        rows.iter().map(|r| row_to_pydict(py, r)).collect()
    }

    /// `WHERE field IN (values)`. Same field set as `eq`.
    #[pyo3(signature = (field, values, since_unix_ns=None, list=None, limit=100))]
    fn in_list<'py>(
        &self,
        py: Python<'py>,
        field: String,
        values: Vec<String>,
        since_unix_ns: Option<i64>,
        list: Option<String>,
        limit: usize,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let f = EqField::from_name(&field)
            .ok_or_else(|| crate::error::Error::QueryParse(format!("unknown field {field:?}")))?;
        let rows = py.detach(|| {
            self.inner
                .in_list(f, &values, since_unix_ns, list.as_deref(), limit)
        })?;
        rows.iter().map(|r| row_to_pydict(py, r)).collect()
    }

    /// Aggregate counts over the same predicate as `eq`.
    /// Returns {"count", "distinct_authors", "earliest_unix_ns",
    /// "latest_unix_ns"}.
    #[pyo3(signature = (field, value, since_unix_ns=None, list=None))]
    fn count<'py>(
        &self,
        py: Python<'py>,
        field: String,
        value: String,
        since_unix_ns: Option<i64>,
        list: Option<String>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let f = EqField::from_name(&field)
            .ok_or_else(|| crate::error::Error::QueryParse(format!("unknown field {field:?}")))?;
        let summary = py.detach(|| self.inner.count(f, &value, since_unix_ns, list.as_deref()))?;
        let d = PyDict::new(py);
        d.set_item("count", summary.count)?;
        d.set_item("distinct_authors", summary.distinct_authors)?;
        d.set_item("earliest_unix_ns", summary.earliest_unix_ns)?;
        d.set_item("latest_unix_ns", summary.latest_unix_ns)?;
        Ok(d)
    }

    /// Case-insensitive byte substring scan over `subject_raw`.
    #[pyo3(signature = (needle, list=None, since_unix_ns=None, limit=100))]
    fn substr_subject<'py>(
        &self,
        py: Python<'py>,
        needle: String,
        list: Option<String>,
        since_unix_ns: Option<i64>,
        limit: usize,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let rows = py.detach(|| {
            self.inner
                .substr_subject(&needle, list.as_deref(), since_unix_ns, limit)
        })?;
        rows.iter().map(|r| row_to_pydict(py, r)).collect()
    }

    /// Substring scan inside one named trailer column. `name` ∈
    /// {fixes, link, closes, cc-stable, signed-off-by, reviewed-by,
    /// acked-by, tested-by, co-developed-by, reported-by}.
    #[pyo3(signature = (name, value_substring, list=None, since_unix_ns=None, limit=100))]
    fn substr_trailers<'py>(
        &self,
        py: Python<'py>,
        name: String,
        value_substring: String,
        list: Option<String>,
        since_unix_ns: Option<i64>,
        limit: usize,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let rows = py.detach(|| {
            self.inner.substr_trailers(
                &name,
                &value_substring,
                list.as_deref(),
                since_unix_ns,
                limit,
            )
        })?;
        rows.iter().map(|r| row_to_pydict(py, r)).collect()
    }

    /// DFA-only regex over one of {subject, from_addr, body_prose,
    /// patch}. Patterns with backrefs / lookaround are rejected.
    /// `anchor_required=True` rejects leading `.*` patterns.
    #[pyo3(signature = (field, pattern, anchor_required=true, list=None, since_unix_ns=None, limit=100))]
    #[allow(clippy::too_many_arguments)]
    fn regex<'py>(
        &self,
        py: Python<'py>,
        field: String,
        pattern: String,
        anchor_required: bool,
        list: Option<String>,
        since_unix_ns: Option<i64>,
        limit: usize,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let f = RegexField::from_name(&field).ok_or_else(|| {
            crate::error::Error::QueryParse(format!(
                "unknown regex field {field:?}; supported: subject, from_addr, body_prose, patch"
            ))
        })?;
        let rows = py.detach(|| {
            self.inner.regex(
                f,
                &pattern,
                anchor_required,
                list.as_deref(),
                since_unix_ns,
                limit,
            )
        })?;
        rows.iter().map(|r| row_to_pydict(py, r)).collect()
    }

    /// Diff two messages by message-id. `mode` ∈ {patch, prose, raw}.
    /// Returns `{"a": <row>, "b": <row>, "text_a": str, "text_b": str}`.
    /// Caller can then run difflib / show side-by-side.
    #[pyo3(signature = (a, b, mode="patch"))]
    fn diff<'py>(
        &self,
        py: Python<'py>,
        a: String,
        b: String,
        mode: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        let m = DiffMode::from_name(mode).ok_or_else(|| {
            crate::error::Error::QueryParse(format!(
                "unknown diff mode {mode:?}; supported: patch, prose, raw"
            ))
        })?;
        let result = py.detach(|| self.inner.diff(&a, &b, m))?;
        let d = PyDict::new(py);
        d.set_item("a", row_to_pydict(py, &result.row_a)?)?;
        d.set_item("b", row_to_pydict(py, &result.row_b)?)?;
        d.set_item("text_a", result.text_a)?;
        d.set_item("text_b", result.text_b)?;
        Ok(d)
    }

    // ---- embedding tier (Phase 8) ----------------------------------

    /// Top-`k` nearest message-ids to a pre-computed query vector.
    /// `query_vec` must be L2-normalized and the same dim as the
    /// stored index. Returns `[(message_id, cosine_similarity), ...]`
    /// as a list of `(str, float)` tuples; Python side wraps in a
    /// pydantic model.
    #[pyo3(signature = (query_vec, k=25))]
    fn nearest<'py>(
        &self,
        py: Python<'py>,
        query_vec: Vec<f32>,
        k: usize,
    ) -> PyResult<Vec<(String, f32)>> {
        let result = py.detach(|| -> Result<_, crate::error::Error> {
            let Some(reader) = EmbeddingReader::open(self.inner.data_dir())? else {
                return Ok(Vec::new());
            };
            reader.nearest(&query_vec, k)
        })?;
        Ok(result)
    }

    /// Top-`k` nearest message-ids to the stored vector of an
    /// existing message. Useful for "more like this" without
    /// re-embedding text.
    #[pyo3(signature = (message_id, k=25))]
    fn nearest_to_mid(
        &self,
        py: Python<'_>,
        message_id: String,
        k: usize,
    ) -> PyResult<Vec<(String, f32)>> {
        let result = py.detach(|| -> Result<_, crate::error::Error> {
            let Some(reader) = EmbeddingReader::open(self.inner.data_dir())? else {
                return Ok(Vec::new());
            };
            reader.nearest_to_mid(&message_id, k)
        })?;
        Ok(result)
    }

    /// Current index generation counter. Bumps at every ingest commit;
    /// the Python freshness helper pairs this with `generation_mtime_ns`
    /// to produce a user-visible `as_of` timestamp + `lag_seconds`.
    fn generation(&self, py: Python<'_>) -> PyResult<u64> {
        let gen_val = py.detach(|| self.inner.generation())?;
        Ok(gen_val)
    }

    /// Last-mutation time of the generation file (ns since Unix epoch,
    /// UTC). `None` when the data_dir has never been ingested into.
    fn generation_mtime_ns(&self, py: Python<'_>) -> PyResult<Option<i64>> {
        let ns = py.detach(|| self.inner.generation_mtime_ns())?;
        Ok(ns)
    }

    /// Embedding-index dim, used by the Python tool to verify the
    /// query embedder matches the indexed embedder.
    fn embedding_dim(&self, py: Python<'_>) -> PyResult<Option<u32>> {
        let dim = py.detach(|| -> Result<Option<u32>, crate::error::Error> {
            Ok(EmbeddingReader::open(self.inner.data_dir())?.map(|r| r.meta().dim))
        })?;
        Ok(dim)
    }

    /// Embedding-index model name.
    fn embedding_model(&self, py: Python<'_>) -> PyResult<Option<String>> {
        let m = py.detach(|| -> Result<Option<String>, crate::error::Error> {
            Ok(EmbeddingReader::open(self.inner.data_dir())?.map(|r| r.meta().model.clone()))
        })?;
        Ok(m)
    }

    /// Fetch the raw uncompressed message body by Message-ID.
    ///
    /// Point-looks-up the metadata row, then streams the zstd frame
    /// from the compressed store. Returns None if the message-id is
    /// not present in the corpus.
    fn fetch_body<'py>(
        &self,
        py: Python<'py>,
        message_id: String,
    ) -> PyResult<Option<Bound<'py, pyo3::types::PyBytes>>> {
        let data = py.detach(|| self.inner.fetch_body(&message_id))?;
        Ok(data.map(|d| pyo3::types::PyBytes::new(py, &d)))
    }
}

fn row_to_pydict<'py>(py: Python<'py>, r: &MessageRow) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("message_id", &r.message_id)?;
    d.set_item("list", &r.list)?;
    d.set_item("shard", &r.shard)?;
    d.set_item("commit_oid", &r.commit_oid)?;
    d.set_item("from_addr", &r.from_addr)?;
    d.set_item("from_name", &r.from_name)?;
    d.set_item("subject_raw", &r.subject_raw)?;
    d.set_item("subject_normalized", &r.subject_normalized)?;
    d.set_item("subject_tags", &r.subject_tags)?;
    d.set_item("date_unix_ns", r.date_unix_ns)?;
    d.set_item("in_reply_to", &r.in_reply_to)?;
    d.set_item("references", &r.references)?;
    d.set_item("tid", &r.tid)?;
    d.set_item("series_version", r.series_version)?;
    d.set_item("series_index", r.series_index)?;
    d.set_item("series_total", r.series_total)?;
    d.set_item("is_cover_letter", r.is_cover_letter)?;
    d.set_item("has_patch", r.has_patch)?;
    d.set_item("touched_files", &r.touched_files)?;
    d.set_item("touched_functions", &r.touched_functions)?;
    d.set_item("files_changed", r.files_changed)?;
    d.set_item("insertions", r.insertions)?;
    d.set_item("deletions", r.deletions)?;
    d.set_item("signed_off_by", &r.signed_off_by)?;
    d.set_item("reviewed_by", &r.reviewed_by)?;
    d.set_item("acked_by", &r.acked_by)?;
    d.set_item("tested_by", &r.tested_by)?;
    d.set_item("co_developed_by", &r.co_developed_by)?;
    d.set_item("reported_by", &r.reported_by)?;
    d.set_item("fixes", &r.fixes)?;
    d.set_item("link", &r.link)?;
    d.set_item("closes", &r.closes)?;
    d.set_item("cc_stable", &r.cc_stable)?;
    d.set_item("body_offset", r.body_offset)?;
    d.set_item("body_length", r.body_length)?;
    d.set_item("body_sha256", &r.body_sha256)?;
    d.set_item("schema_version", r.schema_version)?;
    Ok(d)
}
