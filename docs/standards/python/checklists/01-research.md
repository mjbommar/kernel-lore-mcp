# Checklist: Research and Planning

Adapted from `.../kaos-modules/docs/python/checklists/01-research.md`.

Read before you touch code. The fastest way to break this project is
to start coding before you have read the proscriptions that govern
the area you are changing.

---

## Non-negotiables (from `CLAUDE.md`)

- [ ] You have **read `CLAUDE.md` in full** for this session. The
      "What NOT to use", "Tokenizer proscriptions", "MCP server
      contract", and "Session-specific guidance" sections are
      binding.
- [ ] You have **read `TODO.md`** and verified your change maps to
      an open item. If it doesn't, stop and ask — do not invent work.
- [ ] You have **read the relevant `docs/` subdir**: `architecture/`
      for tier contracts, `indexing/` for tokenizer + index shape,
      `mcp/` for tool schemas and routing, `ingestion/` for
      gix/mbox flow, `ops/` for deploy and security posture.

---

## Items

- [ ] **State the goal in one sentence.** If you cannot, stop and
      re-read `TODO.md`.

- [ ] **Verify no proscription is violated.** Check every rule in
      the CLAUDE.md "What NOT to use" and the "Do not …" paragraph
      at the bottom. If your plan touches stemming, SSE, git2-rs,
      vendored fastmcp, side-effect imports, bare-dict tool
      returns, or stdout-in-stdio — stop.

- [ ] **Search the codebase for prior art.** Use `Grep` / `Glob`
      on `src/kernel_lore_mcp/` and `src/` (Rust) for related
      code. A second implementation of something that already
      exists is an anti-goal.

- [ ] **Inspect existing pydantic models** in
      `src/kernel_lore_mcp/models.py`. Reuse or extend; don't
      duplicate shapes across tools.

- [ ] **Inspect the Rust surface.** Read `src/kernel_lore_mcp/_core.pyi`
      and the relevant `src/*.rs` module to see what is already
      exposed. `uv run python -c "from kernel_lore_mcp import _core; print(dir(_core))"`
      enumerates it at runtime.

- [ ] **Inspect external libraries before using them.**
      `uv run python -c "import fastmcp, inspect; print(inspect.signature(fastmcp.Client.call_tool))"`.
      For Rust crates, read the docs of the pinned version (see
      CLAUDE.md "Stack"). Do not assume an API — confirm it.

- [ ] **Identify the tier.** Metadata, trigram, or BM25? A query
      that touches multiple tiers must go through the router, not
      stitch tiers together inside a tool.
      > Ref: [../../../architecture/three-tier-index.md](../../../architecture/three-tier-index.md)

- [ ] **Determine whether Rust is needed.** CPU-bound? Called
      >1000x or on large inputs? Profiled? If yes to all, plan
      a PyO3 surface. If no, stay in Python.
      > Ref: [../pyo3-maturin.md](../pyo3-maturin.md)

- [ ] **Check concurrency shape.** I/O? `async def`. Calls into
      `_core`? `await asyncio.to_thread(...)`. Do not hold the
      GIL across a heavy Rust call.
      > Ref: [../design/concurrency.md](../design/concurrency.md)

- [ ] **Check the query grammar.** If you are adding an operator,
      read `docs/mcp/query-routing.md` first. Operators that cross
      tiers have explicit router contracts.

- [ ] **Sketch the test strategy.** Unit tests use synthetic
      fixtures in `tests/python/fixtures/`. Integration tests
      need a real lore shard. What does "correct" look like for
      this change?
      > Ref: [04-test.md](04-test.md)

- [ ] **Check the blind-spots register.** If your change could
      mask a known gap (security@ queue, syzbot pre-public, off-list),
      surface it via `blind_spots://coverage`, not per-response.

- [ ] **Confirm the change does not force a pin bump.** If it
      does, that is a project decision — log the reason now, not
      in the commit message.
