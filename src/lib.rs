//! kernel-lore-mcp native core.
//!
//! Three-tier index + ingestion + query routing. See `docs/architecture/`
//! for the design rationale. This crate is loaded as
//! `kernel_lore_mcp._core` from the Python layer.
//!
//! Module order mirrors the data-flow pipeline:
//!   error   -- shared error type with `impl From<_> for PyErr`
//!   state   -- last-indexed-oid, index-generation epoch, lockfile
//!   schema  -- shared Arrow field defs and tantivy Schema builders
//!   store   -- compressed raw message store (zstd-dict per list)
//!   metadata-- Arrow/Parquet columnar tier
//!   trigram -- Zoekt-style trigram tier (fst + roaring)
//!   bm25    -- tantivy tier with our kernel_prose analyzer
//!   ingest  -- gix shard walk -> mbox parse -> all tiers
//!   router  -- query grammar, tier dispatch, result merge
//!
//! GIL discipline: every heavy Python-facing call releases the GIL
//! via `Python::detach(...)`. In pyo3 0.28.3 stable, `detach` and
//! `attach` are the renamed forms of `allow_threads` and `with_gil`.

use pyo3::prelude::*;

mod bm25;
mod error;
mod ingest;
mod metadata;
mod router;
mod schema;
mod state;
mod store;
mod trigram;

#[pymodule]
mod _core {
    use super::*;

    /// Version string baked in at compile time; Python side uses this to
    /// sanity-check wheel / extension version match.
    #[pyfunction]
    fn version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}
