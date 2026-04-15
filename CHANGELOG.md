# Changelog

All notable changes to `kernel-lore-mcp` land here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [SemVer](https://semver.org/).

Unreleased changes accumulate under an `## [Unreleased]` heading;
release tags move them into a dated section. Release process in
[`CONTRIBUTING.md`](./CONTRIBUTING.md).

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
- Three-tier index rebuilds from the store alone:
  metadata (Arrow/Parquet), trigram (`fst` + `roaring`), BM25
  (`tantivy` 0.26, stemmer deliberately disabled).
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
