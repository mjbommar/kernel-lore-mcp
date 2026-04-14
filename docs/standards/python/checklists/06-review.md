# Checklist: Self-Review

Adapted from `.../kaos-modules/docs/python/checklists/06-review.md`.

Re-read your own code before staging. Tools catch syntax; you
catch intent.

---

## Non-negotiables (from `CLAUDE.md`)

Re-confirm each rule against the diff. If a line in the diff
violates any, stop.

- [ ] No stemmer / stopwords / asciifolding / typo tolerance.
- [ ] No SSE transport; Streamable HTTP or stdio only.
- [ ] No git2-rs / libgit2 — gix only.
- [ ] No vendored `mcp.server.fastmcp` — standalone `fastmcp`.
- [ ] No side-effect-import tool registration — explicit in
      `server.py`.
- [ ] No `allow_threads` / `with_gil` in new PyO3 code.
- [ ] No bare-dict returns from MCP tools — pydantic models only.
- [ ] No stdout writes in stdio mode outside MCP framing.
- [ ] No FastAPI surface in v1.
- [ ] No holding the GIL across heavy Rust calls.
- [ ] No `data/`, `*.tantivy`, `*.zst` in the staged diff.

---

## Items

- [ ] **Read the full diff.** `git diff` and `git diff --staged`.
      Every line. Look for typos, debug artifacts, commented-out
      code, `TODO`s you forgot.

- [ ] **Remove debug artifacts.** Grep before staging:
      ```bash
      rg "breakpoint\(|print\(|dbg!|import pdb" src/ tests/
      ```

- [ ] **No secrets staged.** `.env`, API keys, HMAC keys,
      tokens. Check `git diff --staged --name-only`.

- [ ] **No generated artifacts staged.** `data/`, `target/`,
      `*.tantivy`, `*.zst`, `*.so`, `*.parquet`. `.gitignore`
      should already catch these — verify.

- [ ] **Naming consistency.** Same concept -> same name across
      the module. Plural for collections, singular for items.
      Tools follow `lore_{action}`; models follow
      `{Tool}Request` / `{Tool}Response`.

- [ ] **Every tool returns a pydantic model.** Not `dict`, not
      `TypedDict`, not `Any`. Grep the tool files for
      `-> dict` — should never be the return type of a tool
      handler.

- [ ] **Every hit carries the required envelope.** `message_id`,
      `cite_key`, `from_addr`, `lore_url`, `subject_tags[]`,
      `is_cover_letter`, `series_version`, `series_index`,
      `patch_stats` (if `has_patch`), `snippet{...}`,
      `tier_provenance[]`, `is_exact_match`,
      `cross_posted_to[]`. Cross-reference against
      `docs/mcp/tool-schemas.md`.

- [ ] **Defaulted filters echoed.** `default_applied: ["rt:5y"]`
      (or whatever defaults fired) is present whenever the
      router applied defaults.

- [ ] **Errors follow the three-part rule.** What went wrong,
      how to fix it, what to try instead. Every new error site
      gets a message an AI agent can act on.

- [ ] **Tool descriptions are actionable.** Read each one — does
      it tell the agent WHEN to use the tool, WHEN NOT to, and
      what to call before/after?

- [ ] **Concurrency correctness.** Every Rust call goes through
      `await asyncio.to_thread(...)`. No `await` on a sync
      function. No forgotten `await` returning a coroutine.
      Stat-and-reload wraps each query entry.

- [ ] **Reader-reload discipline applied.** If you added a query
      path, it stats the generation file and calls
      `reader.reload()` when the counter advanced.

- [ ] **`_core.pyi` matches the Rust surface.** If you added a
      `#[pyfunction]`, a stub exists.

- [ ] **Commit atomicity.** Does the diff describe one logical
      change? "and" in the subject line -> split.

- [ ] **Read it like a stranger.** If someone with no context
      opened this PR, would the names, structure, and error
      messages make sense? If not, fix now — not in a follow-up.
