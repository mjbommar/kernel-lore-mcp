# kernel-lore-mcp — Best-in-class MCP server survey

**Date:** 2026-04-14
**Scope:** Top-quartile public MCP servers (Sentry, GitHub, Linear, Atlassian,
Stripe, Sourcegraph, Cloudflare, Notion, Brave, Context7, DeepWiki, Playwright,
Anthropic reference servers, Vercel + Cloudflare Workers ecosystem).
**Output:** 20 dimensions + 36-item scorecard. Current posture ~9.5/36.

---

## 1. Tool-description quality

- **Median.** One-sentence "what" + parameter schema.
- **Top-quartile.** 3-section descriptions: purpose, worked examples, "prefer this when / prefer that when." Context7's `resolve-library-id` embeds operational constraints. Gong's `get_calls` encodes routing guidance inline. GitHub's server adds *toolsets* + read-only mode to reduce prompt bloat.
- **Us.** 19 tools with terse pydantic-derived schemas; no worked examples, no "prefer this" blocks.
- **Gap.** Rewrite every tool to 3-section template; group by toolset.

## 2. Output schema discipline

- **Median.** Text blobs in `content`; no `outputSchema`.
- **Top-quartile.** Declare `outputSchema`, return both `structuredContent` + `content`. MCP 2025-06-18 blessed.
- **Us.** All 19 tools return pydantic `BaseModel` — already top-quartile.
- **Gap.** Tighten a few auto-derived schemas with `Literal[...]` enums.

## 3. Resource patterns

- **Median.** Zero or one static resource.
- **Top-quartile.** RFC-6570 resource templates, multiple URI schemes, MIME types.
- **Us.** One static resource (`blind-spots://coverage`). Phase 10 addresses.
- **Gap.** Ship `lore://message/{mid}`, `lore://thread/{tid}`, `lore://patch/{mid}` with `text/x-diff` MIME.

## 4. Prompts

- **Median.** None.
- **Top-quartile.** 3–10 prompts as slash commands. GitHub, Cloudflare portals, Atlassian ship curated catalogs.
- **Us.** Zero. Phase 11 adds five.
- **Gap.** Ship `pre-disclosure-novelty-check`, `cve-chain-expand`, `series-version-diff`, `recent-reviewers-for`, `triage-incoming-patch`.

## 5. Sampling (`ctx.sample`)

- **Median.** Not used (client support limited).
- **Top-quartile.** Used with graceful non-sampling fallback (Memgraph).
- **Us.** Not used. Phase 12 adds `lore_summarize_thread`, `lore_classify_patch`, `lore_explain_review_status`.
- **Gap.** Ship with extractive fallback; gate on `client_supports_extension("sampling")`.

## 6. Elicitation (`ctx.elicit`)

- **Median.** Not used; hard reject on ambiguous input.
- **Top-quartile.** Narrow risky queries (regex scans, bulk deletes). `delete_all_notes` is canonical.
- **Us.** Not used. Phase 15 adds elicit-to-narrow on `lore_regex`, `lore_thread`, `lore_in_list`.
- **Gap.** Ship with `client_supports_extension("elicitation")` gate.

## 7. Progress notifications

- **Median.** None; long tools feel hung.
- **Top-quartile.** `ctx.report_progress(done, total, message)` per row-group/page.
- **Us.** None. Phase 14.
- **Gap.** Wire into `lore_regex`, `lore_patch_search`, `lore_summarize_thread`.

## 8. Tool annotations

- **Median.** Only `readOnlyHint`.
- **Top-quartile.** Full quad + `title`: `readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`, `title`. SEP-986/1575 add `version`/`deprecates`.
- **Us.** Only `readOnlyHint` + partial `idempotentHint`.
- **Gap.** Add all four + `title`; `openWorldHint=true` (corpus grows).

## 9. Error patterns

- **Median.** Bare exception strings.
- **Top-quartile.** Three-question error messages: what/why/how-to-fix. Block's playbook codifies this.
- **Us.** `Error::QueryParse` / `Error::RegexComplexity` actionable.
- **Gap.** Audit every error for what/why/fix triad; add retry guidance on transient errors.

## 10. Auth model

- **Median.** API key in env/query.
- **Top-quartile.** OAuth 2.1 + Dynamic Client Registration (RFC 7591). Linear, Atlassian, Cloudflare, Notion. Stripe layers restricted keys.
- **Us.** Anonymous public read-only — appropriate for a public corpus.
- **Gap.** None today. OAuth 2.1 + DCR if hosted instance ever accepts embargoed material.

## 11. Pagination

- **Median.** `limit`/`offset` or none.
- **Top-quartile.** Opaque cursor strings, resumable.
- **Us.** HMAC-signed cursors designed/built at router layer but never wired through any tool. Phase 13c.
- **Gap.** Wire cursors into the 9 tools; HMAC-signed is *above* top-quartile.

## 12. Freshness signals

- **Median.** Silent.
- **Top-quartile.** Explicit `data_age_seconds` / `as_of` / freshness in responses (FreshProbe).
- **Us.** `/status` has it but not folded into tool responses.
- **Gap.** Add `as_of` + `lag_seconds` to every search/activity response.

## 13. Multi-tenant scoping

- **Top-quartile.** Workspace/org/project params; respect per-user ACLs.
- **Us.** Public, single-tenant. `list:` is first-class.
- **Gap.** None critical. Consider server-level defaults (`list`/`since`) via config or resource.

## 14. Telemetry exposed to clients

- **Top-quartile.** `ctx.info`/`ctx.debug` in client log panes; Sentry auto-instruments MCP spans.
- **Us.** structlog stderr only.
- **Gap.** Emit `ctx.info` on expensive-branch decisions. Phase 14.

## 15. Eval / test suites

- **Top-quartile.** Public scorecards. GitHub published offline eval; MCPBench, MCP-Bench, mcp-eval, MCPEval, OpenAI Evals API recipe.
- **Us.** 132 tests, unit + e2e. No LLM-as-judge or task-success metric.
- **Gap.** Adopt `mcp-eval`; publish a scorecard (task success, tool-selection accuracy, mean round-trips to answer) on ~20 kernel-research queries.

## 16. Versioning + deprecation

- **Top-quartile.** SemVer in tool metadata, explicit `deprecates`, `minProtocolVersion`. SEP-1575.
- **Us.** Nothing.
- **Gap.** Add `version` + `deprecates` to annotations once SEP lands; expose `server_version` via `/status` and in every structured response.

## 17. Documentation format

- **Top-quartile.** Auto-generated tool reference, OAuth flow docs, example transcripts, toolset recipes. GitHub's `docs/server-configuration.md` recipe book.
- **Us.** Architecture docs strong; no auto-gen tool reference; no transcripts.
- **Gap.** CW-B: generate `docs/mcp/tools-reference.md` from registered tools; add 5-transcript "worked examples."

## 18. Performance + cost telegraphy

- **Top-quartile.** Tool descriptions mention latency class and/or token cost.
- **Us.** Nothing in descriptions.
- **Gap.** Add `cost_class: cheap|moderate|expensive` hint and `expected_latency_ms` in regex / bulk / summarize tool descriptions.

## 19. Streaming vs batch

- **Top-quartile.** Streamable HTTP + optional per-tool partial-result streaming. DeepWiki streams "Deep Research" progress.
- **Us.** Streamable HTTP declared; partial-result streaming inside a response not implemented.
- **Gap.** Implement row-group streaming on long-tail tools (Phase 14).

## 20. Citation patterns

- **Top-quartile.** Every record carries immutable ID + canonical URL + byte-offset/checksum. Brave, DeepWiki.
- **Us.** Every hit ships `message_id`, `cite_key`, `lore_url`, `snippet.sha256`. Best-in-class — except `Snippet` field never populated.
- **Gap.** Phase 13a: wire KWIC snippet extractor.

---

## 36-item scorecard

A server hitting ≥30 sits top 10%. Our count: **9.5/36**. `[we]` = already hit.

### Tools & schemas
1. [ ] 3-section description (what / examples / when-to-prefer) on every tool.
2. [we] `structuredContent` via `outputSchema` on every tool.
3. [ ] `readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`, `title` on every tool.
4. [ ] `version` + deprecation policy per tool.
5. [ ] Cost/latency class hint in tool description.
6. [ ] Tools grouped into toolsets (enable/disable).
7. [we] Read-only mode supported.
8. [ ] Bulk/batch variant for N-lookup cases.

### Resources
9. [ ] RFC-6570 resource template with parameters.
10. [ ] MIME types set correctly.
11. [we] Static capabilities/blind-spots resource.
12. [ ] `list_resource_templates` tested.

### Prompts
13. [ ] ≥3 server-provided prompts as slash commands.
14. [ ] Prompts have optional-with-defaults args (Claude Code compat).
15. [ ] Prompts reference server's own tools.

### Context plumbing
16. [ ] `ctx.report_progress` on any tool >500ms p95.
17. [ ] `ctx.info`/`ctx.debug` on nontrivial branch decisions.
18. [ ] `ctx.sample` with non-sampling fallback.
19. [ ] `ctx.elicit` with hard-reject fallback.
20. [ ] `ctx.list_roots` drives smart defaults on scope-aware tools.
21. [ ] Optional client capabilities gated via `client_supports_extension`.

### Transport & auth
22. [we] Streamable HTTP (not SSE).
23. [ ] OAuth 2.1 + DCR **or** documented anonymous-read rationale.
24. [we] stdio mode: nothing to stdout outside framing.

### Pagination & scale
25. [ ] Opaque cursor pagination wired through every list-returning tool.
26. [we] Cursors tamper-resistant (HMAC-signed).
27. [ ] Cap + "narrow your query" guidance on unbounded scans.

### Output quality
28. [we] Every citation: stable ID + canonical URL + checksum.
29. [ ] Every citation has populated snippet + byte offset.
30. [ ] Every response has `as_of` / `lag_seconds`.
31. [ ] Errors answer what/why/fix + suggest retry.

### Observability & evals
32. [we] Prometheus `/metrics` (localhost).
33. [ ] Public eval scorecard under a named benchmark.
34. [ ] OpenTelemetry / Sentry MCP span instrumentation.

### Docs
35. [ ] Auto-generated tool reference from registered schemas.
36. [ ] ≥3 worked-transcript examples against a real client.

---

## Priority-ordered closers

1. **Resource templates** (9, 10, 12) — Phase 10, 1 d.
2. **Full annotation set + 3-section descriptions + cost class** (1, 3, 5) — Phase 17, 0.5 d.
3. **Snippets populated** (29) — Phase 13a, 0.5 d.
4. **Cursor wiring** (25) — Phase 13c, 1 d.
5. **Progress + ctx.info** (16, 17) — Phase 14, 1.5 d.
6. **Prompts** (13, 14, 15) — Phase 11, 1 d.
7. **Freshness marker** (30) — new, 0.25 d.
8. **Sampling with fallback** (18) — Phase 12, 2 d.
9. **Elicitation** (19) — Phase 15, 1 d.
10. **Public eval scorecard** (33) — new, 1–2 d (mcp-eval + 20 queries).
11. **Auto-gen tool reference + transcripts** (35, 36) — CW-B, 0.5 d.
12. **OTEL/Sentry spans** (34) — new, 0.5 d.

**Total beyond existing plan:** ~3 extra days (freshness, eval scorecard, OTEL). All-in lands ~32/36 — top-decile.

## Key findings

- Our **data model** is already ahead of top-quartile (stable `message_id`, `cite_key`, `body_sha256`, tier_provenance, HMAC cursors). What's missing is MCP-surface plumbing to **expose** it.
- Biggest unforced error: freshness (item 30) — <100 LOC, no competitor does it well except FreshProbe, and we already have the generation counter.
- Public scorecards are becoming table-stakes for credibility. Publishing an `mcp-eval` harness + 20-query scorecard is disproportionate reputation leverage.
- **Resist OAuth 2.1.** Anonymous read-only is the *right* posture for a public lore mirror; document it, don't treat it as a gap.

## Sources

- MCP 2025-06-18 tools spec; MCP prompts spec; MCP versioning
- `github/github-mcp-server` + `server-configuration.md`
- Sentry MCP docs, cookbook, Python integration
- Linear MCP docs + changelog; Atlassian remote MCP; Stripe MCP
- Cloudflare MCP + portals + blog; Context7 API ref; DeepWiki MCP
- Sourcegraph MCP; Notion MCP; Brave Search MCP; Playwright MCP; Vercel MCP
- FastMCP docs (elicitation, resources, tools)
- Merge blog on tool descriptions; Philschmid best practices; Block's MCP playbook
- MCPcat error handling; Alpic on error responses; Marc Nuri on annotations
- FreshProbe; MCP-Bench paper + Accenture repo; mcp-eval; MCPBench; GitHub offline eval post; OpenAI Evals API MCP recipe
- Evolvable MCP; SEP-1575 discussion; Cisco on structured content + elicitation; The New Stack on elicitation; Memgraph on sampling + elicitation; AWS Bedrock AgentCore blog
- Docker MCP best practices; Apigene 12 rules; Glama directory; PulseMCP; mcpservers.org
