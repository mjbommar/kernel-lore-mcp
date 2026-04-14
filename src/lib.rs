//! kernel-lore-mcp native core.
//!
//! Three-tier index + ingestion + query routing. See `docs/architecture/`
//! for the design rationale. This crate is loaded as
//! `kernel_lore_mcp._core` from the Python layer.
//!
//! Module order mirrors the data-flow pipeline:
//!   error    -- shared error type with `impl From<_> for PyErr`
//!   state    -- last-indexed-oid, index-generation epoch, lockfile
//!   schema   -- shared Arrow field defs and tantivy Schema builders
//!   store    -- compressed raw message store (zstd-dict per list)
//!   parse    -- RFC822 + trailer + subject + patch extraction
//!   metadata -- Arrow/Parquet columnar tier (writer)
//!   reader   -- Arrow/Parquet reader + query methods
//!   trigram  -- Zoekt-style trigram tier (fst + roaring)     [phase 3]
//!   bm25     -- tantivy tier with our kernel_prose analyzer  [phase 4]
//!   ingest   -- gix shard walk -> mbox parse -> tier writers
//!   router   -- query grammar, tier dispatch, result merge   [phase 4]
//!   python   -- PyO3 surface
//!
//! GIL discipline: every heavy Python-facing call releases the GIL via
//! `Python::detach(...)`. In pyo3 0.28.3 stable, `detach` and
//! `attach` are the renamed forms of `allow_threads` and `with_gil`.

use pyo3::prelude::*;

mod bm25;
mod error;
mod ingest;
mod metadata;
mod parse;
mod python;
mod reader;
mod router;
mod schema;
mod state;
mod store;
mod trigram;

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(crate::python::py_ingest_shard, m)?)?;
    m.add_class::<crate::python::PyReader>()?;
    Ok(())
}

/// Version string baked in at compile time; Python side uses this to
/// sanity-check wheel / extension version match.
#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
