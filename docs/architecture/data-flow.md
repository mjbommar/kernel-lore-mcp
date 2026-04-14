# Architecture — data flow

## Read path (query)

1. Client sends an MCP `tools/call` (Streamable HTTP).
2. Python tool handler validates input via pydantic (min/max length,
   enum bounds).
3. Before dispatching, the handler `stat()`s `<data_dir>/state/generation`;
   if it's advanced since the reader was last reloaded, it calls
   `_core.reader_reload()` — this keeps all uvicorn worker processes
   coherent with the ingest process.
4. The handler calls `asyncio.to_thread(_core.router.dispatch,
   query, limit, cursor)` so the asgi reactor thread isn't pinned
   for the duration of the Rust work. Inside Rust, the entry
   function uses `Python::detach(|py| ...)` (pyo3 0.28 rename from
   `allow_threads`) to let other Python threads run.
5. Rust router parses the query (see
   [`../mcp/query-routing.md`](../mcp/query-routing.md)), picks
   tiers, runs them in parallel via rayon, merges by `message_id`.
6. Router returns a `Vec<Hit>`; pyo3 converts to pydantic `SearchHit`
   models (not dicts — see `docs/mcp/tools.md` contract).
7. Handler wraps in `SearchResponse`, attaches HMAC-signed
   `next_cursor`, returns. FastMCP emits structured content with
   `outputSchema` derived from the model.

## Write path (ingest)

Ingestion runs in a **separate systemd unit** (`klmcp-ingest.service`),
not in-process with the MCP server. The ingest process holds the sole
`tantivy::IndexWriter` (via a flocked `<data_dir>/state/writer.lock`);
MCP workers see it and refuse to open another writer.

1. `grok-pull` cron pulls shard updates.
2. The ingest unit calls `_core.ingest.run_once(lore_mirror_dir)`
   (GIL released via `Python::detach` for the whole run).
3. Rust ingestor:
   - For each shard, opens `gix::ThreadSafeRepository`.
   - `rev_walk([new_head]).with_hidden([last_indexed_oid])` to get
     new commits.
   - rayon fanout across shards (not within a shard — packfile
     locality wins).
   - Per commit: read `m` blob, `mail-parser` it, split prose/patch,
     extract touched files+functions, emit append rows to:
     - compressed zstd store (mmap-appended, tracked by offset)
     - metadata Arrow batch (flushed to Parquet at segment boundary)
     - trigram builder (accumulates in RAM per shard, flushed to
       disk as one merge unit)
     - tantivy `IndexWriter` (one shared writer, segment merges
       automatic)
   - After all shards complete: commit `last_indexed_oid` state.
4. Atomic swap of new index files into place; query threads
   reopen next request.

## Failure semantics

- Ingestion is **idempotent per commit**. If we crash mid-shard, a
  re-run picks up from the last durably committed `last_indexed_oid`
  and re-processes duplicates safely (message_id is unique; we
  upsert).
- Index readers use mmap; they see a stable snapshot until they
  explicitly reopen. A swap in progress never corrupts a running
  query.
