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

## Write path (sync / ingest)

Writes run in a **separate one-shot process** (`kernel-lore-sync`,
typically from `klmcp-sync.service`), not in-process with the MCP
server. The sync process holds the sole writer lock on
`<data_dir>/state/writer.lock`; MCP workers see it and refuse to
open another writer.

1. `kernel-lore-sync` fetches `manifest.js.gz` and diffs shard
   fingerprints against local state.
2. For changed shards, it fetches delta packfiles via `gix`.
3. Rust ingest then:
   - Opens each changed shard as a `gix::ThreadSafeRepository`.
   - Uses `rev_walk([new_head]).with_hidden([last_indexed_oid])` to
     enumerate only new commits.
   - Fans out across shards with rayon (not within a shard —
     packfile locality wins).
   - For each commit: reads the `m` blob, parses it, splits
     prose/patch, extracts touched files+functions, and appends rows
     to the compressed store, metadata Parquet, trigram tier, and
     optionally `over.db`.
4. At the end of the run, sync commits shard OIDs, bumps the
   generation marker, and optionally performs explicit inline
   rebuilds (`--with-bm25`, `--with-tid-rebuild`,
   `--with-path-vocab-rebuild`).
5. Long-lived readers reopen on the next request boundary.

## Failure semantics

- Ingestion is **idempotent per commit**. If we crash mid-shard, a
  re-run picks up from the last durably committed `last_indexed_oid`
  and re-processes duplicates safely (message_id is unique; we
  upsert).
- Index readers use mmap; they see a stable snapshot until they
  explicitly reopen. A swap in progress never corrupts a running
  query.
