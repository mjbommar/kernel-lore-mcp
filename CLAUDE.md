# kernel-lore-mcp — project state

## Standards

Before touching Python code: read [`docs/standards/python/index.md`](./docs/standards/python/index.md)
and the relevant guide. Before touching Rust code: read
[`docs/standards/rust/index.md`](./docs/standards/rust/index.md) and
the relevant guide. When these standards disagree with this
CLAUDE.md, CLAUDE.md wins — it's the project-specific contract.

## Original goal

A public MCP server that makes lore.kernel.org (all kernel mailing
lists) searchable by LLM-backed developer tools — Claude Code,
Codex, Cursor, and anything else that speaks MCP. Target user is a
kernel contributor or security researcher who wants structured,
low-latency queries over every patch, review thread, and bug
report on every kernel list without living inside `lei`.

This is infrastructure, not a product. Be conservative. Be correct.
Do not over-engineer. Do not under-engineer. Full design rationale
lives in `docs/architecture/`. Execution contract in `TODO.md`.

## Stack (April 2026, pinned on purpose)

| Component | Version | Notes |
|---|---|---|
| Rust toolchain | stable 1.85 (edition 2024) | pinned in `rust-toolchain.toml` |
| PyO3 | 0.28.3 | `Python::detach` / `Python::attach` are the CURRENT names (renamed from `allow_threads` / `with_gil` in PRs #5209 #5221, shipped in 0.28). Do not write `allow_threads` in new code. |
| maturin | 1.13.1 | build backend |
| Python | 3.12 minimum (abi3 floor), 3.14 preferred. Free-threaded `python3.14t` requires `--no-default-features` (abi3 incompatible until PEP 803 "abi3t" lands). |
| tantivy | 0.26.0 | stemming gated behind `stemmer` feature — NEVER enabled |
| tantivy-py | NOT USED | we bind tantivy ourselves in the PyO3 module |
| gix (gitoxide) | 0.81.0 | features: `max-performance-safe`, `revision`, `parallel`. NO `blocking-network-client` (grokmirror fetches) |
| mail-parser | 0.11 | `full_encoding` feature (legacy charsets) |
| roaring | 0.11 | posting lists (trigram tier) |
| fst | 0.4 | term dictionary (trigram tier) |
| regex-automata | 0.4 | DFA-only regex — safe for untrusted input |
| zstd | 0.13 | compressed raw store (dictionary-trained per list) |
| arrow | 58 | metadata tier (Parquet on disk) |
| parquet | 58 | ditto; `zstd` + `arrow` + `async` features |
| fastmcp | 3.2.4 | MCP framework. Streamable HTTP only; NOT SSE. |
| mcp (low-level SDK) | 1.27 (explicit dep) | types only; serving via FastMCP |

Any bump to these pins is a project decision, not a casual
`cargo update` / `uv lock --upgrade`. Log the reason in a commit
message.

## Canonical project layout (uv init --build-backend maturin)

```
kernel-lore-mcp/
├── CLAUDE.md                 # you are here — authoritative proscriptions
├── TODO.md                   # review-driven execution contract
├── README.md                 # public pitch, links to docs/
├── LICENSE                   # MIT
├── rust-toolchain.toml       # pinned Rust toolchain
├── pyproject.toml            # maturin build backend, uv deps/groups
├── Cargo.toml                # Rust crate (cdylib + rlib) + reindex bin
├── src/                      # Rust + Python mixed (maturin convention)
│   ├── lib.rs                # #[pymodule] root
│   ├── error.rs              # Error + From<_> for PyErr
│   ├── state.rs              # last_indexed_oid, generation, writer lockfile
│   ├── schema.rs             # shared Arrow + tantivy schemas
│   ├── store.rs              # compressed raw store
│   ├── metadata.rs           # Arrow/Parquet columnar tier
│   ├── trigram.rs            # fst + roaring trigram tier
│   ├── bm25.rs               # tantivy tier
│   ├── ingest.rs             # gix shard walk + extract + dispatch
│   ├── router.rs             # query grammar + tier dispatch + merge
│   ├── bin/
│   │   └── reindex.rs        # rebuild indices from compressed store
│   └── kernel_lore_mcp/      # Python package
│       ├── __init__.py       # lazy _core import
│       ├── __main__.py       # entry point; default bind 127.0.0.1
│       ├── server.py         # FastMCP app; explicit tool registration
│       ├── config.py         # pydantic-settings
│       ├── models.py         # pydantic response models (outputSchema)
│       ├── logging_.py       # structlog; stdio mode -> stderr only
│       ├── tools/            # one file per MCP tool
│       ├── resources/        # blind_spots://coverage etc.
│       ├── routes/           # /status, /metrics via @mcp.custom_route
│       └── _core.pyi         # type stubs for the Rust extension
├── tests/
│   └── python/               # pytest with in-process fastmcp.Client
├── docs/
│   ├── architecture/         # design rationale
│   ├── ingestion/            # how data comes in
│   ├── indexing/             # the three tiers, tokenizer spec
│   ├── mcp/                  # tool schemas, routing, transport, clients
│   ├── ops/                  # EC2, cost, freshness, deploy, security
│   └── research/             # dated investigations
├── scripts/                  # one-offs (grokmirror conf)
└── .github/workflows/        # CI
```

**Keep this organized.** Every doc has a single home. If you can't
decide where something goes, update the taxonomy — don't scatter.

## Three-tier index architecture

Corpus is heterogeneous. One index is wrong. See
`docs/architecture/three-tier-index.md`.

1. **Metadata tier** — Arrow/Parquet. Structured fields: message_id,
   list, from, subject (raw + normalized + tags), date, in_reply_to,
   references[], tid (thread id, precomputed at ingest),
   touched_files[], touched_functions[], series_version,
   series_index, is_cover_letter, has_patch, patch_stats, trailers
   (signed_off_by[], reviewed_by[], acked_by[], tested_by[],
   co_developed_by[], reported_by[], fixes[], link[], closes[]),
   cross_posted_to[], body_offset, body_length, body_sha256,
   schema_version. ~3 GB for all of lore.
2. **Trigram tier** — custom; `fst` + `roaring`. Indexes patch/diff
   content. Regex + substring over code. Confirm-with-real-regex
   by decompressing the patch body from the compressed store
   (candidates capped; see `src/trigram.rs`). ~20 GB.
3. **BM25 tier** — tantivy with our `kernel_prose` analyzer.
   Indexes prose body (message minus patch) + subject. Positions
   OFF (`IndexRecordOption::WithFreqs`). Phrase queries on prose
   are REJECTED, not silently degraded. ~10 GB.

Rebuildability contract: the compressed raw store is the source
of truth. All three tiers can be rebuilt from it without
refetching lore. `reindex` binary does this.

## Tokenizer proscriptions

Non-negotiable. See `docs/indexing/tokenizer-spec.md`.

1. **No stemming, no stopwords, no asciifolding, no typo tolerance.**
   tantivy 0.26 puts stemming behind a feature flag — leave off.
2. **Strip quoted reply prefixes (`^> `) and signature blocks
   (after `-- \n`) before indexing.**
3. **Split patch off at ingest.** First `^diff --git` line starts
   the patch; prose goes to BM25, patch to trigram. **Never mix.**
4. **Preserve kernel identifiers whole AND emit subtokens.** For
   `vector_mmsg_rx`, emit the whole identifier plus `vector`,
   `mmsg`, `rx` subtokens at `position_inc=0`. **Preserve the
   leading-underscore signal** (`__skb_unlink` stays distinct from
   `skb_unlink`).
5. **Atomic tokens** for email addresses, Message-IDs, commit SHAs,
   CVE IDs — dedicated `raw` analyzer or STRING field.
6. **Subject-line prefixes (`[PATCH ...]`, `Re:`, `Fwd:`) stripped
   before BM25**; raw subject stored for display.
7. **Subject tags** — `[RFC]`, `[RFT]`, `[GIT PULL]`, `[ANNOUNCE]`,
   `[RESEND]`, `[PATCH vN]`, `N/M` — extracted to `subject_tags[]`
   column, not discarded.

## Ingestion pipeline

- `grokmirror` pulls lore shards on a 10-minute cron.
- Ingestion runs as a **separate systemd unit** (`klmcp-ingest`),
  NOT in-process with the MCP server. It holds the sole
  `tantivy::IndexWriter` + trigram builder + store appender.
- Walk via `gix::ThreadSafeRepository` with one rayon task per
  shard (never within a shard — packfile cache locality).
  Incremental via `rev_walk([head]).with_hidden([last_oid])` with
  full-rewalk fallback if `last_oid` is dangling (shard repack).
- Per commit: extract fields in `docs/ingestion/mbox-parsing.md`
  and trailers listed above; propagate `touched_files` from
  sibling `1..N/N` patches to cover-letter via `tid`.
- After shard done: atomic rename of state file.
- After all shards done: writer commit, Parquet finalize, trigram
  segment rename, bump `state::generation` counter.

## Reader reload discipline

- `tantivy::ReloadPolicy::Manual`.
- Every query-request entry `stat()`s the generation file; if the
  u64 advanced, `reader.reload()?` runs before the query.
- Same file tells multi-worker uvicorn deployments to stay coherent.

## MCP server contract

See `docs/mcp/` for full details.

- Transport: **Streamable HTTP only** (SSE deprecated Apr 1 2026).
  stdio for local dev.
- Default bind `127.0.0.1`; set `KLMCP_BIND=0.0.0.0` explicitly for
  public deploy.
- **stdio mode**: all logs to stderr. Never write a byte to stdout
  outside the MCP framing — corrupts the protocol.
- Tools v1: `lore_search`, `lore_thread`, `lore_patch`,
  `lore_activity`, `lore_message`, `lore_series_versions`,
  `lore_patch_diff`. All read-only. All annotate `readOnlyHint: true`.
- **Tools return pydantic `BaseModel`**, not `dict`. FastMCP
  auto-derives `outputSchema` + emits `structuredContent`.
- **Every hit carries**: `message_id`, `cite_key`, `from_addr`
  (always), `lore_url`, `subject_tags[]`, `is_cover_letter`,
  `series_version`, `series_index`, `patch_stats` (if `has_patch`),
  `snippet{offset,length,sha256,text}`, `tier_provenance[]`,
  `is_exact_match`, `cross_posted_to[]`.
- **Pagination**: opaque **HMAC-signed** cursor (not a plain b64
  tuple — rejects tampered cursors). Key from env
  `KLMCP_CURSOR_KEY`.
- **No phrase queries on prose body** in v1 (no positions). Router
  returns `Error::QueryParse` with an actionable message, never
  silent degradation.
- **Regex queries MUST compile to DFA** via `regex-automata`. No
  backrefs, no catastrophic patterns. Rejected with
  `Error::RegexComplexity`.
- **`blind_spots` is an MCP resource** (`blind_spots://coverage`),
  NOT a per-response payload. Per-response tax is a token sink.
- **Default `rt:` is 5 years**; always echoed in
  `default_applied: ["rt:5y"]` so LLMs know.
- **`/status`** via `@mcp.custom_route`; cached 30s. **`/metrics`**
  via `prometheus_client` (localhost by default).

## Query grammar (lei-compatible subset, expanded)

Full list in `docs/mcp/query-routing.md`. Key operators beyond the
v0 sketch:

- Metadata: `tc:` (to-or-cc combined), `reviewed-by:`, `acked-by:`,
  `tested-by:`, `signed-off-by:`, `co-developed-by:`, `fixes:<sha>`
  (reverse-lookup patches mentioning this SHA), `closes:`, `link:`,
  `patchid:`, `applied:`, `cherry:`, `tag:<RFC|RFT|...>`,
  `trailer:<name>:<value>`.
- Trigram: `dfpre:`, `dfpost:`, `dfa:` (either side), `dfb:` (body
  incl. context), `dfctx:` (context only), `/<regex>/`.
- BM25: `b:`, `nq:` (body minus quoted reply).

## What NOT to use

Evaluated and rejected; see `docs/research/`:

- **Sonic, Toshi, Meilisearch, Quickwit, Bluge/Bleve** — wrong shape
  or wrong language for embedded Rust library use.
- **tantivy stemmer** — off by design. Never enable.
- **git2-rs / libgit2** — not `Sync`; gix wins on linear history.
- **vendored `mcp.server.fastmcp`** — diverged from standalone
  `fastmcp`. Use standalone.
- **SSE transport** — deprecated Apr 1 2026.
- **Keeping full git shards** after ingest — compressed store is
  source of truth.
- **`allow_threads` / `with_gil`** in new PyO3 code — renamed to
  `detach` / `attach` in 0.28.
- **FastAPI REST surface** — deferred past v1 unless demand lands.
  Don't mount it.

## Operational contract

- EC2 single-box deploy, **`r7g.xlarge`** Graviton (32 GB RAM —
  NOT `c7g.xlarge` 8 GB, which won't hold the hot set), or
  `r7i.xlarge` on Intel.
- **gp3 16000 IOPS / 1000 MB/s** (6000/250 would queue cold BM25).
- Ingestion is a separate systemd unit from serving.
- Blue/green deploy via dual systemd units + nginx upstream swap.
- RPO = hours (re-grok-pull from lore). RTO = ~30 min
  (snapshot-restore cold). State this explicitly in responses if
  ever relevant.
- `robots.txt` + `LEGAL.md` posture for public re-hosting of
  author names/emails. See `docs/ops/` and LEGAL doc.

## Known blind spots

Surfaced via the `blind_spots://coverage` MCP resource (once, not
per-response):

- Private `security@kernel.org` queue.
- Distro vendor backports.
- Syzbot pre-public findings.
- ZDI / research-shop internal pipelines.
- CVE Project in-flight embargoes.
- Off-list discussion (IRC, private email, calls).
- Lore trails vger by 1–5 minutes; our ingest adds 10–20 more.

## Research-novelty discipline (inherited from parent project)

This server makes novelty-checking *easier*, but the discipline is
unchanged: don't fish where everyone else is fishing. The
`lore_activity` tool must make it trivial to ask "who has touched
this file in the last 6 months, grouped by series, with trailers,
and mapped against MAINTAINERS membership." That's what the
metadata tier is for.

## Session-specific guidance

- Prefer editing existing files over creating new ones.
- Never add speculative features. This project gets misused as a
  sandbox because it touches many interesting topics.
- Do not run `grok-pull` from developer machines by default — the
  deploy box does that. Ingestion tests use synthetic fixtures in
  `tests/python/fixtures/`.
- Do not commit compressed stores, indices, or fetched lore data.
  `.gitignore` catches `data/`, `*.tantivy`, `*.zst`, etc.
- Do not write comments explaining WHAT code does. Identifiers
  already tell you that. Comments explain WHY — non-obvious
  constraints, workarounds for specific bugs.
- Do not add a stemmer. Do not add SSE transport. Do not add
  git2-rs. Do not add FastAPI for v1. Do not hold the GIL across
  heavy Rust calls. Do not write logs to stdout in stdio mode.
  Do not return bare dicts from MCP tools. Do not use the
  side-effect-import tool registration pattern. These are decided.
