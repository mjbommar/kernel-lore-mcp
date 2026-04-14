# Checklist: Debugging

Adapted from `.../kaos-modules/docs/python/checklists/08-debug.md`.

Observe first. Hypothesize second. Verify the fix with a test.

---

## Non-negotiables (from `CLAUDE.md`)

- [ ] **Stdio mode: logs go to stderr only.** If you are
      debugging a stdio client, any `print()` anywhere in the
      process corrupts the protocol. Use `structlog` (configured
      to stderr) or direct `sys.stderr.write`.
- [ ] **Never commit debug prints.** Removal is part of review.

---

## Items

- [ ] **Reproduce first.** Write a minimal `uv run python -c`
      snippet or a pytest case that triggers the bug. No
      repro -> no verified fix.

- [ ] **Read the error message.** Our three-part errors usually
      name the fix. If the error is a bare Rust panic, that is
      itself a bug — panics at the PyO3 boundary should never
      escape.

- [ ] **Check the transport.** Running over stdio? Any stray
      stdout write breaks framing — symptoms look like "client
      hangs" or "invalid JSON-RPC". Start with
      `--log-level DEBUG` and watch stderr.

- [ ] **Enable MCP wire tracing.**
      ```bash
      uv run python -m kernel_lore_mcp --transport streamable-http --log-level DEBUG
      ```
      For a stdio client, run the server with the same flag and
      tail the stderr log.

- [ ] **Inspect objects in a REPL.**
      ```bash
      uv run python -c "from kernel_lore_mcp import _core; print(dir(_core))"
      uv run python -c "from kernel_lore_mcp.models import SearchResponse; import inspect; print(inspect.signature(SearchResponse))"
      ```

- [ ] **Use `breakpoint()` + pdb.** Set at the failure site.
      `p obj`, `w` (stack), `u` / `d`, `n` / `s`. Remove before
      commit.

- [ ] **Read the stack bottom-up.** The actual error is at the
      bottom. Your code is usually mid-stack.

- [ ] **Check async context.** Missing `await` -> coroutine
      leaked as a return value. Blocking call in async -> event
      loop stalls. Call into `_core` without `asyncio.to_thread`
      -> UI-thread freeze.
      > Ref: [../design/concurrency.md](../design/concurrency.md)

- [ ] **Check settings resolution.** Is `KernelLoreSettings`
      picking up the right `KLMCP_*` env vars? Print the
      resolved object at startup. Remember `.env` is only loaded
      if present.

- [ ] **Check generation / reader reload.** Stale results? The
      generation counter may not have advanced (ingest unit not
      running) or the reader may not be reloading (missed
      `stat()` on request entry).

- [ ] **Check the tier provenance.** If a query returns
      unexpected hits, inspect `tier_provenance[]` on each hit.
      Router dispatching to the wrong tier is a common cause.

- [ ] **Check the HMAC cursor.** Pagination broken? Try a fresh
      query with no cursor — if that works, the cursor HMAC
      verification is the issue (key rotation, tampering, or
      cross-environment bleed).

- [ ] **Check thread safety.** `tantivy::IndexWriter` is
      single-writer. If you see lock contention, you have two
      writers — check that only `klmcp-ingest` is running.

- [ ] **Check build freshness.** After Rust changes, did you run
      `uv run maturin develop`? Stale `_core.so` is the most
      common false bug.

- [ ] **Verify the fix with a test.** Failing-before,
      passing-after. Commit the test with the fix.

- [ ] **Search for the pattern elsewhere.** If the bug is a
      pattern (missing `await`, wrong error type, forgotten
      reload), grep the codebase and fix every instance.
