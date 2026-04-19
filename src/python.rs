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

/// Rebuild the BM25 index from the compressed store + metadata.
/// Returns the number of docs indexed.
#[pyfunction]
#[pyo3(name = "rebuild_bm25")]
pub fn py_rebuild_bm25(py: Python<'_>, data_dir: PathBuf) -> PyResult<u64> {
    let count = py.detach(|| ingest::rebuild_bm25(&data_dir))?;
    Ok(count)
}

/// Incremental, streaming builder for the embedding index.
///
/// Python opens one `EmbeddingBuilder`, pushes batches via
/// `add_batch(mids, vectors)` as the embedder runs, then calls
/// `finalize()`. Vectors are written to disk on every call — the
/// builder never accumulates the full corpus (~54 GB at 17.6M×768×4).
///
/// The one-shot `build_embedding_index` function below is a thin
/// wrapper for callers that already have both lists in memory.
#[pyclass(name = "EmbeddingBuilder")]
pub struct PyEmbeddingBuilder {
    inner: Option<EmbeddingBuilder>,
}

#[pymethods]
impl PyEmbeddingBuilder {
    #[new]
    fn new(data_dir: PathBuf, model: String, dim: u32) -> PyResult<Self> {
        let b = EmbeddingBuilder::new(&data_dir, model, dim)?;
        Ok(Self { inner: Some(b) })
    }

    fn add(&mut self, message_id: &str, vector: Vec<f32>) -> PyResult<()> {
        let b = self.inner.as_mut().ok_or_else(|| {
            crate::error::Error::State("EmbeddingBuilder already finalized".into())
        })?;
        b.add(message_id, &vector)?;
        Ok(())
    }

    fn add_batch(
        &mut self,
        message_ids: Vec<String>,
        vectors: Vec<Vec<f32>>,
    ) -> PyResult<()> {
        if message_ids.len() != vectors.len() {
            return Err(crate::error::Error::State(format!(
                "add_batch: {} message-ids vs {} vectors",
                message_ids.len(),
                vectors.len()
            ))
            .into());
        }
        let b = self.inner.as_mut().ok_or_else(|| {
            crate::error::Error::State("EmbeddingBuilder already finalized".into())
        })?;
        for (mid, v) in message_ids.iter().zip(vectors.iter()) {
            b.add(mid, v)?;
        }
        Ok(())
    }

    #[pyo3(signature = (build_hnsw=true))]
    fn finalize<'py>(
        &mut self,
        py: Python<'py>,
        build_hnsw: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        let b = self.inner.take().ok_or_else(|| {
            crate::error::Error::State("EmbeddingBuilder already finalized".into())
        })?;
        let meta = py.detach(move || b.finalize_with_hnsw(build_hnsw))?;
        let d = PyDict::new(py);
        d.set_item("model", &meta.model)?;
        d.set_item("dim", meta.dim)?;
        d.set_item("metric", &meta.metric)?;
        d.set_item("count", meta.count)?;
        d.set_item("schema_version", meta.schema_version)?;
        Ok(d)
    }

    fn __len__(&self) -> usize {
        self.inner.as_ref().map(|b| b.len()).unwrap_or(0)
    }
}

/// Build (or rebuild) the embedding index. Caller passes parallel
/// lists of message-ids and L2-normalized f32 vectors (one row each).
/// The Python side runs the actual embedding model (fastembed) and
/// hands the resulting `numpy.ndarray.astype(np.float32)` here.
///
/// Idempotent — overwrites `<data_dir>/embedding/` atomically.
///
/// **Memory warning:** holds both full lists in RAM. For corpus-scale
/// builds use `EmbeddingBuilder` (streaming).
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
        let mut b = EmbeddingBuilder::new(&data_dir, model, dim)?;
        for (mid, v) in message_ids.iter().zip(vectors.iter()) {
            b.add(mid, v)?;
        }
        b.finalize()
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
    ) -> PyResult<Bound<'py, PyDict>> {
        type DispatchOut = (Vec<router::RankedHit>, Vec<String>);
        let (hits, default_applied) =
            py.detach(|| -> Result<DispatchOut, crate::error::Error> {
                let parsed = router::parse_query(&query)?;
                router::dispatch(&self.inner, &parsed, limit)
            })?;
        let rows: Vec<Bound<'py, PyDict>> = hits
            .iter()
            .map(|h| {
                let d = row_to_pydict(py, &h.row)?;
                d.set_item("_score", h.fused_score)?;
                d.set_item("_tier_provenance", &h.tier_provenance)?;
                d.set_item("_is_exact_match", h.is_exact_match)?;
                Ok::<_, PyErr>(d)
            })
            .collect::<PyResult<_>>()?;
        let out = PyDict::new(py);
        out.set_item("hits", rows)?;
        out.set_item("default_applied", default_applied)?;
        Ok(out)
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
    /// `fuzzy_edits`: 0 = exact (default), 1-2 = Levenshtein
    /// approximate substring match at confirmation step.
    #[pyo3(signature = (needle, list=None, limit=100, fuzzy_edits=0))]
    fn patch_search<'py>(
        &self,
        py: Python<'py>,
        needle: String,
        list: Option<String>,
        limit: usize,
        fuzzy_edits: u32,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let rows = py.detach(|| {
            self.inner
                .patch_search_fuzzy(&needle, list.as_deref(), limit, fuzzy_edits)
        })?;
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

    /// Aggregate profile for one author. Samples their most-recent
    /// `limit` messages via the indexed from_addr path. Optionally
    /// expands scope with `include_mentions=True` to also aggregate
    /// rows where the address appears in any trailer on someone
    /// else's patch (bounded by `mention_limit`, one extra Parquet
    /// scan — slower, matches what a full-text search on lore shows).
    #[pyo3(signature = (
        addr,
        list=None,
        since_unix_ns=None,
        limit=10_000,
        include_mentions=false,
        mention_limit=2_000,
    ))]
    fn author_profile<'py>(
        &self,
        py: Python<'py>,
        addr: String,
        list: Option<String>,
        since_unix_ns: Option<i64>,
        limit: usize,
        include_mentions: bool,
        mention_limit: usize,
    ) -> PyResult<Bound<'py, PyDict>> {
        let profile = py.detach(|| {
            self.inner.author_profile_extended(
                &addr,
                list.as_deref(),
                since_unix_ns,
                limit,
                include_mentions,
                mention_limit,
            )
        })?;

        let d = PyDict::new(py);
        d.set_item("addr_queried", &profile.addr_queried)?;
        d.set_item("sampled", profile.sampled)?;
        d.set_item("authored_count", profile.authored_count)?;
        d.set_item("mention_count", profile.mention_count)?;
        d.set_item("limit_hit", profile.limit_hit)?;
        d.set_item("oldest_unix_ns", profile.oldest_unix_ns)?;
        d.set_item("newest_unix_ns", profile.newest_unix_ns)?;
        d.set_item("patches_with_content", profile.patches_with_content)?;
        d.set_item("cover_letters", profile.cover_letters)?;
        d.set_item("unique_subjects", profile.unique_subjects)?;
        d.set_item("with_fixes_trailer", profile.with_fixes_trailer)?;

        let own = PyDict::new(py);
        own.set_item(
            "signed_off_by_present",
            profile.own_trailers.signed_off_by_present,
        )?;
        own.set_item("fixes_issued", profile.own_trailers.fixes_issued)?;
        d.set_item("own_trailers", own)?;

        let recv = PyDict::new(py);
        recv.set_item("reviewed_by", profile.received_trailers.reviewed_by)?;
        recv.set_item("acked_by", profile.received_trailers.acked_by)?;
        recv.set_item("tested_by", profile.received_trailers.tested_by)?;
        recv.set_item(
            "co_developed_by",
            profile.received_trailers.co_developed_by,
        )?;
        recv.set_item("reported_by", profile.received_trailers.reported_by)?;
        recv.set_item("cc_stable", profile.received_trailers.cc_stable)?;
        d.set_item("received_trailers", recv)?;

        let subs_list: Vec<Bound<'py, PyDict>> = profile
            .subsystems
            .iter()
            .map(|s| {
                let b = PyDict::new(py);
                b.set_item("list", &s.list)?;
                b.set_item("patches", s.patches)?;
                b.set_item("oldest_unix_ns", s.oldest_unix_ns)?;
                b.set_item("newest_unix_ns", s.newest_unix_ns)?;
                Ok::<_, PyErr>(b)
            })
            .collect::<PyResult<_>>()?;
        d.set_item("subsystems", subs_list)?;

        Ok(d)
    }

    /// Cross-reference a kernel path against the MAINTAINERS snapshot
    /// + observed lore activity.
    #[pyo3(signature = (path, window_days=180, activity_limit=5000))]
    fn maintainer_profile<'py>(
        &self,
        py: Python<'py>,
        path: String,
        window_days: u32,
        activity_limit: usize,
    ) -> PyResult<Bound<'py, PyDict>> {
        let profile = py.detach(|| {
            self.inner
                .maintainer_profile(&path, window_days, activity_limit)
        })?;

        let out = PyDict::new(py);
        out.set_item("path_queried", &profile.path_queried)?;
        out.set_item("maintainers_available", profile.maintainers_available)?;
        out.set_item("sampled_patches", profile.sampled_patches)?;

        let declared: Vec<Bound<'py, PyDict>> = profile
            .declared
            .iter()
            .map(|e| {
                let d = PyDict::new(py);
                d.set_item("name", &e.name)?;
                d.set_item("status", e.status.clone())?;
                d.set_item("depth", e.depth)?;
                d.set_item("lists", &e.lists)?;
                d.set_item("maintainers", &e.maintainers)?;
                d.set_item("reviewers", &e.reviewers)?;
                Ok::<_, PyErr>(d)
            })
            .collect::<PyResult<_>>()?;
        out.set_item("declared", declared)?;
        out.set_item("stale_declared", profile.stale_declared)?;

        let obs_to_py = |o: &crate::reader::ObservedAddr| -> PyResult<Bound<'py, PyDict>> {
            let d = PyDict::new(py);
            d.set_item("email", &o.email)?;
            d.set_item("reviewed_by", o.reviewed_by)?;
            d.set_item("acked_by", o.acked_by)?;
            d.set_item("tested_by", o.tested_by)?;
            d.set_item("signed_off_by", o.signed_off_by)?;
            d.set_item("last_seen_unix_ns", o.last_seen_unix_ns)?;
            Ok(d)
        };
        let active: Vec<Bound<'py, PyDict>> = profile
            .active_unlisted
            .iter()
            .map(&obs_to_py)
            .collect::<PyResult<_>>()?;
        out.set_item("active_unlisted", active)?;
        let observed: Vec<Bound<'py, PyDict>> = profile
            .observed
            .iter()
            .map(&obs_to_py)
            .collect::<PyResult<_>>()?;
        out.set_item("observed", observed)?;

        Ok(out)
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

    /// Path tier: search for messages mentioning a file path.
    ///
    /// `match_mode`: "exact" | "basename" | "prefix".
    /// Returns a list of row-dicts (same shape as `fetch_message`).
    #[pyo3(signature = (path, match_mode="exact", list=None, since_unix_ns=None, limit=100))]
    fn path_mentions<'py>(
        &self,
        py: Python<'py>,
        path: String,
        match_mode: &str,
        list: Option<String>,
        since_unix_ns: Option<i64>,
        limit: usize,
    ) -> PyResult<Vec<Bound<'py, pyo3::types::PyDict>>> {
        use crate::path_tier;

        let data_dir = self.inner.data_dir().to_owned();
        let mode = match_mode.to_owned();

        let rows = py.detach(
            move || -> Result<Vec<crate::reader::MessageRow>, crate::error::Error> {
                let vocab = path_tier::load_vocab(&data_dir)?;
                let Some(vocab) = vocab else {
                    return Ok(Vec::new());
                };

                let path_ids: Vec<u32> = match mode.as_str() {
                    "exact" => vocab.lookup_exact(&path).into_iter().collect(),
                    "basename" => vocab.lookup_basename(&path).to_vec(),
                    "prefix" => vocab.lookup_prefix(&path),
                    _ => {
                        return Err(crate::error::Error::State(format!(
                            "unknown match_mode {mode:?}; use exact/basename/prefix"
                        )));
                    }
                };

                if path_ids.is_empty() {
                    return Ok(Vec::new());
                }

                // Stream rows; break as soon as `limit` matches land.
                // The old implementation called all_rows which would
                // materialize the full 17.6M-row corpus into a Vec
                // when `list=None` — OOM.
                let reader = crate::reader::Reader::new(&data_dir);
                let list_owned = list.clone();
                let mut results = Vec::new();
                let mut scan_err: Option<crate::error::Error> = None;
                reader.scan_streaming(list_owned.as_deref(), |row| {
                    if let Some(since) = since_unix_ns {
                        if let Some(d) = row.date_unix_ns {
                            if d < since {
                                return true;
                            }
                        }
                    }
                    let body = match reader.fetch_body(&row.message_id) {
                        Ok(Some(b)) => b,
                        Ok(None) => return true,
                        Err(e) => {
                            scan_err = Some(e);
                            return false;
                        }
                    };
                    let found = vocab.scan_body(&body);
                    if found.iter().any(|id| path_ids.contains(id)) {
                        results.push(row);
                        if results.len() >= limit {
                            return false;
                        }
                    }
                    true
                })?;
                if let Some(e) = scan_err {
                    return Err(e);
                }
                Ok(results)
            },
        )?;

        rows.iter()
            .map(|r| crate::python::row_to_pydict(py, r))
            .collect()
    }

    /// Stream every row in the metadata tier through a Python
    /// callback. The callback receives a list of row-dicts (one
    /// batch at a time) and returns True to continue or False to
    /// stop early.
    ///
    /// Used by the embedding-bootstrap CLI; avoids materializing
    /// the full 17.6M-row corpus (~45 GB RSS) in one Python list.
    #[pyo3(signature = (callback, batch_size=2048, list=None, since_unix_ns=None))]
    fn scan_batches(
        &self,
        py: Python<'_>,
        callback: Bound<'_, pyo3::PyAny>,
        batch_size: usize,
        list: Option<String>,
        since_unix_ns: Option<i64>,
    ) -> PyResult<()> {
        let batch_size = batch_size.max(1);
        let mut buf: Vec<MessageRow> = Vec::with_capacity(batch_size);
        let mut stop = false;
        let mut py_err: Option<PyErr> = None;

        let flush = |py: Python<'_>,
                     callback: &Bound<'_, pyo3::PyAny>,
                     buf: &mut Vec<MessageRow>,
                     stop: &mut bool,
                     py_err: &mut Option<PyErr>| {
            if buf.is_empty() {
                return;
            }
            let dicts: Vec<Bound<'_, PyDict>> = match buf
                .iter()
                .map(|r| row_to_pydict(py, r))
                .collect::<PyResult<Vec<_>>>()
            {
                Ok(v) => v,
                Err(e) => {
                    *py_err = Some(e);
                    *stop = true;
                    buf.clear();
                    return;
                }
            };
            match callback.call1((dicts,)) {
                Ok(ret) => match ret.is_truthy() {
                    Ok(true) => {}
                    Ok(false) => *stop = true,
                    Err(e) => {
                        *py_err = Some(e);
                        *stop = true;
                    }
                },
                Err(e) => {
                    *py_err = Some(e);
                    *stop = true;
                }
            }
            buf.clear();
        };

        self.inner
            .scan_streaming(list.as_deref(), |row| {
                if stop {
                    return false;
                }
                if let Some(since) = since_unix_ns {
                    if let Some(d) = row.date_unix_ns {
                        if d < since {
                            return true;
                        }
                    }
                }
                buf.push(row);
                if buf.len() >= batch_size {
                    flush(py, &callback, &mut buf, &mut stop, &mut py_err);
                }
                !stop
            })?;

        if !stop {
            flush(py, &callback, &mut buf, &mut stop, &mut py_err);
        }

        if let Some(e) = py_err {
            return Err(e);
        }
        Ok(())
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
    d.set_item("suggested_by", &r.suggested_by)?;
    d.set_item("helped_by", &r.helped_by)?;
    d.set_item("assisted_by", &r.assisted_by)?;
    d.set_item("fixes", &r.fixes)?;
    d.set_item("link", &r.link)?;
    d.set_item("closes", &r.closes)?;
    d.set_item("cc_stable", &r.cc_stable)?;
    d.set_item("trailers_json", &r.trailers_json)?;
    d.set_item("body_segment_id", r.body_segment_id)?;
    d.set_item("body_offset", r.body_offset)?;
    d.set_item("body_length", r.body_length)?;
    d.set_item("body_sha256", &r.body_sha256)?;
    d.set_item("schema_version", r.schema_version)?;
    Ok(d)
}
