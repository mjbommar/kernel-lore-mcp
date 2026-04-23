# Ops — update cadence

**See [`update-frequency.md`](./update-frequency.md) for the
authoritative policy doc + cost analysis.** This file documents the
per-stage cadence; the policy file documents the *why*.

## Pull cadence (lore → disk)

- `klmcp-sync.timer` fires every **5 minutes** and launches the
  one-shot `kernel-lore-sync` writer.
- lore.kernel.org itself trails vger by 1–5 minutes; our tick adds
  at most 5 min of jitter; ingest adds <1 min. End-to-end p50 ~5 min,
  p95 ~11 min.
- Self-hosters can override the timer cadence, but should also set
  `KLMCP_GROKMIRROR_INTERVAL_SECONDS` in the server env so
  `/status` and `/metrics` freshness math matches the real timer.
  Hosted instance runs the 300 s default by policy.

## Ingestion cadence (disk → indices)

- `kernel-lore-sync` does manifest fetch, shard fetch, ingest, and
  generation bump in one process under one writer lock.
- Single-writer invariant: sync acquires an exclusive `flock` on
  `state/writer.lock` (`src/state.rs::acquire_writer_lock`). A
  racing sync returns fast with "another writer is already
  running"; no deadlocks, no corruption.
- Cost of one tick with ~17 new messages: ~200–500 ms of one vCPU.
  Idle ticks (no changed shards) cost ~50 ms on disk stat alone.

## Index swap

After ingestion commits:
1. Metadata tier writes a new Parquet manifest file.
2. Trigram tier writes a new segment directory.
3. If `--with-over` is enabled, `over.db` is updated incrementally.
4. If explicit inline rebuild flags are enabled, tid/path-vocab/BM25
   rebuild and commit under the same writer lock.
5. Our index reader calls `.reload()` at next request boundary
   (not mid-query).

No mid-flight inconsistency — each running query sees one snapshot
start-to-finish.

## Freshness surface

`kernel-lore-mcp status --data-dir ...` and the HTTP `/status` route
return:
```json
{
  "service": "kernel-lore-mcp",
  "version": "0.3.5",
  "generation": 5,
  "last_ingest_utc": "2026-04-14T19:41:23Z",
  "last_ingest_age_seconds": 42,
  "configured_interval_seconds": 300,
  "freshness_ok": true,
  "writer_lock_present": false,
  "sync_active": false,
  "capabilities": {
    "metadata_ready": true,
    "over_db_ready": true,
    "bm25_ready": true
  }
}
```

Add `?per_list=1` on the HTTP route when you need shard-by-shard
details. During a live sync, status also includes the current stage
from `state/sync.json`.

## Reindex cadence

Derived rebuilds from local data:
- On-demand when schema changes.
- Expected frequency: once per quarter in early life, then rare.
- Use `kernel-lore-reindex` for `tid`, `path_vocab`, and BM25.
- Documented in [`../indexing/compressed-store.md`](../indexing/compressed-store.md).

## Manifest re-fetch

`manifest.js.gz` is fetched on every sync tick. If a shard vanishes
from the manifest (lore occasionally retires shards), our state
file keeps its last-indexed oid but we stop adding to it. We don't
purge existing data.
