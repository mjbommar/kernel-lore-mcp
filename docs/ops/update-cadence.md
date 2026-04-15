# Ops — update cadence

**See [`update-frequency.md`](./update-frequency.md) for the
authoritative policy doc + cost analysis.** This file documents the
per-stage cadence; the policy file documents the *why*.

## Pull cadence (lore → disk)

- `grok-pull` fires every **5 minutes** via the
  `klmcp-grokmirror.timer` systemd unit (not cron — timer +
  debounced path-trigger gives cleaner journal + restart semantics).
- lore.kernel.org itself trails vger by 1–5 minutes; our tick adds
  at most 5 min of jitter; ingest adds <1 min. End-to-end p50 ~5 min,
  p95 ~11 min.
- Self-hosters can override via `KLMCP_GROKMIRROR_INTERVAL_SECONDS`
  (floor 60 s, ceiling 3600 s). Hosted instance runs the 300 s
  default by policy.

## Ingestion cadence (disk → indices)

- `grok-pull`'s `post_update_hook` touches
  `$KLMCP_DATA_DIR/state/grokpull.trigger`; the
  `klmcp-ingest.path` unit watches that file and fires
  `klmcp-ingest.service` exactly once per successful pull.
- The ingest driver (`scripts/klmcp-ingest.sh`) enforces a minimum
  `KLMCP_INGEST_DEBOUNCE_SECONDS` gap (default 30 s) between
  consecutive runs regardless of trigger rate.
- Single-writer invariant: ingest acquires an exclusive `flock` on
  `state/writer.lock` (`src/state.rs::acquire_writer_lock`). A
  racing ingest returns fast with "another ingest is already
  running"; no deadlocks, no corruption.
- Cost of one tick with ~17 new messages: ~200–500 ms of one vCPU.
  Idle ticks (no changed shards) cost ~50 ms on disk stat alone.

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
