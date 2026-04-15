# 2026-04-15 — MCP spec coverage + agentic UX uplift plan

This is the durable plan for the next phase of work. Two themes:

1. **Reach full MCP-spec coverage** — today we use ~35% of what
   `fastmcp` 3.2 + the MCP 2025-06-18 spec offer. Closing that gap
   makes the server materially more useful in Claude Code, Cursor,
   Codex, and Zed without writing many new tools.
2. **Performance + UX wins on the existing surface** — bulk APIs,
   real cursor pagination, streaming progress, populated snippets,
   richer tool descriptions.

Each item carries (a) a one-line goal, (b) a concrete API sketch,
(c) the LOC + day estimate, and (d) the test gate that proves it
landed. Order is "biggest agentic-UX uplift per LOC, in
dependency-aware sequence."

The `lessons learned` retro that motivated this plan is at the
bottom; if you only have five minutes, skip there first.

---

## Coverage matrix — what we ship today vs. what the spec offers

| MCP primitive               | We have | Notes |
|-----------------------------|---------|-------|
| Tools (`@mcp.tool`)         | YES — 19 | All `readOnlyHint: true`, all return pydantic |
| Static resources            | YES — 1 | `blind-spots://coverage` |
| Resource templates (RFC 6570) | NO | `add_template` / `@mcp.resource("uri://{p}")` |
| Prompts (`@mcp.prompt`)     | NO | Server-provided slash commands w/ args |
| Sampling (`ctx.sample`)     | NO | Server asks the *client* LLM — no API key needed |
| Elicitation (`ctx.elicit`)  | NO | Server asks user for structured input mid-call |
| Progress (`ctx.report_progress`) | NO | Long-running tools feel snappier |
| Logging (`ctx.info` etc.)   | NO | Per-call logs surface in the client UI |
| Roots (`ctx.list_roots`)    | NO | Client-declared workspace boundaries |
| Pagination (cursor)         | DESIGNED | HMAC-signed cursors built; tools never wire them |
| Custom HTTP routes          | YES — 2 | `/status`, `/metrics` |
| Tool annotations (full)     | PARTIAL | Only `readOnlyHint` + `idempotentHint` |
| Snippet provenance          | DESIGNED | `Snippet` model exists; tools never populate |
| Bulk read APIs              | NO | Every fetch is one round-trip |

Source-of-truth references:
- Resources & templates spec — [modelcontextprotocol.io/specification/2025-06-18/server/resources](https://modelcontextprotocol.io/specification/2025-06-18/server/resources) and the FastMCP guide [gofastmcp.com/servers/resources](https://gofastmcp.com/servers/resources).
- Prompts spec — [modelcontextprotocol.io/specification/2025-06-18/server/prompts](https://modelcontextprotocol.io/specification/2025-06-18/server/prompts). Note: as of April 2026 Claude Code does not auto-elicit prompt arguments (issue [anthropics/claude-code#30733](https://github.com/anthropics/claude-code/issues/30733)) — pass them inline.
- Sampling spec — [modelcontextprotocol.io/specification/2025-06-18/client/sampling](https://modelcontextprotocol.io/specification/2025-06-18/client/sampling). FastMCP exposes via `await ctx.sample(...)`.
- Elicitation spec + FastMCP guide — [gofastmcp.com/servers/elicitation](https://gofastmcp.com/servers/elicitation).
- Streaming HTTP + progress — [microsoft/mcp-for-beginners 06-http-streaming](https://github.com/microsoft/mcp-for-beginners/blob/main/03-GettingStarted/06-http-streaming/README.md).
- KWIC algorithm (snippet extraction) — H. P. Luhn 1958; modern descriptions e.g. [Sucharithi Imalla, "Keyword-in-Context Extraction Algorithm"](https://medium.com/@sucharithi_imalla/keyword-in-context-extraction-algorithm-592912663f17).

---

## Phase 10 — Resource templates

**Goal.** Let Claude Code / Cursor users drop `@lore://message/<mid>`,
`@lore://thread/<tid>`, etc. straight into chat without burning a tool
call. Resources are zero-tool-budget citations from the LLM's
perspective — the server returns text, the client renders it inline.

**API sketch (`src/kernel_lore_mcp/resources/lore_templates.py`):**

```python
@mcp.resource("lore://message/{message_id}")
def message_resource(message_id: str) -> str:
    reader = _core.Reader(Settings().data_dir)
    body = reader.fetch_body(message_id)
    if body is None:
        raise ResourceNotFound(f"unknown message_id {message_id!r}")
    return body.decode("utf-8", errors="replace")

@mcp.resource("lore://patch/{message_id}", mime_type="text/x-diff")
def patch_resource(message_id: str) -> str: ...

@mcp.resource("lore://thread/{tid}")
def thread_resource(tid: str) -> str: ...

@mcp.resource("lore://activity/{path*}")  # wildcard segment
def activity_resource(path: str) -> str: ...

@mcp.resource("lore://cve/{cve_id}")
def cve_resource(cve_id: str) -> str: ...

@mcp.resource("lore://maintainer/{email}")
def maintainer_resource(email: str) -> str: ...
```

Notes:
- URIs follow RFC 6570 (FastMCP `*` wildcard for path segments).
- `mime_type` matters: `text/x-diff` makes `lore://patch/...`
  syntax-highlight in Claude Code's preview pane.
- Resources can return `list[ResourceContents]` for multi-part output;
  e.g. `thread_resource` returns one TextContent per message so the
  client can collapse/expand.
- Add a `list_resource_templates` discovery test so the templates
  show up in `client.list_resource_templates()`.

**LOC.** ~250 Python + 6 e2e tests.
**Days.** 1.

**Test gate.**
- `tests/python/test_resource_templates_e2e.py` — registers each
  template, asserts `client.read_resource("lore://message/m1@x")`
  returns the right body, asserts unknown-mid raises a
  `ResourceNotFound`-shaped error, asserts wildcard
  `lore://activity/fs/smb/server/smbacl.c` decodes correctly.
- Subprocess plumbing: extend `test_stdio_subprocess.py` with one
  template lookup so we know the URI shape survives the wire.

---

## Phase 11 — Server-provided prompts

**Goal.** Ship the kernel-research workflows from
`/nas4/data/workspace-infosec/kernel-network-simulatory/CLAUDE.md`
as MCP slash commands. An agent invokes
`/mcp__kernel-lore__pre-disclosure-novelty-check fs/smb/server/smbacl.c`
and gets a structured user-message (or a multi-message conversation
seed) back. The tool calls inside the prompt body are still ours,
so the agent uses our primitives — but the *strategy* is encoded.

**API sketch (`src/kernel_lore_mcp/prompts/`):**

```python
@mcp.prompt(
    name="pre-disclosure-novelty-check",
    title="Pre-disclosure novelty check",
    description="Calibrate effort before sending a security report.",
)
def pre_disclosure(
    file: Annotated[str, Field(description="e.g. fs/smb/server/smbacl.c")],
    function: Annotated[str | None, Field(description="optional")] = None,
    since: Annotated[str, Field(description="ISO date or '90d'")] = "90d",
) -> list[PromptMessage]:
    return [PromptMessage(role="user", content=TextContent(
        type="text",
        text=(
            f"Use these MCP tools in order to calibrate novelty for "
            f"{file}::{function or '*'} over the last {since}:\n"
            f"  1. lore_count(field='touched_files', value='{file}', since=...)\n"
            f"  2. lore_eq(field='touched_files', value='{file}', limit=50)\n"
            f"  3. lore_substr_trailers(name='reviewed-by', value_substring='@')\n"
            f"  4. For top-3 distinct authors, run lore_eq(field='from_addr', ...)\n"
            f"Then produce a one-paragraph saturation report: distinct "
            f"researchers, last fix date, recent series count, and a "
            f"single 'go/hold' recommendation."
        ),
    ))]
```

Initial slate (one file per prompt):
1. `pre-disclosure-novelty-check(file, function?, since=90d)`
2. `cve-chain-expand(cve_id)` — find every patch citing this CVE,
   walk fixes-of-fixes, summarize.
3. `series-version-diff(message_id_a, message_id_b?)` — auto-resolve
   sibling versions if `b` omitted.
4. `recent-reviewers-for(file_or_function)` — MAINTAINERS-style
   reviewer-suggestion seed.
5. `triage-incoming-patch(message_id)` — agent uses
   `lore_explain_patch` + `lore_critique_patch` (when it lands) to
   produce a Reviewed-by-style comment.

Claude Code gotcha: as of April 2026 the CLI does not auto-elicit
required prompt arguments (anthropics/claude-code#30733). Two
mitigations:
- Make every argument optional with sensible defaults (use
  `Annotated[X | None, Field(default=None, description=...)]`).
- Document the inline-args syntax in tool descriptions: users
  invoke `/mcp__kernel-lore__pre-disclosure-novelty-check
  fs/smb/server/smbacl.c smb_check_perm_dacl 90d`.

**LOC.** ~400 Python + 8 e2e tests.
**Days.** 1.

**Test gate.**
- `client.list_prompts()` returns all five with the right argument
  schemas.
- `client.get_prompt("pre-disclosure-novelty-check", {"file":
  "..."})` returns the expected user-message text.
- One stdio subprocess test confirms the prompt list survives the
  wire.

---

## Phase 12 — `ctx.sample()` for summarize / classify tools

**Goal.** Use the *client's* LLM via MCP sampling to do work we'd
otherwise need to either (a) ask the user to do, or (b) ship a
model on our side. No API key on our side, no per-tool model
lock-in, no inference budget on the deploy box.

**API sketch (`src/kernel_lore_mcp/tools/summarize.py`):**

```python
async def lore_summarize_thread(
    message_id: str,
    style: Literal["bullets", "narrative", "review-status"] = "bullets",
    ctx: Context | None = None,
) -> ThreadSummaryResponse:
    msgs = await asyncio.to_thread(reader.thread, message_id, 200)
    raw = "\n\n--- next message ---\n\n".join(format_for_summary(m) for m in msgs)
    if ctx is None:
        # Fallback when client doesn't support sampling: extractive
        # summary from subject + first 300 chars of each message.
        return _extractive_fallback(msgs, style)
    if not ctx.client_supports_extension("sampling"):
        return _extractive_fallback(msgs, style)

    result = await ctx.sample(
        f"Summarize this kernel review thread as {style}:\n\n{raw}",
        max_tokens=600,
        model_preferences={"intelligencePriority": 0.7,
                           "speedPriority": 0.3,
                           "costPriority": 0.5},
    )
    return ThreadSummaryResponse(
        thread_root=msgs[0].message_id,
        summary=result.text,
        model_used=result.model,  # client tells us what it ran
        tier_provenance=["metadata", "client-sampled"],
        ...
    )
```

Initial slate:
1. `lore_summarize_thread(message_id, style)` — bullets / narrative
   / review-status.
2. `lore_classify_patch(message_id)` — bug-fix vs feature vs
   refactor; returns `{class, confidence, justification}`.
3. `lore_explain_review_status(thread_root)` — "what's blocking this
   from being merged?" Reads trailers + replies + maintainer
   patterns, samples the client model for the synthesis.
4. `lore_extract_repro(message_id)` — given a bug report, pull the
   reproducer steps + crash signature into structured fields.

Discipline:
- **Always provide a non-sampling fallback.** Cursor as of April
  2026 doesn't advertise sampling support; we must degrade
  gracefully via extractive summarization (first-N-chars,
  TextRank-style sentence picking via a tiny pure-Python impl —
  no extra deps).
- **Cost discipline.** `model_preferences.costPriority=0.5` is the
  right default; never `0.0` (the client picks the most expensive
  model).
- **Use `ctx.client_supports_extension("sampling")`** to gate; FastMCP
  has the helper.

**LOC.** ~600 Python + 6 e2e tests + ~150 Python TextRank fallback.
**Days.** 2.

**Test gate.**
- E2E with FastMCP `Client` configured with a stub
  `sampling_handler` that returns a deterministic string — verify
  the tool calls it with the right messages + preferences.
- E2E without a sampling handler — verify the extractive fallback
  produces a non-empty summary.
- Live: against `claude --print` with a sampling-supporting client,
  verify `lore_summarize_thread` returns a model-generated bullets
  list. Mark `KLMCP_LIVE_AGENT=1` like the existing live tests.

---

## Phase 13 — Snippets, bulk reads, real cursor pagination

Three smaller items in one phase. All extend existing tools without
new infrastructure.

### 13a. Populate the `Snippet` field on every body-touching tool

Today `models.py` carries `Snippet { offset, length, sha256, text }`
but no tool sets it. Wire it via a KWIC (keyword-in-context)
extractor in `src/snippet.rs`:

```rust
pub struct SnippetSpan {
    pub offset: u64,        // byte offset into uncompressed body
    pub length: u64,        // span length in bytes
    pub text: String,       // pre-context + match + post-context
    pub body_sha256_prefix: String,  // first 16 hex chars for cite
}

pub fn kwic(body: &[u8], needle: &[u8], window: usize) -> Option<SnippetSpan> {
    let pos = memchr::memmem::find(body, needle)?;
    let start = pos.saturating_sub(window);
    let end = (pos + needle.len() + window).min(body.len());
    let text = String::from_utf8_lossy(&body[start..end]).into_owned();
    Some(SnippetSpan {
        offset: start as u64,
        length: (end - start) as u64,
        text,
        body_sha256_prefix: hex_prefix(&Sha256::digest(body), 16),
    })
}
```

Wire on `lore_substr_*`, `lore_regex`, `lore_patch_search`. The
existing tools call `kwic(body, needle, 80)` and stuff the result
into `SearchHit.snippet`.

**Why it matters.** The agent gets a verifiable citation: it can
quote the snippet, the client can re-fetch the body via
`lore://message/{mid}` (Phase 10), and a human can verify by
clicking through to `lore_url`. Every cite becomes a checkable
chain instead of "trust the LLM's recall."

**LOC.** ~120 Rust + 8 unit tests + ~80 Python wiring.
**Days.** 0.5.

### 13b. Bulk read APIs

Saves agents from N round-trips. New methods on `Reader`:

```rust
pub fn fetch_messages(&self, message_ids: &[String]) -> Result<Vec<Option<MessageRow>>>
pub fn fetch_bodies(&self, message_ids: &[String]) -> Result<Vec<Option<Vec<u8>>>>
```

Single Parquet scan instead of N point-lookups. PyO3 wraps each.
MCP tools:

```python
async def lore_get_many(message_ids: list[str], include_body: bool = False) -> RowsResponse: ...
```

**LOC.** ~150 Rust + ~80 Python + 4 e2e tests.
**Days.** 0.5.

### 13c. Wire HMAC-signed cursors into the actual tool responses

The router builds them; the tools never propagate. Plumbing:

- `SearchResponse.next_cursor` populated when results capped at `limit`.
- Tools accept `cursor: str | None`; when provided, decode +
  resume from `last_seen_score` / `last_seen_mid`.
- Server reads `KLMCP_CURSOR_KEY` from env at startup; if unset,
  generate a random per-process key (cursors don't survive
  restarts but they don't need to).

Touches `lore_search`, `lore_eq`, `lore_in_list`, `lore_substr_subject`,
`lore_substr_trailers`, `lore_regex`, `lore_activity`,
`lore_patch_search`, `lore_nearest`.

**LOC.** ~200 across Rust router + Python tools + 5 e2e tests.
**Days.** 1.

---

## Phase 14 — Streaming progress + per-call logs

**Goal.** Long tools (`lore_regex` over `body_prose`,
`lore_patch_search` near the cap, `lore_summarize_thread` with a
big thread) feel hung in the client UI. MCP gives us two cheap
fixes: progress notifications and per-call logs.

**API sketch:**

```python
async def lore_regex(..., ctx: Context | None = None) -> RowsResponse:
    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    if ctx:
        await ctx.info(f"regex: pattern={pattern!r} field={field}")
    # Currently a single asyncio.to_thread call. Refactor to a
    # streaming variant that walks Parquet row-groups and reports
    # progress per-group.
    rows = []
    async for batch_done, batch_total, partial in reader.regex_streamed(...):
        rows.extend(partial)
        if ctx:
            await ctx.report_progress(batch_done, batch_total,
                                       message=f"scanned {batch_done}/{batch_total} groups")
    return RowsResponse(...)
```

Implementation detail: Rust side gets a `regex_iter` that yields
`(processed, total, batch)` tuples; Python `asyncio.to_thread`s
each step.

Where to add:
- `lore_regex` — body_prose / patch fields can take seconds.
- `lore_patch_search` — confirmation pass is per-candidate.
- `lore_summarize_thread` — emit progress between sample chunks.
- `kernel-lore-embed` CLI — progress per batch already in stderr,
  but bind to `ctx.report_progress` for in-tool calls if we ever
  wrap it as a tool.

**LOC.** ~300 Rust streaming wrappers + ~100 Python + 4 e2e tests.
**Days.** 1.5.

**Test gate.**
- E2E: FastMCP `Client` with a `progress_handler` collecting
  `(progress, total, message)` triples; verify tool emits at least
  3 progress updates on a regex over the synthetic shard.
- E2E: `ctx.info` lines appear in the test client's log capture.

---

## Phase 15 — Elicitation for risky queries

**Goal.** Replace hard rejections with structured user prompts.
`lore_regex` rejects `.*foo` today; instead, elicit a list-or-since
narrowing from the user. `lore_thread` truncates at 200 messages
silently; instead, elicit "the thread is huge — keep last 30 days
or all?"

**API sketch:**

```python
class RegexNarrowing(BaseModel):
    list_filter: str | None = Field(default=None,
        description="Restrict to one mailing list (e.g. linux-cifs)")
    since_days: int | None = Field(default=None,
        description="Only messages newer than N days")
    proceed_unanchored: bool = False

async def lore_regex(..., ctx: Context | None = None):
    if anchor_required and pattern.starts_with(".*"):
        if ctx and ctx.client_supports_extension("elicitation"):
            r = await ctx.elicit(
                "Unanchored `.*` patterns scan the full term-dict and "
                "may be slow. Add a list/date filter or proceed anyway?",
                response_type=RegexNarrowing,
            )
            if isinstance(r, AcceptedElicitation):
                # apply r.value.list_filter / since_days / proceed_unanchored
                ...
            else:
                raise ToolError("regex narrowing declined / cancelled")
        else:
            raise ToolError("anchored-only mode rejected ...")  # current behavior
```

Initial slate of elicit-capable tools:
1. `lore_regex` on `body_prose` / `patch` — narrow before scanning.
2. `lore_thread` past `max_messages` — confirm scope.
3. `lore_nearest` when no embedding index exists — offer to invoke
   `kernel-lore-embed` via `ctx.sample` + `ctx.elicit`.
4. `lore_in_list` with >50 values — confirm full scan.

Discipline:
- **Always check `client_supports_extension("elicitation")`** —
  Codex CLI may not advertise it. Fall back to today's hard reject.
- **Default-accept on decline.** If the user closes the prompt
  without filling it, the tool errors out with the original
  rejection — we do *not* silently downgrade.

**LOC.** ~250 Python + 5 e2e tests with stub elicitation_handler.
**Days.** 1.

---

## Phase 16 — Roots + workspace awareness

**Goal.** Auto-scope queries to the user's current working tree.
If the user runs `claude` from inside a kernel checkout,
`ctx.list_roots()` returns those directories; `lore_activity` then
defaults to "files under any root" instead of "all of lore."

**API sketch (`src/kernel_lore_mcp/tools/activity.py`):**

```python
async def lore_activity(..., ctx: Context | None = None) -> ActivityResponse:
    if file is None and function is None and ctx is not None:
        try:
            roots = await ctx.list_roots()
        except Exception:
            roots = []
        if roots:
            # Auto-scope: walk the metadata tier for any touched_file
            # under any root; bound by since.
            ...
```

Lighter touch than other phases — most useful as a smarter default
on `lore_activity` and `lore_eq(field='touched_files')`. Don't
over-engineer; agents that want explicit scope still pass `file=`.

**LOC.** ~150 Python + 3 e2e tests.
**Days.** 0.5.

---

## Phase 17 — Tool-description quality + full annotation set

**Goal.** Make every tool description an LLM-optimized 3-section
blob: (a) one-sentence "what", (b) 1-2 worked example invocations,
(c) when-to-prefer-this-tool guidance. Set the rest of the MCP
annotation namespace.

Example for `lore_eq`:

```python
async def lore_eq(...) -> RowsResponse:
    """Exact-equality scan over one structured metadata column.

    Examples:
      lore_eq(field="from_addr", value="alice@example.com",
              list="linux-cifs", limit=50)
      lore_eq(field="touched_files", value="fs/smb/server/smbacl.c")
      lore_eq(field="fixes", value="abc123def")  # substring match for trailer cols

    Prefer this over lore_search when you know the exact column +
    value. Use lore_substr_trailers for case-insensitive trailer
    matches; use lore_in_list when you have multiple values.
    """
```

Annotations:

```python
mcp.tool(lore_eq, annotations={
    "readOnlyHint": True,
    "idempotentHint": True,
    "openWorldHint": True,           # corpus changes
    "destructiveHint": False,
    "title": "Exact-equality metadata scan",
})
```

Touches all 19 tools. Mostly docstring rewrites; +`outputSchema`
overrides on the few tools where pydantic auto-derivation produces
a too-loose schema (e.g. `lore_diff` could constrain `mode` to a
`Literal["patch","prose","raw"]`).

**LOC.** ~600 Python (mostly docstrings) + 1 review pass.
**Days.** 0.5.

**Test gate.**
- `tests/python/test_tool_descriptions.py` — assert every tool's
  description has "Examples:" and "Prefer this" sections (string
  presence). Catches regressions when a tool gets reformatted.

---

## Cross-cutting workstreams

These run in parallel with the phases above.

### CW-A. Streaming HTTP transport polish

We already declare `transport="http"` works; verify with the
official `mcp` SDK as a Streamable HTTP client (not just stdio).
Add `tests/python/test_http_subprocess.py` that boots
`kernel-lore-mcp --transport http --port :random`, hits
`/initialize`, calls a tool, asserts SSE frames carry our progress
notifications. Confirms the production deployment path actually
works end-to-end. **0.5 day.**

### CW-B. Tool-description doc generator

Auto-generate `docs/mcp/tools-reference.md` from the registered
tools' schemas + descriptions. Lets us keep one source of truth
and never re-check docs against code. **0.5 day.**

### CW-C. Live-CLI matrix expansion

Today we test `claude --print` and `codex exec`. Add Cursor's CLI
(when it ships a non-interactive mode), Zed's `mcp-test` helper if
present, and the `mcp-cli` reference client. Each as a skip-by-default
test under `KLMCP_LIVE_AGENT=1`. **0.5 day.**

### CW-D. Reciprocity infrastructure (deferred from Phase 6)

The original "publish derived-index snapshots so others bootstrap
from us" plan. Becomes urgent only if a public hosted instance
goes live. Punted here — re-prioritize when we commit to hosting.

---

## Sequencing + dependency graph

```
                 ┌──────────────────┐
                 │ 10 Resource      │  (independent; safe to ship first)
                 │    templates     │
                 └────────┬─────────┘
                          │
                          ▼
                 ┌──────────────────┐
                 │ 11 Prompts       │  (uses tools from phase 7+
                 │                  │   referenced in prompt body text)
                 └────────┬─────────┘
                          │
                          ▼
                 ┌──────────────────┐
                 │ 13a Snippets     │  (independent — Rust side only)
                 │ 13b Bulk reads   │
                 │ 13c Cursor pages │
                 └────────┬─────────┘
                          │
                          ▼
                 ┌──────────────────┐
                 │ 14 Progress + ctx│  (depends on 13b for batch shape)
                 │    logs          │
                 └────────┬─────────┘
                          │
                          ▼
                 ┌──────────────────┐
                 │ 12 Sampling +    │  (uses 14's progress for chunked
                 │    classify      │   summaries on big threads)
                 └────────┬─────────┘
                          │
                          ▼
                 ┌──────────────────┐
                 │ 15 Elicitation   │  (uses 12's sampling fallback path)
                 └────────┬─────────┘
                          │
                          ▼
                 ┌──────────────────┐
                 │ 16 Roots         │  (smaller, lands anywhere after 13b)
                 │ 17 Annotations + │
                 │    descriptions  │
                 └──────────────────┘
```

Total estimate: **~9 working days** for phases 10–17 + cross-cutting
A/B/C, in roughly two sprints. Phases are independent enough that
parts can run in parallel when wall-clock matters.

---

## Test coverage targets after this plan

| Category | Today | After |
|---|---|---|
| Rust unit | 60 | ~85 |
| Rust binary integration | 2 | 2 |
| Python e2e (in-process) | 67 | ~120 |
| Python live (opt-in) | 2 | 5–7 |
| Subprocess + stdio | 3 | 5 |
| HTTP subprocess | 0 | 3 |
| **Total always-on** | **132** | **~215** |

---

## Lessons learned (the retro)

Honest, written for the next-Claude-on-this-project. If you only
have five minutes, read these.

### What went well

1. **Reviewer-agent pivots were the highest-signal moments.**
   Spawning four reviewers (Linus / Greg / vendor engineer /
   security researcher) caught the c7g.xlarge 8 GB sizing mistake,
   the silent phrase-query degradation, the trigram-confirm-vs-zstd
   contradiction, and the embargo-leakage policy gap. Repeat before
   any release.
2. **Three-tier index + RRF was correct.** Most queries hit metadata
   only; trigram is the differentiated win; BM25 is the residual
   prose tool. Shape held up across implementation and live tests.
3. **Compressed store as source-of-truth was the highest-leverage
   architectural decision.** Every "we can rebuild without
   re-grok-pulling lore" claim has paid off.
4. **PyO3 0.28 + maturin + uv stack is honestly very pleasant.**
   `Python::detach` makes the GIL story trivial. Once `uv init
   --build-backend maturin` got the layout right, every iteration
   was a clean `uv run maturin develop --release && uv run pytest`.

### What went badly

1. **Doc-first scaffolding wasted real time.** First three sessions
   produced 60+ standards/docs files before any working code. The
   user pulled me back ("don't lecture me — build it") and that
   correction was right. Future projects: write the minimum doc to
   align on shape, ship code, then doc what shipped.
2. **Pivoting late to low-level primitives cost a phase.** I built
   `lore_activity` and `lore_thread` first; the user's "I want at
   least 3-5 different types of search" prompt pivoted the design.
   The orchestrators-on-top-of-primitives shape is right. Should
   have been the original direction.
3. **Real-CLI subprocess tests should have been week 1.** The
   in-process `Client` tests gave false confidence; the actual
   `claude --print` and `codex exec` integration needed
   `mcp__kernel-lore__lore_eq` tool-name munging, the
   `CODEX_HOME=/tmp` auth-loss workaround, the `--permission-mode
   bypassPermissions` discovery. Two hours of subprocess testing in
   Phase 1 would have shaped earlier decisions.
4. **Small avoidable research misses.** `blind_spots://` rejected by
   pydantic AnyUrl (underscores forbidden — RFC 3986). The
   `Hnsw` vs `HnswMap` API confusion in `instant-distance`. The
   `TopDocs::with_limit().order_by_score()` fluent API in tantivy
   0.26. Each was a 60s research check. Future: read the docs
   *before* the error.
5. **I paternalized at the wrong moments.** Multiple times I paused
   to ask "should we host?" or "is this a paid product?" when the
   user wanted execution. Once a direction is clear, ship it.
6. **I used ~35% of MCP-spec primitives.** The whole `@mcp.prompt`
   / `ctx.sample` / `ctx.elicit` / `ctx.report_progress` /
   resource-templates surface was sitting there in `fastmcp` 3.2.4
   the entire time. This plan closes that gap.

### What I'd tell the next session

- **Build the test harness *first*.** The subprocess + agentic-CLI
  smoke harness was the highest-trust artifact in the project. It
  catches things that no unit test or in-process Client test can.
- **Write tools as primitives, compose into workflows later.**
  Agents are better at composing than humans are at predicting
  composition.
- **Fail loudly, never silently.** Every tool that returns empty
  when it should error (phrase queries, missing index, dim
  mismatch) is a debugging nightmare for the agent. Current
  codebase is good about this; keep it that way.
- **`Context` is not optional.** Tools that take a `Context`
  parameter unlock progress, logs, sampling, elicitation, root
  discovery — it's free leverage. Add it everywhere.
- **Resources + prompts are token-free leverage.** Every workflow
  doc'd as a prompt is one less round of "tell the agent how to
  use the tools." Every resource is one less tool call.

### One-line summary

This codebase does the hard infrastructural things well (three
tiers, RRF, atomic state, PyO3 GIL discipline). The next chapter
is making the *MCP surface* match the quality of the underlying
engine: prompts as workflows, resources as citations, sampling as
client-LLM leverage, elicitation as graceful-narrowing, snippets +
cursors + bulk reads as the basic table-stakes.
