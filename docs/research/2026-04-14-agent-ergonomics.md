# kernel-lore-mcp — Agent-friendly MCP server design

**Date:** 2026-04-14
**Scope:** What LLM agents find easy vs frustrating when driving MCP servers.
Sources: Anthropic engineering blog, Datadog, JetBrains, Klavis, Alpic, MCPcat,
MCP-Bench, MCP-AgentBench, spec issues.

## Top 10 rules

1. **Budget every tool description against a 25k-token response ceiling and a ~5–6% "before-prompt" context tax.** Claude Code caps tool responses at ~25,000 tokens; its built-in toolset alone consumes ~5.9% of the context window.
2. **Keep the tool surface small and workflow-oriented, not API-mirrored.** Cursor degrades past ~40 active tools; GitHub Copilot caps at 128, Cursor at 40, Junie at 100.
3. **Name tools `service_resource_action`, ≤40 chars raw.** Claude Code hard-limits full name to 64 chars *including* `mcp__server__` prefix; leave ≥30 char headroom.
4. **Write descriptions in third person, lead with "use when…", include concrete examples + anti-examples.** Examples lifted Opus accuracy 72% → 90% on complex parameter handling.
5. **Paginate by token budget, not record count.** Datadog got 5× more useful records per call switching to token-budget cutoffs with opaque cursors.
6. **Return dual output: readable `content` + strict `structuredContent` against `outputSchema`.** Spec-blessed in 2025-06-18.
7. **Error envelopes must say what happened, why, and a valid example.** `isError: true` re-injects into the model's context — errors are prompts.
8. **Offer `response_format: "concise" | "detailed"` + a token-cost preview.** Agents actively use verbosity knobs when exposed.
9. **Progressive disclosure: tiny always-on "index" tool + `defer_loading: true` on the rest.** Tool Search Tool preserved 191k vs 123k tokens; Opus 4.5 accuracy 79.5% → 88.1%.
10. **Every fact citable with a stable fetchable identifier.** `Message-Id` + lore URL + SHA, not prose.

---

## Per-dimension notes

### Tool selection with many tools
At ~40+ tools, Cursor agents start picking wrong. The selector weights the **first 1–2 sentences** of the description and `when to use` phrasing. Anti-examples are as valuable as positive examples. **For us:** `lore_*` prefix correct, 7 tools well below any threshold. Add a `lore_index` discovery tool only if count grows past ~12.

### Description framing
- Third person only.
- Template: `"<verb phrase>. Use when the user asks about <trigger 1>, <trigger 2>, or mentions <keyword>."`
- Examples beat prose. Load-bearing detail in first 500 chars; Claude Code truncates at 2 KB.

### Error message shape
Three questions: what / why / what valid input looks like.
High-recovery pattern:
```
"Page number 10 is out of range. Specify a page number between 1 and 3."
"unknown field 'stauts' – did you mean 'status'?"
```
Use `isError: true` on the tool result envelope (application-level), not a transport exception.
**For us:** our "phrase queries on prose are REJECTED" rule is correct — but the rejection must include the corrected query form and a pointer to the right tier.

### Output structure
Return both `content` (token-efficient summary) + `structuredContent` (schema-validated JSON) with declared `outputSchema`. **`content` should be a markdown summary of top N rows** plus "see structuredContent for full results" pointer. Prefer flat objects with scalar IDs over nested trees; models degrade past 3 nesting levels.

### Citation quality (ranked)
1. Fetchable URL (best).
2. Stable content-addressed ID (`body_sha256`, commit SHA).
3. Opaque internal ID (worst — requires another call).
Return all three where applicable. ≤200-char snippet from raw matched text lets the model ground its summary and lets humans verify.

### Token budget numbers
- Claude Code per-tool cap: 25,000 tokens.
- Anthropic's internal agent tooling: 134k tokens before any user work.
- Real 5-server setup: 55k tokens for 58 tools.
- Enable Tool Search when defs > 10k tokens or > 10 tools.
- Add `dry_run: true` parameter returning `{"estimated_tokens": N, "estimated_rows": M}`.

### Pagination
Opaque cursors are spec-blessed; agents handle them if the description says "pass the cursor from the previous call's `next_cursor` field." **Best empirical pattern: token-budget pagination** (Datadog) — server stops emitting when buffer fills, returns `next_cursor`.

### Tool naming
- `snake_case`.
- `<service>_<resource>_<action>` (e.g., `lore_thread_get`).
- Max raw length 32 chars (gives margin for `mcp__kernel-lore-mcp__` ≈ 23 chars).
- Never reuse `anthropic`, `claude`.
Our current names (7, ≤20 chars raw) are safe.

### Default-on vs opt-in
Inflection ~10 tools / 10k tokens. Below: default-on wins. Above: opt-in + search. Anthropic Tool Search gains: 49→74% (Opus 4), 79.5→88.1% (Opus 4.5). **Us at 7 tools: ship everything default-on. Revisit at 10.**

### Streaming vs batched
`notifications/progress` useful for long-running ingest, **irrelevant for query tools < 2s**. If any tool can exceed 30s, emit progress every ~5s (client-side timeouts reset on progress).

### Resources vs tools
- **Tool** = model-controlled, has side effects / requires reasoning to call.
- **Resource** = application-/user-controlled, stable URI, readable as context.
Cursor added resources only in **v1.6 (Sep 2025)**. Don't make resources load-bearing: mirror as a `lore_about` tool for older clients.

### Prompts / slash commands
Discoverability is weak: users type `/` and recognize. Use only for high-frequency, high-value workflows. **Keep to ≤3 prompts.**

### Sampling and elicitation
- **Sampling** poorly supported; don't design around it.
- **Elicitation** in Claude Code + Cursor; adds latency + friction. Use only when parameter genuinely cannot be defaulted. **For us: probably never worth it** — every query can be served with sensible defaults.

### Recovery from misuse
Highest-recovery patterns:
- **Unknown field + did-you-mean** → ~100% recovery.
- **Out-of-range + valid range listed** → very high.
- **Type mismatch + example** → very high.
- **Opaque "invalid input"** → model retries identically or gives up.
**Always echo the rejected input verbatim so the model can diff its own call.**

### Eval suites
- **MCP-Bench** (arXiv 2508.20453) — 28 servers, 250 tools, 6 axes.
- **MCP-AgentBench** (arXiv 2509.09734) — success-focused.
- **modelscope/MCPBench** — open-source runner.
Ship `tests/eval/` mirroring MCP-Bench axes using `fastmcp.Client` in-process against ~100 seed queries from real kernel-dev workflows.

---

## Concrete implications for kernel-lore-mcp

1. Already correct: small surface (7), `lore_` namespace, `readOnlyHint: true`, Streamable HTTP, `outputSchema` via Pydantic.
2. Add `response_format: "concise" | "detailed"` to `lore_search` + `lore_thread`. Default concise.
3. Add token-budget pagination. Return `next_cursor` + `estimated_remaining_tokens`.
4. Every result carries `{message_id, lore_url, body_sha256, snippet}` for verifiable citation.
5. Build `LoreError` envelope: `{code, human_message, valid_example, echoed_input}`; always set `isError: true`.
6. Resources for `lore://lists/all`, `lore://schema`, `blind_spots://coverage`; mirror as `lore_about` tool for Cursor <1.6.
7. No sampling/elicitation round-trips.
8. ≤2–3 prompts (slash commands).
9. Build eval harness now (before more tools ship), MCP-Bench rubric.
10. Keep tool-def tokens < 10k total — no Tool Search Tool until we exceed ~12 tools.
