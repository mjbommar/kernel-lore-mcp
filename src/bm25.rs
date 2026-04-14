//! BM25 tier — tantivy with our custom `kernel_prose` analyzer.
//!
//! Schema + analyzer registration live in the `schema` module so the
//! reader and writer see the same view. Positions are OFF
//! (`IndexRecordOption::WithFreqs`) — phrase queries are not
//! supported on the prose tier in v1 and the router rejects
//! `"phrase"` over `body_prose` with a clear error rather than
//! silently degrading to conjunction.
//!
//! Single-writer discipline:
//!   - One `IndexWriter` per system, held by the ingest process.
//!   - `state::writer_lockfile()` is flocked for the writer's
//!     lifetime; query processes refuse to open a writer if the
//!     lock is held.
//!   - After every writer commit, `state::bump_generation()` writes
//!     a new u64 to the generation file.
//!
//! Reader reload discipline:
//!   - `ReloadPolicy::Manual`.
//!   - Every query-request entry `stat()`s the generation file; if
//!     the u64 advanced, `reader.reload()?` runs before the query.
//!
//! Tokenizer-fingerprint sanity: a sidecar `analyzers.fingerprint`
//! file contains the SHA-256 of the registered analyzer config.
//! Opening a reader with a different fingerprint returns a loud
//! error, not silent data corruption.
//!
//! Implementation lands in a follow-up PR.
