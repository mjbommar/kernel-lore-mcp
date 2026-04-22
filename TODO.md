# TODO — kernel-lore-mcp

This is the execution contract. Every item here is traceable to
either an original design goal (`CLAUDE.md`) or a reviewer finding
from the April 14 2026 review pass.

Status markers:
- `[ ]` — open
- `[x]` — done
- `[DEFER]` — decided to defer past v1; must have a `--> docs/...`
  pointer explaining why

## Current release track (2026-04-22)

`v0.3.0` shipped on 2026-04-21. The active patch line is
`v0.3.1`: sync-under-load hardening, operator visibility, and release
cleanup after the first same-box hosted soak on `server6`.

### 0.3.1 target — sync-under-load visibility + safer inline BM25

- [x] `kernel-lore-sync` writes live machine-readable progress under
  `state/sync.json`, and `/status` + `kernel-lore-mcp status` surface
  `writer_lock_present`, `sync_active`, and the current sync stage.
- [x] Sync logs the post-ingest work explicitly:
  `bm25_commit`, `tid_rebuild`, `path_vocab_rebuild`,
  `generation_bump`, and `save_manifest` are no longer silent gaps.
- [x] Inline BM25 is safer-by-default and operator-tunable:
  default BM25 writer thread count is conservative, and env knobs exist
  for thread count + memory budget.
- [x] `rate_limited` / `query_timeout` retry hints are load-aware and
  rise when a live writer-heavy sync stage is active.
- [x] The async + multiprocess soak harness used against `server6`
  lives in-tree and is referenced by the launch docs.

### 0.2.3 carry-forward — land these in `v0.3.0` first

- [x] `kernel-lore-sync` self-heals poisoned shard repos: detect
  zero-ref / unopenable local shard repos, delete them, and
  reclone automatically. `kernel-lore-doctor --heal` stays as the
  explicit maintenance tool, not the primary recovery path.
- [x] `lore_regex` public-hosted posture: disable/gate it in hosted
  mode or add enough admission control that the full-corpus default
  path never just burns 5 s and returns `query_timeout`.
- [x] `_cached_corpus_stats` generation cache + warm path so warm
  calls stay well under the wall-clock cap on the full corpus.
- [x] `/metrics` must record `rate_limited` and every other
  non-`ok` status consistently, including overload that rejects
  before the tool body runs.
- [x] Add end-to-end request latency + queue-wait histograms so the
  client-side p95 inflation seen under concurrency is visible in
  Prometheus.
- [x] Auto-build `paths/vocab.txt` during sync so
  `lore_path_mentions` works on a fresh healthy hosted box without
  a manual follow-up command.
- [x] Hosted default logging drops `tantivy::*` INFO churn unless
  explicitly enabled; operator logs should show our own progress /
  warning / error lines first.

### 0.3.0 target — ship after the carry-forward hardening line above

- [x] Hosted deployment profile (`local` vs `hosted`, or equivalent)
  that flips the exact runtime defaults we intend to expose on the
  public internet.
- [x] Repeatable adversarial HTTP/MCP load harness in CI: cheap
  flood, moderate over-cap concurrency, expensive-tool saturation,
  and concurrent `/status` responsiveness checks.
- [x] Public-safe `lore_regex` redesign or permanently narrower
  admission control. Current full-corpus behavior is not launchable.
- [x] Reconcile client-observed latency vs server-side tool latency
  before retuning concurrency caps or over.db pool sizes.
- [x] Public-launch checklist against a full-corpus host: generation
  health, shard health, tool surface, metrics, abuse posture, and
  operator-log readability.

## Phase 0 — scaffold correctness (blocks everything else)

- [x] Use `uv init --build-backend maturin` for canonical layout
  (not hand-written). Layout is `src/lib.rs` + `src/kernel_lore_mcp/`.
  Native module name is `_core` per maturin default.
- [x] Restore design content (CLAUDE.md, docs/, LICENSE, .gitignore).
- [ ] Merge design dependencies into generated Cargo.toml.
- [ ] Merge design dependencies into generated pyproject.toml.
- [ ] Add `rust-toolchain.toml` pinning stable 1.85 (required by
  edition 2024 + tantivy 0.26).
- [ ] Add missing Rust modules: `store`, `schema`, `state`, `error`,
  plus existing `ingest`/`metadata`/`trigram`/`bm25`/`router`.
  `src/bin/reindex.rs` binary target.
- [ ] Fix pyo3 doc claim: `Python::detach` / `Python::attach` IS in
  0.28.3 stable (renamed from `allow_threads` / `with_gil` in PRs
  #5209, #5221). Rust reviewer was wrong about 0.29.
- [ ] arrow + parquet bump to 58.x (was 54 — mismatched ecosystem).
- [ ] gix features: drop `blocking-network-client`; add `parallel`;
  keep `revision`, `max-performance-safe`.
- [ ] Release profile: `lto = "thin"`, `codegen-units = 1`,
  `panic = "abort"`, `strip = "symbols"`.
- [ ] Python package under `src/kernel_lore_mcp/` (per uv convention),
  NOT `python/` (our initial guess).
- [ ] Smoke test: `cargo check`, `uv sync`, `uv run maturin develop`,
  `uv run pytest`.

## Phase 1 — correctness fixes from Rust reviewer

- [ ] arrow/parquet 58 + standalone zstd 0.13: force alignment via
  `[patch.crates-io]` or strip one to avoid `zstd-sys` dup link.
  Test with `cargo tree -d`.
- [ ] `abi3-py312` gated behind a default Cargo feature so
  `--no-default-features` builds the non-abi3 wheel required for
  free-threaded 3.14t (abi3 incompatible per PEP 803 pending).
- [ ] `thiserror` → `PyErr` mapping in `src/error.rs`. No
  `anyhow::Error` crosses the PyO3 boundary.
- [ ] `tracing-subscriber` as dev-dep only (Rust tests); Python side
  owns production logging.
- [ ] rayon thread-pool size configurable from Python via
  `kernel_lore_mcp.init(rayon_threads=N)`.
- [ ] `mail-parser` features pinned explicitly (legacy charset
  coverage per `docs/ingestion/mbox-parsing.md`).
- [ ] `tantivy` features: `mmap` only; stemmer feature NEVER added.
- [ ] State-file atomic rename + stale-OID fallback (if
  `find_object(last_oid)` fails due to shard repack, full re-walk
  + dedupe by `message_id`).
- [ ] Schema `src/schema.rs` shared between `metadata`, `bm25`,
  tests. No duplicated field name literals.
- [ ] Tokenizer analyzer-fingerprint sidecar: reject opens where
  the on-disk fingerprint doesn't match the registered analyzer.
- [ ] Reader reload discipline: version-file stat on every request
  entry; `reader.reload()?` if changed.

## Phase 2 — correctness fixes from Python/MCP reviewer

- [ ] **Tools actually registered.** `@mcp.tool` decorator on every
  tool function; `build_server()` imports tool modules or calls
  `mcp.add_tool(fn)` explicitly. Side-effect import pattern is a
  circular-import hazard — switch to explicit registration.
- [ ] `readOnlyHint: True` in every tool's `annotations`.
- [ ] Every tool returns a pydantic `BaseModel` (not bare `dict`)
  so `outputSchema` + `structuredContent` land on the wire.
  Define models in `src/kernel_lore_mcp/models.py`.
- [ ] HMAC-signed opaque cursor. Key from env. Reject tampered
  cursors with `INVALID_PARAMS`.
- [ ] `/status` via `@mcp.custom_route("/status")`. Cached 30s.
- [ ] `/metrics` via `prometheus_client` (localhost-only by default).
- [ ] `blind_spots` as MCP **resource** (`blind_spots://coverage`),
  NOT per-response payload (token tax). Tool responses include
  only `"blind_spots_ref": "blind_spots://coverage"`.
- [ ] Freshness cached 30s with invalidation from ingest.
- [ ] Default bind `127.0.0.1`; env `KLMCP_BIND=0.0.0.0` for public.
- [ ] stdio transport: structlog to stderr only. NEVER stdout.
- [ ] `asyncio.to_thread(_core.router.dispatch, ...)` wrapper so
  Rust calls don't pin the asyncio reactor thread.
- [ ] Unix-socket transport (`--uds PATH`) for container deploys.
- [ ] Drop `fastapi` unless we commit to mounting REST on Starlette.
  For now: MCP-only; REST comes in v1.1 if demand exists.
- [ ] Lazy `_core` import in `__init__.py` (so tests/tooling don't
  hard-require a built wheel).
- [ ] Dev deps: `pytest`, `pytest-asyncio`, `respx`, `freezegun`,
  `ruff`, `mypy`, `maturin`.
- [ ] `tests/python/conftest.py` with in-process
  `fastmcp.Client(build_server())` fixture.
- [ ] Error-mapping middleware: pydantic validation / router
  errors → MCP `isError: true` + actionable message.
- [ ] Rate-limit middleware (slowapi-style) keyed on client host
  when HTTP; bypassed for stdio.
- [ ] Input validation: `query` min_length=1, max_length=512.
- [ ] Remove `--config FILE` plan; env + `.env` only. Add
  `--log-level` only.

## Phase 3 — design corrections from kernel-user reviewer

These are doc/contract changes that shape the ingest + query layer
*before* real code lands. Treat them as requirements, not wishlist.

### Ingest-time fact extraction (expand beyond v0 scope)

- [ ] Extract **trailers** as structured columns:
  `signed_off_by[]`, `reviewed_by[]`, `tested_by[]`, `acked_by[]`,
  `co_developed_by[]`, `reported_by[]`, `fixes[]` (commit SHA + subject),
  `link[]`, `closes[]`.
- [ ] Extract **subject tags** as structured column `subject_tags[]`:
  `RFC`, `RFT`, `GIT PULL`, `ANNOUNCE`, `RESEND`, `PATCH`, `PATCH vN`,
  `N/M` series index + count.
- [ ] Extract **patch_stats**: files_changed, insertions, deletions
  (cheap from `git format-patch` diffstat or on-the-fly count).
- [ ] Extract **is_cover_letter** from series `0/N` numbering.
- [ ] Precompute **thread_id (tid)** at ingest via public-inbox's
  heuristic (In-Reply-To / References + subject-normalized fallback
  within a 30-day window). Store on every row.
- [ ] Propagate **touched_files / touched_functions** from sibling
  `1..N/N` mails to cover-letter via `tid`. A `dfn:` query that
  hits a patch also surfaces its cover.
- [ ] Parse **cover-letter diffstat** (`git format-patch` style) so
  `has_patch=true` for covers that describe a series.
- [ ] Preserve **leading-underscore signal** in identifier tokens
  (`__skb_unlink` keeps its prefix as a distinct token).

### Query grammar (expand lei-compatible subset)

- [ ] Add operators: `dfpre:<term>` (diff minus-side content),
  `dfpost:<term>` (plus-side), `dfa:<term>` (either), `dfb:<term>`
  (either, body match incl. hunk context), `dfctx:<term>` (context
  only), `tc:<term>` (to-or-cc combined), `patchid:<sha>`,
  `reviewed-by:<term>`, `acked-by:<term>`, `tested-by:<term>`,
  `fixes:<sha>` (reverse-lookup patches that mention this SHA),
  `applied:<bool>`, `cherry:<sha>`, `tag:<subj-tag>`,
  `trailer:<name>:<value>` (catchall).
- [ ] `"phrase"` over prose: either narrow positional subindex OR
  reject with clear error. NO silent degradation to conjunction.
- [ ] Regex queries: compile to DFA via `regex-automata`; reject
  anything that requires backrefs. Per-query wall-clock cap 5s.
- [ ] Default `rt:` window is 5 years; include `default_applied:
  ["rt:5y"]` in response so LLM callers know.
- [ ] `list:` default = all; warn in response metadata when
  candidate set > 1M.

### Tools (expand surface)

- [ ] Add `lore_series_versions(message_id_or_subject)` →
  `{v1: mid, v2: mid, ...}` via subject pattern + author match.
- [ ] Add `lore_patch_diff(mid_a, mid_b)` — the single most common
  reviewer task ("what changed between v2 and v3").
- [ ] `lore_activity` output: split by `tid` (one row per series,
  not per mail). Return:
  - `series[]` with `{tid, first_mid, last_mid, version, reviewers[], acked_by[], fixes[], applied: bool|unknown, distinct_authors}`
  - `distinct_authors`: split by MAINTAINERS-entry membership if
    known, by commit-count threshold otherwise.
  - `since` accepts `last-rc` / `this-cycle` in addition to ISO
    and `Nd`.
- [ ] Output contract on every hit: `message_id`, `cite_key`
  (`<list>/<YYYY-MM>/<patch-slug>`), `from_addr` (always), `from_name`,
  `lore_url`, `list`, `date`, `subject`, `subject_tags[]`,
  `is_cover_letter`, `series_version`, `series_index`, `patch_stats`
  (if `has_patch`), `snippet` with `{offset, length, sha256}`,
  `tier_provenance` (which tier hit: metadata | trigram | bm25),
  `is_exact_match` (for trigram post-confirm).
- [ ] `lore_thread` response: keep `prose` and `patch` as separate
  fields per message. Don't re-concatenate what we spent ingestion
  splitting.
- [ ] Cross-list dedup: same `Message-ID` on multiple lists → one
  hit with `cross_posted_to: [list, ...]`.

## Phase 4 — ops fixes from SRE reviewer

- [ ] `docs/ops/ec2-sizing.md`: **c7g.xlarge → r7g.xlarge 32 GB**
  (8 GB won't hold our 25–45 GB hot set). EC2 pricing corrected to
  April 2026 rates.
- [ ] EBS IOPS baseline **bumped to 16000 / 1000 MB/s** — 6000/250
  would queue cold BM25 under load. Provision as gp3 16k IOPS.
- [ ] Ingestion as **separate systemd unit** (`klmcp-ingest.service`)
  from the MCP server (`klmcp-serve.service`). Shared index files
  via filesystem + generation epoch file.
- [ ] Index swap across workers via **`index.generation`** file;
  every request-entry `stat()`; `reader.reload()` on change.
- [ ] Manifest integrity check on each `grok-pull`: HTTP 200 + size
  sanity + "shards present today vs yesterday" alert.
- [ ] Per-tool **response byte ceilings** (`lore_thread` caps at
  5 MB response; threads larger paginate).
- [ ] **TLS/DNS/CDN plan** committed: domain name reserved,
  Let's Encrypt via certbot-managed systemd timer, ACME challenge
  via nginx, HSTS on.
- [ ] **Blue/green** deploy: two systemd units + nginx upstream
  swap + 10-min drain. Document that index rollback is NOT
  required for binary rollback (indices are forward-compatible
  within a schema_version).
- [ ] **RPO/RTO** explicit: RPO = "time to re-grok-pull" = hours
  (not minutes; lore is the source of truth). RTO = snapshot-restore
  ~30 min cold.
- [ ] **Supply chain**: `uv lock --upgrade-package` hygiene;
  Renovate bot; hash-pinned requirements.
- [ ] **Regex DoS**: DFA-only regex (regex-automata), prefix-anchor
  required for unbounded term-dict scans, per-query 5s wall-clock.
- [ ] **SLO**: 99.5% monthly, p95 warm-query <500 ms, p95 cold-BM25
  <3 s. Error budget measured in CloudWatch.
- [ ] **CloudWatch Logs**: 7-day retention, filter at source
  (drop DEBUG in production).
- [ ] **fail2ban** on 429 signal — automates IP blocks.
- [ ] **CloudFront + POST**: either add a body-keyed cache policy
  with vary-on-tool-args hash, or drop CloudFront from v1 and rely
  on single-origin caching.
- [ ] **Scratch space** on separate mount — runaway ingest must not
  fill the index volume.
- [ ] `docs/LEGAL.md` (or README section): Linux Foundation
  mirroring policy reference; contact email for redaction requests;
  log-retention policy for queries containing PII.

## Phase 5 — research doc corrections

- [ ] `docs/research/2026-04-14-pyo3-maturin.md`: `Python::detach`
  confirmed in 0.28.3 stable (not 0.29). Fix the section.
- [ ] `docs/research/2026-04-14-storage-footprint.md`: double-check
  with `lore.kernel.org/manifest.js.gz` periodically; note that
  counts drift.
- [ ] `docs/research/2026-04-14-tantivy.md`: verified stemmer
  feature flag (correct) — no change needed.
- [ ] Add `docs/research/2026-04-14-review-findings.md` archiving
  the four reviewer reports so future sessions see the provenance.

## over.db tier complete (2026-04-17)

Driven by the lore-scale failure mode discovered during the
2026-04-16 full-corpus ingest: `fetch_message` and every `eq()`
predicate were doing full Parquet scans (187 s wall-clock, 36 GB
RSS / OOM on `f:gregkh@linuxfoundation.org`). Plan in
[`docs/plans/2026-04-17-overdb-metadata-tier.md`](./docs/plans/2026-04-17-overdb-metadata-tier.md);
Phase 5 validation in
[`docs/research/2026-04-17-overdb-validation.md`](./docs/research/2026-04-17-overdb-validation.md).

- [x] **Phase 1** — `src/over.rs` SQLite module (public-inbox
  `over.sqlite3` pattern; indexed columns + zstd-msgpack `ddd`
  blob; WAL pragmas; bulk-load mode).
- [x] **Phase 2** — `kernel-lore-build-over` binary
  (`src/bin/build_over.rs`); deferred-index strategy; tempfile +
  atomic rename; ~30 min for 17.6M rows.
- [x] **Phase 3** — Reader wired through over.db with graceful
  Parquet fallback. Five paths converted: `fetch_message`,
  `eq`, `prose_search_filtered`, `patch_search`, `all_rows`.
- [x] **Phase 4** — Incremental ingest writes to over.db when
  `--with-over` is set (auto-detected if file exists).
  `INSERT OR REPLACE` keyed on `(message_id, list)` for
  idempotency. `tid` column stays NULL until `rebuild_tid`
  wires through (deviation from plan; tracked as follow-up).
- [x] **Phase 5** — Validation closed. fetch_message 0.06 ms p50
  (was 187 s); eq from_addr 3.08 ms p50 (was 587 ms after first
  build, 5.4 s p95); prose_search 23.5 ms p50 (was 170 s); peak
  RSS 151 MB (was 1754 MB). All plan targets met after one round
  of tuning (composite `over_from_date` index + lowered
  `mmap_size` / `cache_size`).
- [x] **Phase 6** — Documentation + rollout
  ([`docs/architecture/four-tier-index.md`](./docs/architecture/four-tier-index.md),
  [`docs/architecture/over-db.md`](./docs/architecture/over-db.md),
  CLAUDE.md "Four-tier index architecture", runbook §10A,
  corpus-coverage disk footprint).

**Follow-ups not blocking:**

- Cross-post collapse: validation §5e found zero cross-posted
  message_ids in the 17.6M corpus, suggesting upstream
  `Reader::scan` dedup flattens by message_id alone before rows
  reach over.db. Schema supports the multi-row representation;
  fix is in the dedup pass.
- `eq()` for non-indexed `EqField` variants (signed_off_by,
  touched_files, …) still falls through to a sequential
  `ddd`-decode scan inside over.db. Promote to dedicated
  columns / side-tables if they become hot.
- `tid` column population (Phase 4 deviation).

**Retro:** the failure mode and the fix were both well-aligned
with prior art (public-inbox runs essentially this layout in
production at lore.kernel.org scale). The validation-protocol
fragility — renaming `over.db` to test the Parquet fallback
crashed the subagent and broke the live system mid-test — is the
one process lesson worth carrying forward; future parity tests
should compare against a separately-built second instance, not
filesystem renames.

## Phase 1 complete (2026-04-14)

Landed in commit `2b54d7c`:

- Real ingest pipeline: `src/ingest.rs` walks a public-inbox
  shard via `gix` with per-shard rayon tasks.
- `src/parse.rs` parses RFC822 via `mail-parser`, strips quoted
  replies + signatures, splits prose from patch at the first
  `^diff --git`.
- Trailer extraction (`signed_off_by`, `reviewed_by`, `acked_by`,
  `tested_by`, `co_developed_by`, `reported_by`, `fixes`, `link`,
  `closes`, `cc_stable`), subject-tag extraction
  (`[PATCH vN]`, `N/M`, `RFC`, `RFT`, `GIT PULL`, `ANNOUNCE`,
  `RESEND`), and `is_cover_letter` detection.
- `touched_files` from patch hunks; patch_stats
  (`files_changed`, `insertions`, `deletions`).
- `src/store.rs` compressed raw store (zstd, append-only).
- `src/metadata.rs` Parquet writer for the metadata tier.
- `src/schema.rs` canonical column-name constants + Arrow
  schema, `SCHEMA_VERSION=1`.
- `src/state.rs` atomic-rename state file with generation
  counter and stale-OID fallback.
- `src/error.rs` single error enum, `From<_> for PyErr`.
- Synthetic shard fixtures + Python integration tests under
  `tests/python/`.

Still open in this codebase: query router wiring
(`src/router.rs` is a stub), trigram tier (`src/trigram.rs`
stub), BM25 tier (`src/bm25.rs` stub), the MCP tool surface
wired to real data. Those are Phase 2.

## Public Instance Track

Independent of Phase 1–6. Gates the hosted instance, not the
code. Runs in parallel. Release-gated by the current release-track
section above.

- [ ] Pin `KLMCP_MODE` in `kernel_lore_mcp.config`
  (`local` default, `hosted` opt-in). Same binary, runtime
  gate. See `docs/architecture/deployment-modes.md`.
- [ ] Embargo quarantine per `SECURITY.md` (72h hold for
  `Fixes:`-to-<7d-old-SHA` or `CVE-YYYY-\d+` subject).
- [ ] structlog processor that scrubs query strings + tool
  args before serialization (per `LEGAL.md`). Nginx access
  log format with path + status + timing only.
- [ ] Redaction-request email workflow (72h SLA on hosted).
  Re-ingest honors lore-side redactions on the grokmirror
  cadence (already true by construction).
- [ ] API-key gate on `lore_activity` file granularity
  (free signup, no approval queue). Coarser granularity
  stays anonymous.
- [ ] Snapshot-bundle publication (weekly) so new self-hosters
  bootstrap from us, not lore. Channel TBD (likely S3 +
  IPFS). See `docs/architecture/reciprocity.md`.
- [ ] TLS + nginx + certbot + HSTS (per Phase 4).
- [ ] Abuse response: fail2ban on 429, named User-Agent on
  any outbound request, honor Retry-After.

Cross-link to Phase 6 "Reciprocity infrastructure": the
snapshot-bundle piece is shared work.

## Phase 6 — deferred to v1.1 / v2

- [DEFER] REST surface via FastAPI alongside MCP → depends on
  demand. --> `docs/mcp/transport-auth.md`.
- [DEFER] Semantic / vector search. --> `docs/architecture/overview.md`.
- [DEFER] OAuth 2.1 + PKCE for user-scoped flows. --> `docs/mcp/transport-auth.md`.
- [DEFER] MAINTAINERS-file-driven author classification in
  `lore_activity` — needs a mainline tree mirror, not just lore.
- [DEFER] Narrow positional subindex for phrase-over-prose. Ship
  v1 with explicit "no phrase on prose" behaviour.
- [DEFER] Compaction pass to drop tombstoned rows from Parquet.
- [DEFER] Multi-AZ / read replicas. Single-box v1; trigger is SLO
  breach > 2 months OR first paying customer.
- [DEFER] Free-threaded `python3.14t` wheel job in CI. Gate behind
  PEP 803 abi3t landing.
- [DEFER] MCP resource for the full MAINTAINERS snapshot so LLM
  can reason about ownership.

### Reciprocity infrastructure

Not deferred in spirit — just deferred in code, because it lands
once the tiers are real. See
`docs/architecture/reciprocity.md`.

- [ ] Snapshot-bundle builder: pack the compressed store + all
  index tiers + `SCHEMA_VERSION` + state-file into a single
  bundle. Publish weekly from our infrastructure so new
  self-hosters bootstrap from us, not from lore.
- [ ] Snapshot-bundle fetcher in the ingest pipeline: if a
  local shard tree is absent, pull the latest bundle before
  running `grok-pull`.
- [ ] Named User-Agent
  (`kernel-lore-mcp/<version> (+https://github.com/mjbommar/kernel-lore-mcp)`)
  on every outbound request. Honor `Retry-After`.
- [ ] Prefer tier-1 mirror (`erol.kernel.org`) over lore
  direct in the default grokmirror config under `scripts/`.

## Phase 0+ additions after review of KAOS standards

- [x] Adopt KAOS Python standards as `docs/standards/python/` (index,
  language, uv, code-quality, testing, naming, pyo3-maturin,
  data-structures, git + design/*/libraries/*/checklists/*).
- [x] Create parallel `docs/standards/rust/` (index, language,
  cargo, code-quality, testing, naming, ffi, unsafe +
  design/*/libraries/*/checklists/*).
- [x] Switch pyproject.toml from mypy to `ty` (Astral).
- [x] Reference standards from `CLAUDE.md`.

## Ground rules for execution

1. Complete Phase 0 before touching Phase 1+.
2. Each phase's items can parallelize among themselves. Phases
   themselves gate.
3. Every change to a proscription (`CLAUDE.md`) is a commit on its
   own, not buried in a feature commit.
4. Every `[DEFER]` keeps its `--> docs/...` pointer when it moves
   off this list into the archived `docs/research/` record.
5. `TODO.md` is authoritative; sub-agents and future sessions work
   from it.
