# Production hardening — anonymous multi-tenant readiness

Operating notes for exposing `kernel-lore-mcp` as a public service
where callers are anonymous and their workload is adversarial-until-
proven-otherwise.

If you're running a single-user local instance (`stdio` transport,
one agent), most of this doesn't apply — the defaults are safe for
that shape. Read this when you flip `transport=http` and point it
at the open internet.

## Threat model (summarized)

- No authentication is part of the product contract (see CLAUDE.md).
  Every query is treated as anonymous.
- Expected abuse shapes:
  1. **Cheap-query flood** — millions of `fetch_message` / `eq`.
  2. **Expensive-query flood** — a handful of `substr_*`, `regex`,
     `include_mentions=True`, or `lore_nearest` per minute that
     saturate the worker pool.
  3. **Pathological inputs** — regex DoS (mitigated by DFA-only
     engine), unbounded patch-search needles, forged cursors,
     oversized queries.
- Upstream data (lore mirror) is trusted. Shard integrity is a
  grokmirror problem, not a query-serving problem.

## Defenses in place (as of April 2026)

| Threat | Mitigation | Where |
|---|---|---|
| Cheap-query flood | over.db connection pool (default 3) + WAL, fetch_message p99 2.3 ms at 16 concurrent | `src/over.rs::OverDbPool`, `src/reader.rs` |
| Expensive-query flood | Per-cost-class in-flight semaphores (cheap=1024 / moderate=32 / expensive=4), reject fast with `rate_limited` | `src/kernel_lore_mcp/cost_class.py` |
| Unbounded `include_mentions` scan | `mention_limit` ceiling = 500; refuses `include_mentions=True` unless `list_filter` or `since_unix_ns` narrows | `src/kernel_lore_mcp/tools/author_profile.py` |
| Runaway Rust-side scan | Thread-local `Deadline` checked at every Parquet batch boundary; Python `asyncio.wait_for` wraps the call | `src/timeout.rs`, `src/reader.rs::scan`, `src/python.rs::read_query_guard` |
| Regex DoS | `regex-automata` DFA-only, anchor-required mode, backrefs rejected | `src/reader.rs::regex` |
| Pathological `patch_search` needle | `MAX_PATCH_CANDIDATES=100 000` cap on trigram candidate union | `src/reader.rs` |
| Oversized query | pydantic `max_length=2048` on `lore_search.query`; longer inputs rejected as `query_too_long` | `src/kernel_lore_mcp/tools/search.py` |
| Forged pagination cursor | Any non-None cursor rejected as `invalid_cursor` (pagination not yet shipped) | `src/kernel_lore_mcp/tools/search.py` |
| Tantivy cold-mmap tail | One throwaway prose_search at server boot pages BM25 segments in | `src/kernel_lore_mcp/server.py::_warmup_tiers` |
| Tier drift / partial ingest | Per-tier generation markers; Reader disables over.db on drift; per-row fallback to Parquet on miss | `src/state.rs`, `src/reader.rs` |

## Environment knobs

| Variable | Default | Raise for |
|---|---|---|
| `KLMCP_OVER_POOL_SIZE` | 3 | >16 concurrent clients per process |
| `KLMCP_COST_CAP_CHEAP` | 1024 | Effectively unbounded |
| `KLMCP_COST_CAP_MODERATE` | 32 | Larger boxes (16+ vCPUs) |
| `KLMCP_COST_CAP_EXPENSIVE` | 4 | Rarely; expensive tools are expensive |
| `KLMCP_MAINTAINERS_FILE` | `<data_dir>/MAINTAINERS` | Custom kernel-tree snapshot path |
| `KLMCP_CURSOR_KEY` | (unset) | Required in http mode once pagination ships |
| `KLMCP_LOG_LEVEL` | INFO | DEBUG when diagnosing a specific request |
| `KLMCP_DISABLE_OVER` | (unset) | Set to `1` for parity tests that compare the over.db indexed paths against the legacy Parquet scan. Production leaves this unset. |
| `KLMCP_BIND` | 127.0.0.1 | **`0.0.0.0` only when behind a reverse proxy** |

## Capacity planning

Baseline deploy (from CLAUDE.md): `r7g.xlarge` (4 vCPU, 32 GB RAM,
Graviton) with `gp3` 16 000 IOPS / 1 000 MB/s.

Measured on the 17.6 M-row klmcp-local corpus
(`scripts/bench/bench_concurrent_mixed.py`):

| Concurrency | Total RPS | Agg mean (ms) | router p50 | fetch_message p50 |
|---|---|---|---|---|
| 1 | 415 | 2.4 | 5.2 | 0.04 |
| 4 | 1 326 | 3.0 | 6.1 | 0.04 |
| 16 | 1 526 | 10.4 | 33.4 | 0.05 |
| 32 | 1 521 | 20.8 | 110.9 | 0.05 |

Observations:
- Near-linear scaling to 4 concurrent, saturation at 16.
- Scale horizontally (more boxes) before going past ~16 concurrent
  per box — router p50 degrades 7 ms → 33 ms → 111 ms across
  c=4/16/32.
- Indexed point lookups (`fetch_message`, `expand_citation`) stay
  at 0.04-0.05 ms p50 regardless of load; over.db pool absorbs.

Memory footprint (steady-state, 17.6 M-row corpus):
- BM25 tantivy mmap: ~32 GB virtual, resident grows with working
  set of queries.
- Trigram mmap: ~17 GB virtual.
- over.db: 19 GB file + 200 MB cache per pooled connection.
- Reader per-process overhead: ~500 MB before any queries.
- Query working set: small (tens of MB per in-flight query).

Plan: 32 GB RAM keeps the hot set in resident memory; 64 GB is
comfortable for larger BM25 working sets.

## Deployment layout

**Ingest and serving are separate systemd units.** The ingest
writer holds the per-data_dir flock; the serving reader can run
any number of instances against the same data_dir. Recommended:

```
klmcp-ingest.service           (one-shot, runs every N minutes)
  ExecStart=/usr/bin/kernel-lore-ingest --with-over
  After=klmcp-grok-pull.service
  CPUQuota=200%

klmcp-mcp.service              (long-running, serving)
  ExecStart=/usr/bin/kernel-lore-mcp serve --transport http --port 8787
  Environment=KLMCP_DATA_DIR=/var/klmcp/data
  Environment=KLMCP_OVER_POOL_SIZE=3
  Environment=KLMCP_BIND=127.0.0.1
  MemoryHigh=28G
  MemoryMax=30G
```

Front with nginx for TLS + X-Forwarded-For. Reverse-proxy rate
limits at the nginx layer complement the cost-class caps inside
the server — use them to catch single-IP floods before they hit
the Python layer.

## Monitoring signals

Exported at `/metrics` (Prometheus format):

- `klmcp_tool_calls_total{tool,status}` — per-tool call counts.
  `status` ∈ `{ok, timeout, rate_limited, invalid_argument, ...}`.
- `klmcp_tool_duration_seconds{tool}` — latency histogram.
- `klmcp_corpus_generation` — current generation counter.
- `klmcp_corpus_last_ingest_age_seconds` — since the last bump.

What to alert on:
- `rate_limited` rate > 1% of requests for more than 5 minutes —
  someone is flooding expensive tools or the cap is too tight.
- `timeout` rate > 0.1% — a scan path isn't honoring the deadline
  (check the `Deadline` wiring is present in new tools).
- `last_ingest_age_seconds` > 3 × `grokmirror_interval_seconds` —
  ingest is stuck; staleness-ok check in `/status` will flip.
- Process RSS growing past `MemoryHigh` — a scan-accumulating
  regression (the OOM round should make this unreachable, but
  worth watching).

## Known gaps (not blocking launch, worth knowing)

- **Per-IP fairness** — the cost-class semaphore is process-global.
  One IP flooding expensive tools locks out every other IP in
  that class. A future commit will layer per-IP buckets on top.
- **Thread-participant scope** — `include_mentions=True` catches
  formal trailers but not Cc/body mentions (~40% of lore's
  full-text hits). Follow-up `thread_participant_scope`
  feature pending.
- **Git sidecar integration** — the sidecar binary and SQLite
  tier exist (#40), but `lore_stable_backport_status` and
  `lore_thread_state` don't yet consult them. They'll upgrade
  from pure-lore heuristic to authoritative git-tree truth
  when wired.
