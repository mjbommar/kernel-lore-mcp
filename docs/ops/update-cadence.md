# Ops — update cadence

## Pull cadence (lore → disk)

- `grok-pull` cron at `*/10 * * * *` (every 10 minutes).
- lore.kernel.org itself trails vger by 1–5 minutes, so p95 freshness
  from send → our disk is ~15–20 minutes. Don't promise better.

## Ingestion cadence (disk → indices)

- After every successful `grok-pull`, we invoke `_native.ingest.run_once`
  synchronously in a single worker. Idempotent; overlapping runs
  short-circuit via a filesystem lock.
- Full ingestion of new commits since last run usually finishes in
  seconds (lore produces <50k new messages/hour worst case).

## Index swap

After ingestion commits:
1. Metadata tier writes a new Parquet manifest file.
2. Trigram tier writes a new segment directory.
3. BM25 tier calls `IndexWriter::commit()`; tantivy rotates
   segments atomically.
4. Our index reader calls `.reload()` at next request boundary
   (not mid-query).

No mid-flight inconsistency — each running query sees one snapshot
start-to-finish.

## Freshness surface

`/status` endpoint returns:
```json
{
  "last_grok_pull_utc": "2026-04-14T19:41:00Z",
  "last_ingest_utc": "2026-04-14T19:41:23Z",
  "per_list": {
    "linux-cifs": { "last_message_date": "2026-04-14T19:30:12Z" },
    "lkml":       { "last_message_date": "2026-04-14T19:39:04Z" }
  },
  "pending_lists": [],
  "index_generation": 41127
}
```

Every MCP tool response also carries an inline
`freshness.oldest_list_last_updated` so LLM callers can qualify
claims without a separate call.

## Reindex cadence

Full reindex from compressed store:
- On-demand when schema changes.
- Expected frequency: once per quarter in early life, then rare.
- Documented in [`../indexing/compressed-store.md`](../indexing/compressed-store.md).

## Manifest re-fetch

`manifest.js.gz` pulled on every `grok-pull`. If a shard vanishes
from the manifest (lore occasionally retires shards), our state
file keeps its last-indexed oid but we stop adding to it. We don't
purge existing data.
