//! kernel-lore-mcp native core.
//!
//! Public surface (as an `rlib`) re-exports `ingest::ingest_shard`
//! and `ingest::IngestStats` so the `kernel-lore-ingest` binary can
//! drive it without duplicating PyO3 glue.
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
mod embedding;
mod error;
mod ingest;
mod maintainers;
mod metadata;
mod over;
mod parse;
pub mod path_tier;
mod python;
mod reader;
mod router;
mod schema;
mod state;
mod store;
mod tid;
mod timeout;
mod trigram;

// Library re-exports for the `kernel-lore-ingest` binary (and any
// future internal tooling) so they don't have to name the module
// paths.
pub use bm25::BmWriter;
pub use embedding::{EmbeddingBuilder, EmbeddingMeta, EmbeddingReader};
pub use ingest::{
    IngestStats, ingest_shard, ingest_shard_unlocked, ingest_shard_with_bm25, rebuild_bm25,
};
pub use over::{DddPayload, OverDb, OverRow};
pub use reader::{MessageRow, Reader};
pub use router::{CursorPayload, ParsedQuery, RankedHit, parse_query, sign_cursor, verify_cursor};
pub use state::State;
pub use store::Store;
pub use tid::{TidRow, rebuild as rebuild_tid};

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(crate::python::py_ingest_shard, m)?)?;
    m.add_function(wrap_pyfunction!(crate::python::py_rebuild_tid, m)?)?;
    m.add_function(wrap_pyfunction!(crate::python::py_rebuild_bm25, m)?)?;
    m.add_function(wrap_pyfunction!(
        crate::python::py_build_embedding_index,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(crate::python::py_embedding_meta, m)?)?;
    m.add_class::<crate::python::PyReader>()?;
    m.add_class::<crate::python::PyEmbeddingBuilder>()?;
    Ok(())
}

/// Version string baked in at compile time; Python side uses this to
/// sanity-check wheel / extension version match.
#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
