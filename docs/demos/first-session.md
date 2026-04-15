# First session — 10 queries to try

You've run the quick-start in [`../../README.md`](../../README.md)
or runbook §0A. Your agent (Claude Code, Codex, Cursor, Zed) is
wired to `kernel-lore-mcp` via stdio. Here are 10 concrete queries
that together exercise the whole MCP surface — every tool family,
every resource template, every sampling-backed capability, every
slash-command prompt.

Each item lists:
- the agent prompt (what you type),
- which tools/resources/prompts it should trigger,
- a sketch of what a correct response looks like.

The personal-scoped first-sync (scripts/grokmirror-personal.conf)
covers linux-cifs, linux-nfs, linux-security-module, linux-
hardening, and bpf. Use these as the substrates; substitute your
preferred subsystems if you widened the mirror.

---

## 1. Basic activity scan

> Find every message touching `fs/smb/server/smbacl.c` in the last
> 90 days. Include trailer info if present. Just list message-ids,
> subjects, and from-addresses.

- Expect: tool `lore_activity` called with `file=fs/smb/server/smbacl.c`.
- Response cites ≥ 1 message with `message_id`, `lore_url`, and a
  populated `reviewed_by[]` / `signed_off_by[]` list on patches
  that carry trailers.

## 2. Exact-column primitive (cheapest path)

> Use lore_eq to find every message where from_addr is exactly
> namjae@kernel.org on linux-cifs. Reply with only the subject
> lines.

- Exercises: `lore_eq` primitive; cheap metadata scan.
- Confirms the `typo → did-you-mean` error shape if you swap
  `from_addr` for `from_add` (Sprint 0 / CW-D behavior).

## 3. Trigram-backed patch substring search

> Search patch bodies (not prose) for the literal token
> `smb_check_perm_dacl` across every list. Show the top 5 hits
> with KWIC snippets.

- Expect: `lore_patch_search(needle=smb_check_perm_dacl)`.
- Every hit carries a `snippet` with `offset`, `length`,
  `sha256`, `text` — the KWIC payload from Sprint 0 / H.
- Snippets contain the needle verbatim inside a ~200-char window.

## 4. Regex over prose, with the DFA guardrail

> Find linux-nfs messages where the prose body matches the regex
> `CVE-2024-\d{4,}`. Use lore_regex.

- Exercises: DFA-only regex engine; rejects backref attempts.
- If you try `(\w+)\1` instead, you get `[unknown_regex_field]` —
  wait, no, you get a DFA-compile error. Try it: the error
  message tells you what to do.

## 5. Series version chain

> Pick any patch in linux-security-module from the last 30 days.
> Show me the full vN chain using lore_series_timeline.

- Exercises: `lore_series_timeline` + cite_key propagation.
- If the chain has cover letters, `is_cover_letter: true` is set.
- Follow-up: "and now lore_patch_diff between v1 and v2." Calls
  both tools; response carries a unified diff.

## 6. Read a message as an MCP resource (Phase 10)

> Read the resource `lore://message/<some-real-mid-from-#1>` and
> paste the Message-ID and Date headers.

- Exercises: Phase 10 RFC-6570 templates. Claude Code / Codex both
  have native `read_mcp_resource`.
- MIME type returned is `text/plain`.
- The patch variant works too:
  `lore://patch/<mid>` returns MIME `text/x-diff` (we monkey-
  patched FastMCP's template-mime-drop bug in Phase 10).

## 7. Sampling-backed classification with fallback

> Call lore_classify_patch on <some patch mid>. Tell me the label
> and which backend produced it.

- If the client advertises sampling (Claude Code does): backend =
  `"sampled"`, the label comes from the client LLM.
- If not (Codex stdio sometimes, Cursor often): backend =
  `"extractive"`, the label comes from the Phase 12 rule scanner.
- Either way the label is one of {bugfix, feature, cleanup, doc,
  test, merge, revert, backport, security, unknown}.

## 8. Thread summarization (dual-path)

> Summarize the thread starting from `<some cover-letter mid>`.
> Max 5 sentences.

- Exercises: `lore_summarize_thread` — Phase 12.
- Same sampled/extractive split as #7; `backend` is surfaced on
  the response.

## 9. Slash-command prompt (Claude Code only)

In Claude Code, type:

> /mcp__kernel-lore__klmcp_recent_reviewers_for file_path=fs/smb/server/smbacl.c window_months=6

- Exercises: Phase 11 prompt surface.
- Claude Code renders the prompt body as a user message; the model
  follows the enumerated steps and calls `lore_activity` +
  trailer-scan tools.
- If you're on Codex / Cursor / Zed and the slash menu doesn't
  appear, just paste the prompt text manually — the model
  follows the same instructions.

## 10. Freshness audit

> What's the `freshness` block on the most recent response? And
> how old is my index overall? (If needed, read the
> blind-spots://coverage resource.)

- Every tool response carries `freshness.as_of`,
  `freshness.lag_seconds`, `freshness.generation`.
- Resource `blind-spots://coverage` narrates what the index does
  NOT cover (private security@kernel.org, distro backports,
  syzbot pre-public, etc.).
- Back in the shell:
  `kernel-lore-mcp status --data-dir ~/klmcp-data | jq .`
  gives the same numbers without burning agent tokens.

---

## What each exercise proves about the stack

| # | Invariant under test |
|---|---|
| 1 | metadata tier + trailer extraction + lore_url citations |
| 2 | low-level primitives + LoreError did-you-mean recovery |
| 3 | trigram tier + KWIC snippet offsets + sha256 provenance |
| 4 | DFA-only regex (safe for untrusted input) |
| 5 | series semantics + patch-vs-patch diff |
| 6 | RFC-6570 resource templates + MIME typing |
| 7 | ctx.sample with graceful extractive fallback |
| 8 | sampling-backed summarization |
| 9 | server-provided prompts as slash commands |
| 10 | freshness marker end-to-end |

If all 10 behave: your deployment covers ~24/36 of the
[best-in-class MCP scorecard](../research/2026-04-14-best-in-class-mcp-survey.md)
and every surface shipped through Sprint 0 + Phase 10 + Phase 11 +
Phase 12.

## If something is wrong

- `./scripts/agentic_smoke.sh local` — runs the whole MCP surface
  offline, in <3 s, with no API keys. FAIL there means your
  install is broken, not the agent.
- `kernel-lore-mcp status --data-dir $KLMCP_DATA_DIR` — quick
  freshness probe without booting HTTP.
- Agent-side: turn on `--debug` (Claude Code) or
  `RUST_LOG=debug` + `--verbose` (codex exec) and re-run.
