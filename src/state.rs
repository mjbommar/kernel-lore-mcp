//! Cross-tier state management:
//!   - per-shard `last_indexed_oid` (incremental ingest pointer)
//!   - per-index `generation` epoch counter (readers re-open when it advances)
//!   - singleton-writer lockfile (prevents multiple `IndexWriter`s)
//!
//! All writes go through atomic-rename to survive crashes. Callers
//! who find the state file missing or corrupt should fall back to a
//! full re-walk; public-inbox shards can occasionally be repacked
//! upstream, invalidating any stored OID.
//!
//! Concrete file layout under `<data_dir>/state/`:
//!     shards/<list>/<N>.oid        -- 40-byte hex, trailing newline
//!     generation                   -- u64 counter, updated on each commit
//!     writer.lock                  -- flock-held by the ingest process
//!
//! Implementation lands in a follow-up PR; this skeleton pins the
//! contract.
