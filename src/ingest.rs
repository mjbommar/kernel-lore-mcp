//! Ingestion pipeline: lore shards -> RFC822 blobs -> tiers + store.
//!
//! Runs in its own process (`klmcp-ingest.service`), not in the MCP
//! server process. Holds the sole `bm25::IndexWriter` plus the
//! trigram builder and the store appender.
//!
//! Per-shard flow:
//!   1. `gix::ThreadSafeRepository::open(shard)` ->
//!      `.to_thread_local()` inside the rayon worker.
//!   2. Load `state::last_indexed_oid(shard)`. Fall back to full walk
//!      if `find_object(last_oid)` fails (shard was repacked upstream).
//!   3. `rev_walk([head]).with_hidden(last_oid)` for incremental.
//!   4. For each commit: read `m` blob, `mail-parser` it, extract
//!      structured fields (trailers, subject tags, series index,
//!      touched files/functions, patch_stats), compute `tid`.
//!   5. Append body to compressed store, emit Arrow metadata batch,
//!      feed trigram builder (patch), feed tantivy writer (prose).
//!   6. After shard done: `state::save_last_indexed_oid(shard, head)`.
//!
//! Fanout discipline: one rayon task per shard (390 shards >> CPU).
//! NOT within a shard (packfile cache locality).
//!
//! Commit + swap:
//!   - tantivy `IndexWriter::commit()`
//!   - Parquet finalize
//!   - trigram segment rename
//!   - `state::bump_generation()`
//!
//! See docs/ingestion/shard-walking.md for the concrete walk
//! pattern and docs/ingestion/mbox-parsing.md for field extraction.
//!
//! Implementation lands in a follow-up PR.
