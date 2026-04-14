# uv and uvx — The Python Toolchain

Adapted from `../../../../../273v/kaos-modules/docs/python/uv.md`.
Single-package project; monorepo sections dropped. Maturin integration
section kept and tuned to our actual pins.

See also: [Rust counterpart](../rust/cargo.md) for `cargo` discipline.

---

## The Rule

All Python work in kernel-lore-mcp goes through `uv` or `uvx`. There are
no exceptions.

**Never use `pip`, `pip install`, `python -m venv`, `virtualenv`, `conda`,
`pipenv`, `poetry`, or system Python directly.**

| Instead of... | Use... |
|---|---|
| `pip install package` | `uv add package` |
| `pip install -e .` | `uv sync` (editable install happens via maturin) |
| `pip install -r requirements.txt` | `uv sync` (reads `pyproject.toml` + `uv.lock`) |
| `python -m venv .venv` | `uv venv` |
| `python script.py` | `uv run python script.py` |
| `python -m pytest` | `uv run pytest` |
| `pip install ruff && ruff check` | `uvx ruff check` (or `uv run ruff check`) |
| `pip freeze > requirements.txt` | `uv lock` (produces `uv.lock`) |
| `python --version` | `uv python list` |

### Why uv Exclusively

1. **Deterministic resolution.** `uv.lock` captures the full dependency
   graph with hashes. Every developer and CI run gets identical
   packages.
2. **Fast.** uv resolves and installs 10-100x faster than pip. Cold
   installs that take minutes with pip take seconds with uv.
3. **Unified.** One tool for Python version management, virtual
   environments, dependency resolution, locking, and command execution.
4. **Maturin-aware.** `uv sync` with the `[tool.uv] cache-keys` block
   rebuilds the Rust extension when `src/**/*.rs` or `Cargo.toml`
   changes — no `maturin develop` needed most of the time.
5. **No ambient state.** `uv run` always uses the project's virtual
   environment. No `source .venv/bin/activate` to forget, no wrong
   Python on `$PATH`.

---

## Project Setup

### Python Version

The project pins Python **3.12 minimum** (`requires-python = ">=3.12"`)
and targets 3.14 as the preferred runtime.

```bash
# List available Pythons
uv python list

# Install a specific version
uv python install 3.14

# Pin project to a version (writes .python-version)
uv python pin 3.14
```

`.python-version` in the repo root picks the interpreter `uv` uses by
default. Bumping it is a project decision.

### Virtual Environment

```bash
# uv creates .venv automatically on first `uv sync` or `uv run`
# Explicit creation (rarely needed):
uv venv

# With the free-threaded build (tracked, not deployed — see language.md)
uv venv --python 3.14t
```

---

## Dependency Management

### Adding Dependencies

```bash
# Runtime dependency
uv add httpx

# With version constraint
uv add "pydantic>=2.7,<3"

# Dev dependency (not published)
uv add --group dev pytest

# Build-time dep (we keep maturin in the dev group so CI can rebuild)
uv add --group dev "maturin>=1.13,<2"
```

### Removing Dependencies

```bash
uv remove httpx
```

### Locking and Syncing

```bash
# Lock: resolve all dependencies, write uv.lock
uv lock

# Check if lock is fresh (CI)
uv lock --check

# Sync: install from lockfile into .venv
uv sync

# Sync including dev group (what you want for local dev)
uv sync --group dev

# Sync all groups
uv sync --all-groups

# Frozen sync for CI (no resolution, just install)
uv sync --group dev --frozen
```

### Upgrading

```bash
# Upgrade all dependencies — coordinate with the pin table in CLAUDE.md
uv lock --upgrade
uv sync

# Upgrade a specific package (log the reason in the commit message)
uv lock --upgrade-package fastmcp
uv sync
```

Any dep bump is a project decision, not a casual `uv lock --upgrade`.
See `CLAUDE.md` for the authoritative pin table.

---

## Dependency Declaration

### Two Categories — Never Mix

**Runtime deps** (published): declared in `[project] dependencies`.

```toml
[project]
dependencies = [
    "fastmcp>=3.2,<4",
    "mcp>=1.27,<2",
    "pydantic>=2.7,<3",
    "pydantic-settings>=2.5,<3",
    "anyio>=4.4",
    "httpx>=0.28",
    "structlog>=24.4",
    "prometheus-client>=0.21",
]
```

**Dev groups** (contributors only, never published):

```toml
[dependency-groups]
dev = [
    "pytest>=8",
    "pytest-asyncio>=0.24",
    "respx>=0.21",
    "freezegun>=1.5",
    "ruff>=0.11",
    "ty>=0.0.1a17",
    "maturin>=1.13,<2",
]
```

> Do NOT put `pytest`, `ruff`, or `ty` in `[project] dependencies`.
> That ships them to users who `pip install kernel-lore-mcp`.

We do not currently use `[project.optional-dependencies]` — the project
is a single installable unit. If we add an optional feature flag
later, it goes there, not in `dependency-groups`.

---

## Running Commands

### `uv run` — Project-Scoped Execution

`uv run` ensures commands execute in the project's virtual environment
with all dependencies available. It auto-syncs if needed — including
rebuilding the Rust extension via maturin when the cache keys indicate
source change.

```bash
# Run tests
uv run pytest tests/python -v

# Run the server in stdio mode
uv run kernel-lore-mcp --transport stdio

# Run the server in Streamable HTTP (default bind 127.0.0.1)
uv run kernel-lore-mcp --transport http --port 8787

# Run a script
uv run python scripts/check_generation.py

# Run with environment variable
KLMCP_BIND=0.0.0.0 uv run kernel-lore-mcp --transport http

# Quick REPL check
uv run python -c "import kernel_lore_mcp; print(kernel_lore_mcp.__version__)"

# Check the Rust extension surface
uv run python -c "from kernel_lore_mcp import _core; print(dir(_core))"
```

### `uvx` — Ephemeral Tool Execution

`uvx` runs tools without installing them into the project. Ideal for
one-off linting or utility commands outside the project venv.

```bash
# Lint
uvx ruff check src/kernel_lore_mcp tests/python

# Format
uvx ruff format src/kernel_lore_mcp tests/python

# Type check
uvx ty check src/kernel_lore_mcp tests/python
```

### When to Use `uv run` vs `uvx`

| Scenario | Use |
|---|---|
| Running project code (server, tests, REPL) | `uv run` |
| Running dev tools pinned in `[dependency-groups]` | `uv run` |
| Running tools outside the project | `uvx` |
| One-off formatting in a different directory | `uvx` |

We pin `ruff` and `ty` in `dev`, so `uv run ruff check` and `uvx ruff
check` both work. Prefer `uv run` in CI so you exercise the pinned
version.

---

## Maturin Integration

This is the load-bearing section for kernel-lore-mcp. The project is a
**mixed Python+Rust package** built by maturin. The Python source lives
in `src/kernel_lore_mcp/`; the Rust extension is compiled into
`src/kernel_lore_mcp/_core*.so`.

### The `[tool.maturin]` Block (Verbatim from pyproject.toml)

```toml
[build-system]
requires = ["maturin>=1.13,<2.0"]
build-backend = "maturin"

[tool.maturin]
module-name = "kernel_lore_mcp._core"
python-packages = ["kernel_lore_mcp"]
python-source = "src"
features = ["pyo3/extension-module"]
```

Notes:
- `module-name = "kernel_lore_mcp._core"` — the Rust extension imports
  as `kernel_lore_mcp._core`, **not** `_native`. See
  [naming.md](naming.md).
- `python-source = "src"` — Python package lives at
  `src/kernel_lore_mcp/`.
- `features = ["pyo3/extension-module"]` — wires PyO3's extension-module
  feature automatically; matches the `optional = true` on `pyo3` in
  `Cargo.toml`.

### The `[tool.uv] cache-keys` Block

```toml
[tool.uv]
cache-keys = [
    { file = "pyproject.toml" },
    { file = "src/**/*.rs" },
    { file = "Cargo.toml" },
    { file = "Cargo.lock" },
]
```

This causes `uv sync` / `uv run` to rebuild the extension when any
`.rs` file, `Cargo.toml`, `Cargo.lock`, or `pyproject.toml` changes.
Trade-off: `uv sync` always builds in release mode (no debug
assertions). For faster debug iteration, use `maturin develop`
directly.

### Build Workflows

**Debug iteration** (fastest compile, unoptimized — use while working
on Rust):

```bash
uv run maturin develop
uv run pytest tests/python -v
```

**Release iteration** (optimized, what CI and production use):

```bash
uv run maturin develop --release
uv run pytest tests/python -v
```

**Cache-key path** (mostly-Python day, let uv handle Rust):

```bash
# Edit src/*.rs, then:
uv run pytest tests/python -v   # uv detects .rs change, rebuilds release
```

**Free-threaded build** (tracked, not deployed — abi3 incompatible):

```bash
# Requires src/lib.rs to drop abi3-py312 from pyo3 features
uv run maturin develop --no-default-features --features pyo3/gil-refs
```

Do not commit a `--no-default-features` build to production until
PEP 803 ("abi3t") lands.

---

## Common Patterns

### Quick Introspection

```bash
# Check what's installed
uv run python -c "import fastmcp; print(fastmcp.__version__)"

# Inspect the Rust extension
uv run python -c "import kernel_lore_mcp._core as c; print([x for x in dir(c) if not x.startswith('_')])"

# Check a function signature on the Python side
uv run python -c "
import inspect
from kernel_lore_mcp.tools.search import lore_search
print(inspect.signature(lore_search))
"

# Confirm which interpreter uv is using
uv run python -c "import sys; print(sys.version, sys.executable)"
```

### CI Configuration

```bash
#!/usr/bin/env bash
set -euo pipefail

# Install dependencies (fast, cached, reproducible)
uv sync --group dev --frozen

# Build the Rust extension in release mode
uv run maturin develop --release

# QA pipeline — package paths match our layout
uv run ruff format --check src/kernel_lore_mcp tests/python
uv run ruff check src/kernel_lore_mcp tests/python
uv run ty check src/kernel_lore_mcp tests/python
uv run pytest tests/python -v --tb=short
```

The `--frozen` flag skips resolution and installs directly from
`uv.lock`. Use this in CI for speed and reproducibility.

---

## Cross-references

- [code-quality.md](code-quality.md) — full pre-commit pipeline
- [pyo3-maturin.md](pyo3-maturin.md) — deeper on the Rust/Python boundary
- [Rust counterpart](../rust/cargo.md) — `cargo` workflow for the
  pure-Rust side (tests, reindex binary)
