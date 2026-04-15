# kernel-lore-mcp — Best-in-class kernel-research MCP: 6-month roadmap

**Status:** active — supersedes the top-level framing of
[`2026-04-15-mcp-spec-coverage-and-uplift.md`](./2026-04-15-mcp-spec-coverage-and-uplift.md),
which remains authoritative for Phase 10–17 MCP-surface detail.

**Date:** 2026-04-14
**Scope:** The next ~6 months of work, reframed from "hit 100% of the MCP
spec" to "be the best MCP server a kernel contributor, maintainer, or
security researcher can point their agent at."
**Inputs (all committed under `docs/research/`):**
- [`2026-04-14-workflow-gap-analysis.md`](../research/2026-04-14-workflow-gap-analysis.md) — what kernel humans do all day that we can't answer yet.
- [`2026-04-14-best-in-class-mcp-survey.md`](../research/2026-04-14-best-in-class-mcp-survey.md) — 36-item scorecard vs top-quartile public MCP servers.
- [`2026-04-14-agent-ergonomics.md`](../research/2026-04-14-agent-ergonomics.md) — what LLM agents find easy vs frustrating.
- [`2026-04-14-external-data-sources.md`](../research/2026-04-14-external-data-sources.md) — kernel-adjacent sources beyond lore.

---

## 1. Reframing

The earlier plan is structured as "Phase 10–17 covers the ~65% of MCP we
don't use yet." That framing is still correct at the code level, but it
isn't the right **north star**. A kernel researcher is not asking us to
be spec-compliant; they're asking us to answer:

> "Has this patch landed? Who owns this file? Is this CVE fixed in
> stable-6.6? Where else does this XDR pattern appear? Is the series
> superseded?"

Today we answer a strong subset of "what's in lore" and nothing beyond
that boundary. Four streams of research converge on a clear conclusion:
**our data model is top-decile; the tool surface and data boundary are
mid-pack.** Closing the gap is cheap and the leverage is enormous.

## 2. Cross-stream convergence — unanimous wins

Items that appeared in **at least two** of the four research streams,
sorted by leverage:

| # | Feature | Streams | Effort | Value |
|---|---|---|---|---|
| A | **MAINTAINERS parser** — `lore_maintainer(path|function)` + `maintainers_for(diff)` + `lore://maintainer/{path}` | workflow-gap #1-4, data-sources #1 | 2–3 d | 5 |
| B | **CVE chain** — `lore_cve_chain(id)` joining `linux-cve-announce` + CVE V5 + RH CSAF + stable backports | workflow-gap #14, data-sources #2+#3+#7+#8, mcp-survey §12 | 3–5 d | 5 |
| C | **Patchwork state lookup** — `lore_patch_state(message_id)` + `lore://patchwork/{msg_id}` | workflow-gap #7, data-sources #5 | 2–3 d | 5 |
| D | **syzbot wrapper + KASAN parser** — `syzbot_bug(id)` + `lore_extract_repro` + structured crash parser | workflow-gap #17+#18+#19, data-sources #6 | 1 w | 5 |
| E | **patch-id cherry tracking** across mainline + stable + subsystem trees | workflow-gap #8+#10+#13, data-sources Tier 1 | 1 w (tree mirror) + 3 d (query) | 5 |
| F | **Bootlin Elixir proxy (self-hosted)** — `symbol_defs`, `symbol_xrefs`, `file_symbols` | workflow-gap #25, data-sources #10 | M (self-host) / L (proxy-only) | 5 |
| G | **Regzbot + Fixes-reverse-index** — `regression_status` + `who_fixed(sha)` | workflow-gap #23+#36+#38, data-sources #9 | 2–3 d | 5 |
| H | **Populated KWIC snippets + byte-offset citations** | mcp-survey §20 + agent-ergo rule 10 | 0.5 d | 4 |
| I | **Freshness marker (`as_of`, `lag_seconds`) on every response** | mcp-survey §12, workflow-gap §1.8 | 0.25 d | 4 |
| J | **3-section tool descriptions + full annotation quad + cost-class hint** | mcp-survey §1+§8+§18, agent-ergo rules 1+3+4+8 | 0.5 d | 4 |
| K | **Public eval scorecard** via `mcp-eval` on ~20 canned kernel-research queries | mcp-survey §15, agent-ergo §15 | 1–2 d | 4 |
| L | **Token-budget pagination** with HMAC opaque cursors wired through every list tool | mcp-survey §11, agent-ergo rule 5 | 1 d | 4 |
| M | **Server-provided prompts (≤3–5 slash commands)** encoding the canonical workflows | mcp-survey §4, agent-ergo §12 | 1 d | 3 |
| N | **Resource templates** (`lore://message/{mid}`, `lore://thread/{tid}`, `lore://patch/{mid}`, `lore://maintainer/{path}`, `lore://patchwork/{msg}`) | mcp-survey §3, workflow-gap multi | 1 d | 3 |

**Single biggest unforced error**: **I (freshness marker)** — <100 LOC, no
competitor does it well except FreshProbe, generation counter already exists.

**Single biggest reputation lever**: **K (eval scorecard)** — MCP-Bench axes
become our public CI, turning every PR into a credibility artifact.

## 3. Cross-stream "do not ship"

Each stream independently flagged these; the intersection is binding.

- **No authentication of any kind, ever.** No API keys, no OAuth 2.1, no DCR,
  no bearer tokens, no login flow. The server is anonymous read-only on every
  deployment — local, public, and every instance in between. Anything that
  would require a caller to hold a secret is rejected at design time. This is
  a **product constraint, not a posture choice**: we deliberately lower the
  barrier so every developer's agent can talk to a kernel-lore MCP without
  paperwork, without a key rotation story, without a shared-secret footgun.
- **Production hosting is the same public-read-only server as local.** No tiered
  access, no rate-gated "pro" endpoints, no "authenticated users get more."
  If a data source we integrate requires auth against its upstream (KCIDB
  BigQuery, GitHub API, etc.), that auth lives in the server's own deployment
  config — **never exposed to callers**.
- **We REDUCE load on lore.kernel.org; we do not add to it.** Every agent
  pointed at a kernel-lore-mcp instance is one fewer agent that would
  otherwise scrape `lore.kernel.org` directly. Fanout-to-one is the value
  proposition. Do not apologize for integrating; the hosted instance + every
  self-hosted instance together subtract traffic from lore, not add to it.
  The server ingests via grokmirror (the sanctioned upstream mirror protocol)
  and serves the indexed corpus, so there is zero additional HTTP pressure
  on `lore.kernel.org` per MCP query. That's the point.
- **No server-side sampling** as load-bearing — client support is sparse.
  Ship sampling-based tools only when they gracefully fall back to extractive.
- **No elicitation round-trips** on required params — every query can be
  served with defaults. Use elicit only to narrow genuinely-ambiguous risky
  queries, with hard-reject fallback.
- **No SSE transport.** Deprecated Apr 1 2026; Streamable HTTP only.
- **No stemming / stopwords / asciifolding / typo tolerance** in tokenizer.
- **No tree of tools > 10 without Tool Search.** Hold the surface small.
- **No scraping kernel.org properties behind Anubis.** Use sanctioned APIs;
  file infra requests rather than solving PoW.
- **No scraping LWN during the 1-week subscriber embargo.** Take a corporate
  subscription; ingest post-embargo; CC-BY-SA attribution.
- **No Coverity Scan redistribution.** ToS-forbidden. Link only.
- **No syzbot pre-public scraping.** Out of scope by embargo policy.
- **No FastAPI REST surface in v1.** MCP first; REST only if demand lands.

## 4. Revised phase sequence (Phase 10 → Phase 22)

We keep the existing Phase 10–17 numbering (detailed in the uplift plan)
and **extend** with Phase 18–22 for external-data integration. Phase 10–17
is unchanged in scope but is now interleaved with the external-data work so
early wins ship alongside MCP-surface uplift.

### Sprint 0 — landing the cheap unforced errors (1 week)
Not a phase; a thin sweep across two days.

- **I — Freshness marker.** Add `as_of` + `lag_seconds` to every tool's
  pydantic response model. Populated from the generation file. 0.25 d.
- **J.1 — Full annotation quad + `title` on every existing tool.** 0.25 d.
- **J.2 — Cost-class hint** (`cheap|moderate|expensive`) + `expected_latency_ms`
  in description for regex / bulk / summarize tools. 0.25 d.
- **H — Populate KWIC `Snippet`** field (already plumbed, never filled).
  0.5 d. Implements mcp-survey scorecard item 29.

**Checkpoint:** scorecard rises from 9.5/36 → ~15/36. No new data sources yet.

### Phase 10 — Resource templates (1 d)
Per existing plan — `lore://message/{mid}`, `lore://thread/{tid}`,
`lore://patch/{mid}`. **Extend** to also include `lore://maintainer/{path}`
(foreshadowing Phase 18A) and `lore://patchwork/{msg_id}` (foreshadowing
Phase 19). MIME types set correctly (`text/x-diff` for patches).

### Phase 11 — Server-provided prompts (1 d)
Per existing plan — ≤5 slash commands. Content now informed by the workflow-
gap analysis:

1. `/klmcp_pre_disclosure_novelty_check` — has this pattern been reported?
2. `/klmcp_cve_chain_expand` — CVE → fix + introducing + backports (Phase 18C).
3. `/klmcp_series_version_diff` — tour vN→vN+1 changes.
4. `/klmcp_recent_reviewers_for` — rank reviewers by file touches (workflow-gap #32).
5. `/klmcp_cross_subsystem_pattern_transfer` — "is this overflow also in sunrpc/SCSI/RDMA?" (workflow-gap #39 — the canonical user's single highest-valued workflow).

### Phase 12 — Sampling with fallback (2 d)
Per existing plan — `lore_summarize_thread`, `lore_classify_patch`,
`lore_explain_review_status`. Always extractive fallback; gate on
`client_supports_extension("sampling")`.

### Phase 13 — Snippets + bulk + pagination (2.5 d)
- **13a** Snippets — **done in Sprint 0.**
- **13b** Bulk-read variants on lookup tools (agent-ergo bulk/batch rule).
  `lore_messages(message_ids: list[str])` returning N records in one call.
- **13c** Wire HMAC cursors through every list-returning tool. **Use
  token-budget cutoff** (agent-ergo rule 5), not record count — return
  `next_cursor` + `estimated_remaining_tokens`.

### Phase 14 — Streaming progress + `ctx.info` (1.5 d)
Per existing plan. `ctx.report_progress` on `lore_regex`, `lore_patch_search`,
`lore_summarize_thread`. `ctx.info` on expensive branch decisions (regex
scan, Parquet full rescan, trigram confirm).

### Phase 15 — Elicitation for risky queries (1 d)
Per existing plan. Narrow `lore_regex`, `lore_thread`, `lore_in_list` with
`ctx.elicit()`; hard-reject fallback when unsupported.

### Phase 16 — Roots / workspace awareness (0.5 d)
Per existing plan. `ctx.list_roots` drives smart defaults on scope-aware
tools; fall through to env defaults when unavailable.

### Phase 17 — Tool-description + annotation polish (deferred 0.25 d)
Mostly absorbed into Sprint 0 J.1/J.2. Phase 17 residual: rewrite every tool
description to the 3-section template (purpose / examples / prefer-when).

### Phase 18 — kernel.org data, cheap tier (Month 1, ~1 week)
Derived from data-sources Month 1. Pure git / file ingestion — no scraping,
no API auth.

- **18A — MAINTAINERS parser.** `lore_maintainer(path|function|diff)` +
  `maintainers_for(file_list)` + `lore://maintainer/{path}` resource.
  Churn/ownership-mtime surfaced. Parsed from `linux.git`. **~3 d.**
- **18B — kernel.org release feeds.** `latest_releases(branch)`. Atom feed
  poll. **~0.5 d.**
- **18C — CVE List V5 + Red Hat CSAF ingest.** `cve(id)` +
  `cves_touching(file_or_commit)` + `vendor_advisory(cve, vendor="rhel")`.
  Both are git-clone / file-drop; no rate-limit politics. **~3 d.**
- **18D — Documentation/ + htmldocs.** `doc_lookup(topic|symbol)`.
  **~1 d.**

**Checkpoint:** eight high-leverage workflows unblocked on cheap data only.

### Phase 19 — Patchwork + syzbot (Month 2, ~1.5 weeks)
External APIs. Build **shared "external API sink"** abstraction here —
polite-crawl + cache + incremental state + provenance tagging — then
populate it twice.

- **19A — Patchwork state.** `lore_patch_state(message_id)` +
  `series_for(cover_message_id)` + state resource. Polite nightly crawl via
  `events` endpoint; coordinate with kernel.org infra on Anubis.
  **~3 d.**
- **19B — syzbot bug state.** `syzbot_bug(id)` + `syzbot_search(subsystem)`.
  Hourly metadata ingest; repro assets proxied. **~3 d.**
- **19C — Crash parser.** `parse_crash(text) -> {type, symbol, offset,
  access_size, allocated_by, freed_by, pc, stack[]}`. Reference: syzkaller
  `pkg/report/linux.go`. **~3 d.** Pairs with 19B for agent-visible
  structured crash fields.

### Phase 20 — Cross-distro + regression triangulation (Month 3, ~1 week)
- **20A — Debian + Ubuntu + SUSE trackers.** Extend `vendor_advisory` with
  debian/ubuntu/suse values. Deliverable: `cve_status_matrix(id)`.
  **~3 d.**
- **20B — regzbot + `Fixes:` reverse index.** `regression_status(id)` +
  `who_fixed(sha)`. Latent in metadata tier — cheap. **~1.5 d.**
- **20C — `lore_cve_chain` composition.** Wires Phase 18C + 20A + 20B +
  `linux-cve-announce` list. **~1 d.**

### Phase 21 — Code-aware + tree-aware queries (Month 4, ~2 weeks)
- **21A — Self-host Elixir Bootlin.** Deploy container, point at
  mirrored `linux.git`. Expose `symbol_defs`, `symbol_xrefs`, `file_symbols`.
  Storage review + auth isolation. **~1 w.**
- **21B — Tree-aware commit lookup.** Mirror mainline + stable +
  linux-next + top ~20 subsystem trees via grokmirror. `lore_commit(sha,
  tree)`. Feeds Phase 21C. **~3 d.**
- **21C — patch-id cherry tracker.** Given a message-id, compute
  `git patch-id --stable` and search every mirrored tree. Answers "did
  this land?" durably, independent of subject-line drift. **~3 d.**

### Phase 22 — Ethics-sensitive + CI (Month 5–6)
- **22A — LWN post-embargo ingest + RSS headlines.** Requires corporate
  subscription; CC-BY-SA attribution; hard exclude during embargo.
  **~1 w.**
- **22B — openwall oss-security archive.** Polite crawl + RSS. Index
  only; return excerpts + links; no redistribution. **~3 d.**
- **22C — KCIDB BigQuery proxy.** `ci_status(commit_or_series, project)`
  over KernelCI + CKI + LKFT unified schema. **~1 w.**
- **22D — Reciprocity pass.** Push list-metadata improvements upstream;
  publish our grokmirror config; file public-inbox PRs for scale bugs
  we hit. Governance deliverable, not a feature. **~2–3 d.**

## 5. Cross-cutting workstreams

### CW-A — Eval scorecard (K)
Adopt [`mcp-eval`](https://github.com/lastmile-ai/mcp-eval) in-process via
FastMCP `Client`. Ship 20 canned kernel-research queries covering all six
MCP-Bench axes (task fulfillment, info grounding, tool appropriateness,
parameter accuracy, dependency awareness, parallelism efficiency). Publish
`docs/evals/scorecard.md` per release. Wire into CI as a non-blocking
trend report.

**Seed query examples:**
- "Who are the maintainers of `fs/smb/server/oplock.c` and who reviewed
  patches in the last 90 days?"
- "For CVE-2024-26924, what commit fixed it in mainline, what stable
  branches have the backport, and what was the pre-disclosure discussion
  on oss-security?"
- "Has this patch-id already landed in `linux-next` under a different
  subject line?"
- "Find every KASAN splat with a stack frame in `svc_rdma_*` from the last
  6 months, grouped by first-seen date."

### CW-B — Auto-generated tool reference
Walk the FastMCP registry; emit `docs/mcp/tools-reference.md` with the
full outputSchema + annotation quad + cost-class + examples per tool.
Runs in CI. 0.5 d.

### CW-C — Worked-transcript library
Capture ≥5 real transcripts (claude --print, codex exec) against each
major workflow. `docs/mcp/transcripts/*.md`. Rotate quarterly. 0.5 d.

### CW-D — Structured `LoreError` envelope
Build a unified error shape: `{code, human_message, valid_example,
echoed_input, retry_after_seconds?}`, always with `isError: true` on the
tool result envelope (not transport exception). Enables agent
self-correction (agent-ergo §14). 0.5 d.

### CW-E — `response_format: "concise" | "detailed"`
Add to `lore_search`, `lore_thread`, `lore_activity`, `lore_patch_diff`.
Default `"concise"`. Returns markdown summary of top N rows in `content`
pointing to `structuredContent` for full results. 0.5 d.

### CW-F — Streamable HTTP smoke test
Carried over from the uplift plan. Public-HTTP parity with the stdio
live test in `scripts/agentic_smoke.sh`. Runs in CI against a local
uvicorn. 0.5 d.

### CW-G — External-API-sink abstraction
Write once (during Phase 19); reuse for 18C, 19A, 19B, 20A, 21A, 22A–C.
Policy-level primitives: polite crawler, content-addressed cache,
incremental state, per-source rate limit + retry, provenance tagging.
Mentioned in each external-data phase; counted once here. ~2 d.

## 6. Sequencing graph

```
 Sprint 0 ── Phase 10 ── Phase 11 ── Phase 13b/c ── Phase 14 ── Phase 15 ── Phase 16
    │            │            │           │            │           │           │
    │            └────> Phase 18A/B/C/D ──> Phase 19 ──> Phase 20 ──> Phase 21 ──> Phase 22
    │                                         │            │
    └──> CW-A eval scorecard (continuous, weekly update)
    └──> CW-B tool reference (CI, every PR)
    └──> CW-C transcripts (quarterly)
    └──> CW-D LoreError envelope (parallel with Phase 13)
    └──> CW-E response_format (Sprint 0)
    └──> CW-F streamable smoke (post Phase 10)
    └──> CW-G API sink (built in Phase 19, reused thereafter)
```

## 7. Success metrics

At the end of Month 6 we measure:

1. **Scorecard coverage** (`docs/research/2026-04-14-best-in-class-mcp-survey.md`):
   **target ≥32/36.**
2. **Eval scorecard** (CW-A): **task-success rate ≥ 85% on the 20 canned
   queries**, tool-selection accuracy ≥ 90%, median round-trips per answer
   ≤ 2.
3. **Data surface**: every unanimous-win feature (A–N in §2) shipped.
4. **Reciprocity deliverable** (22D): at least one merged upstream PR /
   config publication that benefits the public lore infrastructure.
5. **No regressions** against the "do not ship" list in §3.

## 8. What this supersedes

- The existing Phase 10–17 plan
  ([`2026-04-15-mcp-spec-coverage-and-uplift.md`](./2026-04-15-mcp-spec-coverage-and-uplift.md))
  is still authoritative for **MCP-surface-level detail** of Phases 10–17.
  This plan is the **top-level framing** and extends with Phase 18–22 plus
  Sprint 0 and cross-cutting workstreams.
- `CLAUDE.md` pointer stays on the old plan for the retro section; a new
  pointer to this doc is appended.

## 9. Open questions (flag before starting)

1. **Elixir self-host (21A) vs proxy.** Self-hosting is infra weight but
   sidesteps rate-limit questions. Decide before Phase 21.
2. **KCIDB BigQuery billing (22C).** Google Cloud project + budget cap
   needed. Separate spend decision.
3. **LWN subscription (22A).** Corporate sub before any scraping.
4. **Public eval scorecard publication cadence.** Weekly in CI ✔.
   Published externally quarterly? Decide before CW-A lands.
5. **Tool-count cap.** We will grow from 7 → ~15 over 6 months. Commit now
   to adding a `lore_index` discovery tool + Tool Search at 10.

---

*This plan merges four independently-commissioned research streams
completed 2026-04-14. All four source reports live under
`docs/research/2026-04-14-*`. The sequencing is conservative but
intentionally front-loads the unforced-error items (Sprint 0, Phase 18A–D)
because they unblock the biggest agentic workflows for the smallest code
commits.*
