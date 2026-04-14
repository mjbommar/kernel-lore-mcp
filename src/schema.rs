//! Single source of truth for field names and Arrow/tantivy schemas.
//!
//! Why centralize: metadata ingest, BM25 ingest, reindex binary, and
//! the query router all reference the same field names. Dropping them
//! into string literals across modules is a recurring source of bugs
//! (typos, rename drift). This module owns the definitions.
//!
//! v1 columns: see docs/indexing/metadata-tier.md.
//! v1 analyzers: see docs/indexing/tokenizer-spec.md.
//!
//! `SCHEMA_VERSION` is bumped on every breaking change; the metadata
//! Parquet carries this as a column so old-version rows are rejected
//! at open time rather than silently mis-read.
#[allow(dead_code)] // wired in a follow-up PR
pub const SCHEMA_VERSION: u32 = 1;

// TODO: populate Arrow field defs + tantivy Schema builder in a
// follow-up PR.
