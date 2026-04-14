# SDLC Checklists — kernel-lore-mcp

Adapted from `.../kaos-modules/docs/python/checklists/index.md`.

Progressive checklists for every stage of work on `kernel-lore-mcp`.
Each stage has 10-15 actionable items pointing at the detailed
guides in [../](../index.md) and the authoritative proscriptions in
[`../../../CLAUDE.md`](../../../CLAUDE.md).

Use these as a pre-flight check. Do not skip the non-negotiables.

---

## The Lifecycle

```
Research -> Design -> Implement -> Test -> Quality -> Review -> Commit
                                                        ↕
                                                 Debug ← Optimize → Document
```

---

## Stage Summary

### 1. [Research and Planning](01-research.md)

Read before you touch code.

- [ ] Read `CLAUDE.md` (proscriptions) and `TODO.md` (execution contract).
- [ ] Read the `docs/` subdir covering the area you are changing.
- [ ] Verify the work does not violate a proscription (stemming, SSE, git2-rs, side-effect imports, bare-dict tool returns, stdout-in-stdio, etc.).
- [ ] State the goal in one sentence; if you can't, you don't understand it.

### 2. [Design and Architecture](02-design.md)

Sketch the shape before the code.

- [ ] New MCP tool? Draft the pydantic request + response models first.
- [ ] Every tool return is a `BaseModel` — never a bare `dict`.
- [ ] Every hit carries the full required envelope (see CLAUDE.md "MCP server contract").
- [ ] Decide Python vs Rust; default to Python unless CPU-bound + profiled.

### 3. [Implementation](03-implement.md)

Follow project conventions.

- [ ] Explicit tool registration — no side-effect-import pattern.
- [ ] Lazy `from kernel_lore_mcp import _core` inside function bodies.
- [ ] `asyncio.to_thread(...)` around every call into `_core`.
- [ ] PyO3: `Python::detach` / `Python::attach` — never `allow_threads` / `with_gil`.
- [ ] Run format -> lint -> type-check -> test after every logical change.

### 4. [Testing](04-test.md)

Prove it works.

- [ ] `uv run maturin develop` before `pytest`.
- [ ] In-process `fastmcp.Client` fixture for tool tests.
- [ ] Synthetic fixtures under `tests/python/fixtures/` for unit tests.
- [ ] Live-tier tests run against a real lore shard, gated behind a marker.
- [ ] Assert specific content (message IDs, subject tags, tier provenance) — never just `len(results) > 0`.

### 5. [Code Quality](05-quality.md)

The pipeline, in order.

- [ ] `uv run ruff format src/kernel_lore_mcp tests/python`
- [ ] `uv run ruff check --fix src/kernel_lore_mcp tests/python`
- [ ] `uv run ty check src/kernel_lore_mcp tests/python`
- [ ] `cargo fmt --all`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test`
- [ ] `uv run pytest -v`

### 6. [Self-Review](06-review.md)

Re-read before staging.

- [ ] `git diff` — read every line.
- [ ] No `print()` in MCP-server code paths (stdio transport corrupts on stdout).
- [ ] No bare-dict returns from tools.
- [ ] Every proscription in CLAUDE.md still holds.
- [ ] Tests assert correctness, not existence.

### 7. [Commit and Push](07-commit.md)

Atomic, buildable, described.

- [ ] Pipeline passed (all steps above).
- [ ] `git diff --staged --name-only` — no `data/`, no `*.tantivy`, no `*.zst`, no `.env`.
- [ ] Message: `type(scope): description`, imperative, <=70 chars.
- [ ] One logical change per commit. "and" in the subject -> split.
- [ ] `uv.lock` and `Cargo.lock` committed when deps changed.

### 8. [Debugging](08-debug.md)

Observe, then hypothesize.

- [ ] Reproduce first; fix second.
- [ ] `--log-level DEBUG` on the FastMCP server for wire traces.
- [ ] `uv run python -c "..."` for quick object introspection.
- [ ] Stdio mode: check that every log goes to stderr; stdout must be framing only.
- [ ] Check generation-file / reader-reload before suspecting the router.

### 9. [Optimization](09-optimize.md)

Measure, change, measure.

- [ ] `cargo bench` (criterion) for Rust hot paths.
- [ ] `py-spy record` for the Python side of an MCP request.
- [ ] Never ship a "perf" change without before/after numbers in the commit.
- [ ] Batch across the PyO3 FFI boundary — per-call overhead adds up.

### 10. [Documentation](10-document.md)

Docs in the same commit as the code.

- [ ] Update the relevant `docs/` subdir (architecture, indexing, mcp, ops) in the commit that changes the code.
- [ ] Cross-check `TODO.md` — tick items as they finish.
- [ ] Update `src/kernel_lore_mcp/_core.pyi` when the Rust surface changes.
- [ ] Tool descriptions: tell the agent when to use it, what to call next, what NOT to use it for.

---

## Quick Reference: The Non-Negotiables

Pulled from `CLAUDE.md`. Never optional.

| Rule | Stage | Source |
|------|-------|--------|
| No stemmer, stopwords, asciifolding, typo tolerance | Design/Implement | `CLAUDE.md` "Tokenizer proscriptions" |
| No SSE transport; Streamable HTTP or stdio only | Design/Implement | `CLAUDE.md` "What NOT to use" |
| No git2-rs; gix only | Design/Implement | `CLAUDE.md` "What NOT to use" |
| No vendored `mcp.server.fastmcp`; standalone `fastmcp` only | Design/Implement | `CLAUDE.md` "What NOT to use" |
| No side-effect-import tool registration | Implement | `CLAUDE.md` last paragraph |
| No `allow_threads` / `with_gil` in new PyO3 code | Implement | `CLAUDE.md` "Stack pins" |
| No bare-dict returns from MCP tools | Implement/Review | `CLAUDE.md` "MCP server contract" |
| No stdout writes in stdio mode outside framing | Implement/Debug | `CLAUDE.md` "MCP server contract" |
| No FastAPI surface in v1 | Design | `CLAUDE.md` "What NOT to use" |
| No holding the GIL across heavy Rust calls | Implement | `CLAUDE.md` last paragraph |
| No committing `data/`, `*.tantivy`, `*.zst` | Commit | `CLAUDE.md` "Session-specific guidance" |
| `ty check` — never `mypy` | Quality | [../code-quality.md](../code-quality.md) |
| `uv` for everything — never `pip`, `poetry`, `conda` | All | [../uv.md](../uv.md) |
| Phrase queries on prose body rejected, not silently degraded | Design/Implement | `CLAUDE.md` "MCP server contract" |
| Regex queries must compile to DFA via `regex-automata` | Implement | `CLAUDE.md` "MCP server contract" |
| HMAC-signed pagination cursors | Implement | `CLAUDE.md` "MCP server contract" |

---

## Cross-references

- [`../index.md`](../index.md) — Python standards map.
- [`../../../CLAUDE.md`](../../../CLAUDE.md) — project proscriptions.
- [`../../../TODO.md`](../../../TODO.md) — execution contract.
