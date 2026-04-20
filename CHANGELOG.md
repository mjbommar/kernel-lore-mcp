# Changelog

All notable changes to `kernel-lore-mcp` land here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [SemVer](https://semver.org/).

Unreleased changes accumulate under an `## [Unreleased]` heading;
release tags move them into a dated section. Release process in
[`CONTRIBUTING.md`](./CONTRIBUTING.md).

## [Unreleased]

## [0.2.0] — 2026-04-20

### Added

**Onboarding-review cleanup.** Silent-failure modes caught by the
review replaced with structured errors + CI-enforced drift checks:
- `lore_path_mentions` now raises `setup_required` with the exact
  build command when `paths/vocab.txt` is missing — replaces the
  silent-empty behavior that looked identical to "no matches."
  `_core.rebuild_path_vocab(data_dir)` builds it from
  `over_touched_file` (fast) with a metadata-Parquet fallback for
  fresh deploys. Verified on lore: 704k paths in seconds.
- `_surface_manifest.py` is the single source of truth for the
  required tool / resource / prompt surface; doctor imports it;
  `test_surface_manifest.py` asserts live-server subset containment
  in CI. Fixed the `lore://thread/{tid}` vs `{mid}` drift the
  review caught.
- `kernel-lore-ingest` now exits 2 on missing / empty mirror paths
  with an actionable error; `--allow-empty` opts into the cron-
  style no-op previously baked into the default behavior.
- New `status.capabilities()` helper exposed on `/status`, the
  `stats://coverage` resource, and the `lore_corpus_stats` tool.
  Boolean readiness per tier (`metadata_ready`, `over_db_ready`,
  `bm25_ready`, `trigram_ready`, `tid_ready`, `path_vocab_ready`,
  `embedding_ready`, `maintainers_ready`, `git_sidecar_ready`) so
  callers distinguish "feature not provisioned" from "no matches."
- Shared `LoreError.setup_required(feature, missing, build_cmd)`
  for every optional-tier tool — consistent caller-facing shape.

**Two new perf wins on the trailer / touched-file fast paths (#64).**
- `eq('signed_off_by', <email>)` on lore scale with 28k+ SOB rows
  for prolific maintainers (kuba, davem, gregkh, akpm): **53-64 ms
  → 0.34-0.43 ms p50** (~150×). Same shape for reviewed_by,
  acked_by, tested_by, co_developed_by, reported_by.
- `eq('touched_files', <path>)` on popular kernel paths: **warm
  3.8 s → 0.4-0.7 ms p50** (>5000×).
- Mechanism: denormalized `date_unix_ns` into `over_trailer_email`
  and `over_touched_file`, added partial covering indexes on
  `(kind, email, date_unix_ns DESC)` and `(path, date_unix_ns DESC)`,
  rewrote `scan_eq_via_*` as a `WITH picked AS (...)` CTE that
  streams top-N off the covering index and bounded-JOINs back to
  `over` for full rows. Eliminates the TEMP B-TREE sort over
  tens-of-thousands of candidate rows the old plan paid for every
  prolific-maintainer / popular-path query.
- `OverDb::backfill_side_table_dates` + `_core.backfill_side_table_
  dates(data_dir)` copy `date_unix_ns` from `over` into the side
  tables on pre-#64 deployments without a full rebuild. Chunked
  rowid-cursor walk with per-chunk HashMap lookups (naive correlated
  UPDATE generated a 10+ GB WAL). Landed 27.0 M rows in 29 min on
  klmcp-local.

**Trigram segment cache warmup at boot (#70).** Adds a
`reader.patch_search("__function__", None, 1)` probe to
`_warmup_tiers` so the OS page cache holds the ~530 trigram segment
files before the first real request. Complements the per-process
`SegmentReader` cache shipped in #66; first cross-list
`patch_search` no longer pays the 9 s cold-mmap tail.

**Pagination cursors fanned out to every paginated tool (#71).**
The primitives + pattern shipped in #67 now cover the full surface:
- `lore_search` (#67; RRF-score + mid tiebreak)
- `lore_patch_search` — date_unix_ns + mid tiebreak
- `lore_regex` — date + mid (query_hash includes field / pattern /
  anchor_required / list / since)
- `lore_activity` — date + mid (query_hash includes
  file / function / since / list)
- `lore_author_footprint` — date + mid (query_hash includes
  addr / list_filter)
- `lore_author_profile` — unchanged; returns aggregates, not a row
  list, so pagination doesn't apply.

`CursorPayload.last_seen_score` widened from `f32` → `f64` so
nanosecond dates round-trip exactly. `RowsResponse` and
`ActivityResponse` and `AuthorFootprintResponse` all carry an
optional `next_cursor` so every paginated shape uses the same
envelope.

**v0.2.0 sync pipeline.** The legacy `grok-pull` (Python grokmirror)
+ separate `kernel-lore-ingest` two-process chain is replaced by a
single `kernel-lore-sync` Rust binary that holds the writer lock
across manifest fetch → gix smart-HTTP fetch → ingest → tid rebuild
→ generation bump → per-tier markers → manifest cache save. Ships
as `klmcp-sync.{service,timer}`; the old three-unit chain is marked
LEGACY but still works. Exit codes (0/2/3) distinguish success /
partial-shard failure / manifest unreachable for systemd alerting.

**Four perf wins on the primary hot paths.**
- `patch_search` cross-list: 9.8 s → 360 ms warm (27×). Parallel
  trigram segment walk with an atomic candidate cap, per-segment
  reader cache, date-sorted + parallel confirm that early-exits at
  `limit`.
- `activity(file=…, list=None)` cross-list: full Parquet scan →
  5–44 ms. New `over_touched_file` SQLite side table populated at
  insert time; 15 M row backfill on lore scale.
- `eq('signed_off_by', …)` (+ reviewed_by/acked_by/tested_by/co_
  developed_by/reported_by): full ddd-blob scan → ≤0.15 ms. New
  `over_trailer_email(kind, email, mid, list)` side table; 12 M
  row backfill.
- `eq('subject_normalized', …)`: ddd-blob scan → 10 ms via a
  promoted column on `over`, populated in-place on existing DBs
  via the new `backfill_subject_normalized` helper.
- `eq('body_sha256' | 'commit_oid', …)`: timeout → <0.1 ms via
  partial indexes on existing columns.

**MCP surface expansion.**
- `stats://coverage` resource + `lore_corpus_stats` tool — closes
  the "what IS in here" transparency gap. Per-list row counts +
  date windows, tier drift status, 30 s in-process cache keyed on
  `(data_dir, generation)` with automatic invalidation on ingest.
- `lore_author_footprint` — every lore message mentioning an
  address (authored + trailer mentions + BM25 body match).
  Complements `lore_author_profile`'s narrower authored-only
  surface.
- HMAC-signed pagination cursors wired through `lore_search`.
  Query-scoped `query_hash` prevents cross-query replay; tampering
  surfaces as `invalid_argument` rather than silent acceptance.
- Git-sidecar wiring into `lore_stable_backport_status` + `lore_
  thread_state`. When `kernel-lore-build-git-sidecar` has been run
  against `linux-stable*` / mainline trees, both tools upgrade
  from pure-lore heuristic to authoritative git-history answers,
  with a `backend` discriminator on every response so callers can
  weight confidence.

**Production hardening.**
- Per-cost-class concurrency caps (`cost_class.py`) with a
  structured `rate_limited` error shape; per-class asyncio
  Semaphore (cheap=1024 / moderate=32 / expensive=4).
- Thread-local `Deadline` wiring through Rust scan paths so
  adversarial queries terminate at the `asyncio.wait_for` boundary
  instead of wedging the thread pool.
- `KLMCP_DISABLE_OVER` env var + `Reader::new_no_over` constructor
  for safer parity testing — replaces the old rename-the-file
  protocol that once corrupted a live deploy.
- `include_mentions=True` on `lore_author_profile` now requires
  `list_filter` or `since_unix_ns` — prevents unbounded trailer
  scans on anonymous multi-tenant instances.
- Compressed store moved from NFS to local NVMe (F1 in the
  over.db follow-ups doc).

**Primitives layer.**
- `_core.sign_cursor` / `_core.verify_cursor` PyO3 bindings.
- `_core.git_sidecar_find_sha` / `_core.git_sidecar_find_by_
  subject_author` / `_core.git_sidecar_repos` for tool-layer access
  to the git-sidecar tier.
- `_core.backfill_*` family (subject_normalized, trailer_emails,
  touched_files) for in-place migration of existing over.db files.

### Changed

- `ingest_shard_with_bm25` generation-bump gate corrected: was
  firing whenever `shared_bm25.is_none()`, which caused the new
  multi-shard sync/ingest binaries (`skip_bm25=true`, no shared
  BM25) to double-bump per run. Now gated on
  `shared_bm25.is_some() || skip_bm25`, so callers that orchestrate
  BM25/tid/gen themselves don't get an intermediate bump.
- `kernel_prose` analyzer, trigram tier, tokenizer fingerprint —
  all unchanged; on-disk format stable across this release.

### Deprecated

- Legacy `klmcp-grokmirror.{service,timer}` +
  `klmcp-ingest.{path,service}` systemd units. Marked LEGACY in
  their unit descriptions; removal scheduled for v0.3.0.
- `KLMCP_GROKMIRROR_INTERVAL_SECONDS` and
  `KLMCP_INGEST_DEBOUNCE_SECONDS` env vars. Replaced by the
  systemd timer `OnUnitActiveSec` + the writer-lock flock (no
  debounce needed when one binary holds the lock end-to-end).

## [0.1.0] — 2026-04-15

Inaugural public release. Anonymous read-only MCP server over
`lore.kernel.org` for Claude Code / Codex / Cursor / Zed agents.

### Added

**Ingest pipeline (Rust core via PyO3 0.28 abi3):**
- Incremental public-inbox v2 walker via `gix` with rayon
  fan-out; dangling-OID full-rewalk fallback.
- mail-parser + full_encoding decode; prose/patch split at first
  `^diff --git`; trailer extraction (`Fixes:`, `Reviewed-by:`,
  `Acked-by:`, `Tested-by:`, `Cc: stable`, `Signed-off-by:`,
  `Co-developed-by:`, `Reported-by:`, `Link:`, `Closes:`).
- Zstd-compressed raw store (per-list, segment-based) as source
  of truth.
- Four-tier index: metadata Parquet (Arrow/Parquet) and the
  derived SQLite `over.db` point-lookup tier (public-inbox
  pattern; rebuilds from Parquet via `kernel-lore-build-over` in
  ~30 minutes for 17.6M rows), trigram (`fst` + `roaring`), BM25
  (`tantivy` 0.26, stemmer deliberately disabled). Parquet,
  trigram, and BM25 all rebuild from the compressed store alone.
- Optional embedding tier (HNSW via `instant-distance`) built
  off a fastembed model via `kernel-lore-embed`.
- Single-writer `flock` on `state/writer.lock`; atomic
  tempfile+rename for every state file so crashes never tear.

**MCP surface (FastMCP 3.2, Streamable HTTP + stdio):**
- 19 tools — `lore_search`, `lore_activity`, `lore_message`,
  `lore_expand_citation`, `lore_series_timeline`,
  `lore_patch_search`, `lore_thread`, `lore_patch`,
  `lore_patch_diff`, `lore_explain_patch`, plus 7 low-level
  primitives and 2 embedding tools and 3 sampling-backed tools
  (`lore_summarize_thread`, `lore_classify_patch`,
  `lore_explain_review_status`) with extractive fallbacks.
- 5 RFC-6570 templated resources: `lore://message/{mid}`,
  `lore://thread/{tid}`, `lore://patch/{mid}`,
  `lore://maintainer/{path}` (stub), `lore://patchwork/{msg_id}`
  (stub).
- 5 server-provided prompts exposed as `/mcp__kernel-lore__*`
  slash commands.
- `blind-spots://coverage` honest-coverage resource.
- Populated KWIC snippets on every hit (offset + length +
  sha256 + text); HMAC-signed opaque pagination cursors
  designed (wire-up in a later release).
- Structured `LoreError` envelope with difflib `did_you_mean`
  recovery on enum errors.
- `response_format: "concise" | "detailed"` knob on the
  high-volume tools so agents can cap tokens.
- Full tool annotation quad (`readOnlyHint`, `destructiveHint`,
  `idempotentHint`, `openWorldHint`) + per-tool `title` on
  every tool; `Cost: <class> — expected p95 N ms` line in
  every description.

**Observability + ops:**
- `/status` reports `generation`, `last_ingest_utc`,
  `last_ingest_age_seconds`, `configured_interval_seconds`,
  `freshness_ok`, per-list shards.
- `/metrics` Prometheus gauges: `kernel_lore_mcp_index_generation`,
  `_last_ingest_age_seconds`, `_configured_interval_seconds`,
  `_freshness_ok`; `_tool_calls_total` counter,
  `_tool_latency_seconds` histogram.
- `kernel-lore-mcp status --data-dir <path>` subcommand prints
  the same JSON without booting HTTP.
- `scripts/klmcp-doctor.sh` — 9-check end-to-end sanity test
  (no network, no API keys).
- `scripts/agentic_smoke.sh` — drives the server over stdio from
  real `claude --print` + `codex exec` CLIs (hits real APIs)
  plus a `local` mode that probes the MCP surface with zero API
  cost.
- Full systemd unit set (grokmirror + ingest + mcp services,
  timer, path-trigger debounce) with sandboxing + resource caps.
- Starter `grokmirror-personal.conf` scopes the first sync to 5
  subsystem lists (~1.5 GB) for laptop users.

**Policy + docs:**
- **No authentication, ever.** No API keys, no OAuth, no
  bearer tokens, no login flow. Every deployment — local,
  hosted, every instance between — is anonymous read-only.
- **5-minute grokmirror cadence** as the default policy, with
  documented cost analysis (~20 GB/month egress from kernel.org,
  <0.2% of one vCPU, <0.2% of lore's monthly egress).
- Fanout-to-one framing: every agent pointed at kernel-lore-mcp
  is one fewer scraping lore directly, so adoption
  monotonically reduces load on kernel infrastructure.
- Operator runbook with separate local-dev and hosted-deploy
  sections.
- Client-config doc with copy-paste snippets for Claude Code,
  Codex, Cursor, Zed — all stdio.
- `docs/demos/first-session.md` — 10 concrete queries covering
  every shipped surface.

### Verified

- 125 Python + 65 Rust tests pass; local MCP probe green
  (6/6 tools, 5/5 resource templates, 5/5 prompts).
- HTTP transport round-trips real MCP + /status + /metrics
  via subprocess test.
- `claude --print` + `codex exec` drive the stdio MCP path
  against the real Anthropic / OpenAI APIs every commit via
  `scripts/agentic_smoke.sh`.
- grokmirror 2.0.12 config verified against live
  `lore.kernel.org/manifest.js.gz` (390 shards).

### Known gaps

- Cursor support for resource templates requires Cursor 1.6+.
- `lore://maintainer/{path}` + `lore://patchwork/{msg_id}` ship
  stubs; real data lands with Phase 18A / 19A of
  [`docs/plans/2026-04-14-best-in-class-kernel-mcp.md`](./docs/plans/2026-04-14-best-in-class-kernel-mcp.md).
- HMAC-signed pagination cursors are built at the router layer
  but not wired through every tool response yet — Phase 13c.

### Scorecard

[MCP best-in-class scorecard](./docs/research/2026-04-14-best-in-class-mcp-survey.md):
~24/36 at 0.1.0, up from 9.5/36 at the start of the phase work.
Target for 0.2.0: ≥32/36.
