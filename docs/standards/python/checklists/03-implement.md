# Checklist: Implementation

Adapted from `.../kaos-modules/docs/python/checklists/03-implement.md`.

Write clean, typed, tested code that follows `docs/standards/python/`
and CLAUDE.md proscriptions.

---

## Non-negotiables (from `CLAUDE.md`)

- [ ] **Lazy `_core` import.** Import the PyO3 extension inside the
      function that uses it, not at module top level. Keeps
      pure-Python imports cheap and makes optional features truly
      optional.
      ```python
      async def lore_search(req: SearchRequest) -> SearchResponse:
          from kernel_lore_mcp import _core  # lazy
          rows = await asyncio.to_thread(_core.router_search, req.query, ...)
      ```
- [ ] **Never hold the GIL across heavy Rust calls.** On the Rust
      side use `Python::detach` (PyO3 0.28). On the Python side
      wrap calls into `_core` with `asyncio.to_thread(...)`.
- [ ] **No side-effect-import tool registration.** Wire tools in
      `server.py` explicitly: `mcp.tool(...)(lore_search)`.
- [ ] **Stdio mode logs to stderr.** `structlog` is configured that
      way in `logging_.py` — do not override. No `print()` in tool
      code. Any byte on stdout outside MCP framing corrupts the
      protocol.
- [ ] **Tools return pydantic models.** Never a bare `dict`.
- [ ] **Settings load at the edge.** `KernelLoreSettings()` is
      constructed once in `__main__.py` / `server.py`. Pass the
      typed object into internals. No `os.environ.get(...)` deep
      in library code.

---

## Items

- [ ] **Write a failing test first.** Use `tests/python/fixtures/`
      for synthetic mbox/shard fixtures. The test is the
      specification.
      > Ref: [04-test.md](04-test.md)

- [ ] **Implement the pydantic models first** (if not already
      added in design). `ConfigDict(frozen=True)` on response
      types. `SecretStr` for any secret field. Type every field.
      > Ref: [../libraries/pydantic.md](../libraries/pydantic.md)

- [ ] **Type-annotate every function.** Parameters, returns,
      and non-obvious locals. Modern syntax: `str | None`,
      `list[int]`, `dict[str, Any]` only at boundaries.
      > Ref: [../language.md](../language.md)

- [ ] **Follow naming conventions.** Tools: `lore_{action}`.
      Functions: `verb_object()`. Booleans: `is_`/`has_`/`can_`.
      Numerics include units (`timeout_seconds`, `max_hits`).
      > Ref: [../naming.md](../naming.md)

- [ ] **Use `async def` for every tool handler and every I/O
      path.** Wrap calls into `_core` with
      `await asyncio.to_thread(...)`. Never `time.sleep`,
      never sync HTTP inside async code.
      > Ref: [../design/concurrency.md](../design/concurrency.md)

- [ ] **Three-part error messages at the MCP boundary.** What
      went wrong, how to fix it, what to try instead. Map
      `_core` errors to typed MCP errors in one place per tool.
      > Ref: [../design/errors.md](../design/errors.md)

- [ ] **Register the tool explicitly in `server.py`.** Pass
      `annotations={"readOnlyHint": True}` for every v1 tool.
      Pass a clear description ("when to use" + "when NOT to
      use" + "what to call next").

- [ ] **Echo defaulted filters.** If the router applied
      `rt:5y` (or any default), the response must include
      `default_applied: ["rt:5y", ...]`.

- [ ] **Generate pagination cursors via the HMAC helper.** Keyed
      by `KLMCP_CURSOR_KEY`. Reject unsigned / tampered cursors
      with a typed error.

- [ ] **Reject phrase queries on prose** with
      `Error::QueryParse` ("no positions on prose body; use
      `nq:` for tokens or `/regex/` on the patch tier"). Never
      silently degrade to non-phrase.

- [ ] **PyO3 0.28 naming.** In Rust use `Python::detach` (not
      `allow_threads`) and `Python::attach` (not `with_gil`).
      If you see the old names in generated code, it's either
      a wrong crate version or an auto-port miss — fix it.

- [ ] **Update `_core.pyi` with new Rust symbols.** Every new
      `#[pyfunction]` / `#[pyclass]` needs a stub with
      parameter and return types. `py.typed` is already in
      the package.
      > Ref: [../pyo3-maturin.md](../pyo3-maturin.md)

- [ ] **Respect reader-reload discipline.** At the start of every
      query-serving code path, stat the generation file and call
      `reader.reload()` if the u64 advanced. This is the only way
      multi-worker uvicorn stays coherent.

- [ ] **Import order:** stdlib -> third-party -> local. Heavy deps
      lazy-imported inside functions. `TYPE_CHECKING` for type-only
      imports that would otherwise pull in a dependency.
      > Ref: [../design/dependencies.md](../design/dependencies.md)

- [ ] **Use structured logging.** `from kernel_lore_mcp.logging_
      import get_logger`. Bind `request_id`, `tool`, and tier
      fields. Never `print`.

- [ ] **Use `uv run` for everything.** `uv run python ...`,
      `uv run pytest`, `uv run ruff`, `uv run ty`. Never bare
      `python` / `pip`.
      > Ref: [../uv.md](../uv.md)

- [ ] **Run the pipeline after every logical change.** Do not
      batch. `ruff format` -> `ruff check --fix` -> `ty check`
      -> `cargo fmt` -> `cargo clippy` -> `cargo test` ->
      `uv run pytest`.
      > Ref: [05-quality.md](05-quality.md)
