# Python Standards — kernel-lore-mcp

Adapted from KAOS Python standards (`273v/kaos-modules/docs/python/`).
Kept tight to what this project actually needs.

## Philosophy (five principles)

1. **Ecosystem first.** Prefer stdlib + a small well-chosen set of
   deps over adding libraries. One sharp module under our control
   beats a 50k-line library we use for one feature.
2. **Benchmark-driven.** Never optimize without measurement. Never
   declare "fast enough" without numbers. Profile, fix the
   bottleneck, document before/after.
3. **Typed and checked.** Every function has type annotations.
   `ty check` enforces them. Type errors are caught before tests
   run, not in production.
4. **Tested with real data.** Mocked tests prove the code matches
   the mock. Live tests prove the code works. The acceptance gate
   is `pytest` against a real ingested-lore corpus (synthetic
   fixtures fine for unit tests; full lore for integration).
5. **Rust where it matters.** Pure Python for I/O, MCP glue, and
   the FastMCP surface. Rust+PyO3 for the CPU-bound inner loops
   (ingestion, indexing, query dispatch). Clean interfaces that
   hide which side does the work.

## Stack pins (see `../../CLAUDE.md` for authoritative versions)

- Python **3.12+** floor (abi3-py312). 3.14 preferred runtime.
- `uv` exclusively for package management and command execution.
- `ruff` for format + lint.
- `ty` (Astral) for type checking — **not** mypy.
- `pytest` + `pytest-asyncio` + in-process `fastmcp.Client` for
  tests.
- `pydantic` v2 for every data boundary.
- `fastmcp` for the MCP server; `mcp` only for type imports.
- `structlog` for logging; stdio transport MUST log to stderr.

## Guides

| Guide | When to read |
|-------|--------------|
| [language.md](language.md) | Python version features we rely on |
| [uv.md](uv.md) | Every time you add a dep or run a command |
| [code-quality.md](code-quality.md) | Pre-commit pipeline |
| [testing.md](testing.md) | Before writing or changing tests |
| [naming.md](naming.md) | Variables, classes, MCP tools |
| [pyo3-maturin.md](pyo3-maturin.md) | Touching the Rust/Python boundary |
| [data-structures.md](data-structures.md) | Picking the right shape |

### Design (`design/`)

| Guide | Description |
|-------|-------------|
| [modules.md](design/modules.md) | Package layering, `__init__.py` exports |
| [boundaries.md](design/boundaries.md) | MCP boundary, PyO3 boundary, settings at edges |
| [dependencies.md](design/dependencies.md) | Import DAG, sibling rule, lazy imports |
| [errors.md](design/errors.md) | pydantic validation / router errors → MCP isError |
| [concurrency.md](design/concurrency.md) | async first; asyncio.to_thread for Rust calls |

### Libraries (`libraries/`)

Only the ones we use (KAOS has more we don't need):

| Guide | Description |
|-------|-------------|
| [pydantic.md](libraries/pydantic.md) | Every data boundary — request, response, settings |
| [fastmcp.md](libraries/fastmcp.md) | Server assembly, tool registration, transports |
| [structlog.md](libraries/structlog.md) | Logging with stdio-transport safety |
| [httpx.md](libraries/httpx.md) | Outbound HTTP (if we fetch beyond grokmirror) |

### Checklists (`checklists/`)

Run the one that matches your change class.

| Checklist | Stage |
|-----------|-------|
| [01-research.md](checklists/01-research.md) | Before touching code |
| [02-design.md](checklists/02-design.md) | New module or tool |
| [03-implement.md](checklists/03-implement.md) | Implementation |
| [04-test.md](checklists/04-test.md) | Testing |
| [05-quality.md](checklists/05-quality.md) | Pre-commit |
| [06-review.md](checklists/06-review.md) | Self-review before PR |
| [07-commit.md](checklists/07-commit.md) | Commit + push |
| [08-debug.md](checklists/08-debug.md) | When things misbehave |
| [09-optimize.md](checklists/09-optimize.md) | When perf matters |
| [10-document.md](checklists/10-document.md) | Doc updates |

## Quick decision trees

### What data structure should I use?

```
Is it a simple value or fixed-size group?
  → tuple, NamedTuple, or primitive

Does it need named fields?
  → @dataclass(frozen=True, slots=True)

Does it cross a system boundary (MCP, config, API)?
  → Pydantic BaseModel (never bare dict)

Does it persist across process restarts?
  → compressed store + metadata Parquet (Rust side)
```

### Should I reach for Rust+PyO3?

```
Is it CPU-bound?
  No  → Stay in Python
  Yes ↓
Called > 1000x or on large inputs?
  No  → Stay in Python
  Yes ↓
Did profiling show > 50% time in Python compute?
  No  → Optimize Python first
  Yes ↓
Is the algorithm stable?
  No  → Prototype in Python; port later
  Yes → Rust+PyO3
```

### Which type checker?

```
ty check — always. Not mypy.
```

### Which package manager?

```
uv — always. Not pip, not conda, not poetry.
```

## Cross-references

- [`../../CLAUDE.md`](../../../CLAUDE.md) — project proscriptions.
- [`../../TODO.md`](../../../TODO.md) — execution contract.
- [`../rust/index.md`](../rust/index.md) — Rust counterpart.
