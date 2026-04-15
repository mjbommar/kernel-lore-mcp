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

use crate::ingest;
use crate::reader::{MessageRow, Reader as CoreReader};
use crate::router;

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
