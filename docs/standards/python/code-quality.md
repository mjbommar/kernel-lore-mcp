# Code Quality — ruff, ty, and the Pre-Commit Pipeline

Adapted from `../../../../../273v/kaos-modules/docs/python/code-quality.md`.

Every change to `kernel-lore-mcp` runs a strict QA pipeline before it
hits a commit: **format → lint → type check → test**. All four steps
must pass. No exceptions, no `--no-verify`.

See also: [Rust counterpart](../rust/code-quality.md) for the `cargo
fmt → clippy → test` pipeline on the Rust side.

---

## The Pipeline

```bash
# Our package paths: src/kernel_lore_mcp and tests/python
uv run ruff format src/kernel_lore_mcp tests/python
uv run ruff check --fix src/kernel_lore_mcp tests/python
uv run ty check src/kernel_lore_mcp tests/python
uv run pytest tests/python -v
```

Run in this order. Formatting before linting avoids false positives.
Type checking before testing catches errors tests might miss.

For mixed Python+Rust changes, run `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test --no-default-features` first so the Rust QA loop stays tight.

---

## ruff — Format and Lint

ruff replaces Black (formatting), isort (imports), flake8 (linting),
and most of pylint with a single Rust-powered tool. We pin `ruff>=0.11`.

### Formatting

```bash
# Format in place
uv run ruff format src/kernel_lore_mcp tests/python

# Check without modifying (CI)
uv run ruff format --check src/kernel_lore_mcp tests/python
```

### Linting

```bash
# Check for issues
uv run ruff check src/kernel_lore_mcp tests/python

# Auto-fix what it can
uv run ruff check --fix src/kernel_lore_mcp tests/python

# Show which rules fired
uv run ruff check --show-fixes src/kernel_lore_mcp tests/python
```

### Configuration (from our `pyproject.toml`)

```toml
[tool.ruff]
line-length = 100
target-version = "py312"

[tool.ruff.lint]
select = [
    "E", "F", "W",    # pycodestyle + pyflakes core
    "I",              # isort
    "UP",             # pyupgrade
    "B",              # bugbear
    "C4",             # comprehensions
    "PTH",            # pathlib
    "SIM",            # simplify
    "TC",             # type-checking / TYPE_CHECKING blocks
    "RUF",            # ruff-specific
    "S",              # flake8-bandit security
]
ignore = [
    "S101",   # assert allowed (tests + defensive assertions)
    "B008",   # FastAPI/FastMCP idiom: Annotated[..., Field(...)] in args
    "E501",   # line-length reported by formatter; lint only flags hard cases
]

[tool.ruff.lint.per-file-ignores]
"tests/**" = ["S", "ANN"]
```

### Rule Sets

| Code | Category | What It Catches |
|------|----------|-----------------|
| `E/W` | pycodestyle | Style violations, trailing whitespace |
| `F` | pyflakes | Unused imports, undefined names, redefined vars |
| `I` | isort | Import ordering and grouping |
| `B` | flake8-bugbear | Likely bugs and design smells |
| `UP` | pyupgrade | Code that can use newer Python syntax |
| `SIM` | flake8-simplify | Verbose patterns |
| `RUF` | ruff-specific | Ruff's own rules |
| `C4` | flake8-comprehensions | Unnecessary list/dict/set wrapping |
| `PTH` | flake8-use-pathlib | Prefer `pathlib` over `os.path` |
| `TC` | flake8-type-checking | Moves type-only imports behind `TYPE_CHECKING` |
| `S` | flake8-bandit | Common security issues (sanitized via our ignores) |

### Why Those Three Ignores (and Only Those)

- **`S101`** — `assert` is fine in our code. Pytest relies on it; our
  defensive `assert isinstance(x, Foo)` patterns before a PyO3 call
  are intentional.
- **`B008`** — FastMCP tool signatures use `Annotated[..., Field(...)]`
  with mutable defaults. That pattern is load-bearing for automatic
  `inputSchema` derivation.
- **`E501`** — The formatter already enforces 100 chars. `E501` only
  fires on edge cases the formatter can't fix (e.g. URLs in strings).
  We suppress the lint, keep the formatter.

### Per-File Overrides

`tests/**` relaxes `S` (bandit) and `ANN` (flake8-annotations):
- Test files legitimately use magic values, hardcoded secrets in
  fixtures, and `subprocess.run` — `S` noise isn't useful there.
- We don't require every fixture and helper to carry full annotations;
  pytest introspection handles signatures.

### Common Fixes

```python
# UP — pyupgrade catches legacy patterns
dict(a=1, b=2)          →  {"a": 1, "b": 2}
"{}".format(x)          →  f"{x}"
isinstance(x, (A, B))   →  isinstance(x, A | B)
Optional[str]           →  str | None

# SIM — simplify catches verbose patterns
if x == True:           →  if x:
if not x in y:          →  if x not in y:
if x: return True       →  return bool(x)
else: return False

# C4 — comprehension catches unnecessary wrapping
list([x for x in y])    →  [x for x in y]
dict([(k, v)])          →  {k: v}

# PTH — pathlib
os.path.join(a, b)      →  Path(a) / b
open("file")            →  Path("file").open()

# TC — TYPE_CHECKING
# Before: runtime import of a heavy type-only module
from kernel_lore_mcp.router.tree import ThreadNode  # used only in annotations

# After (what TC asks for):
from typing import TYPE_CHECKING
if TYPE_CHECKING:
    from kernel_lore_mcp.router.tree import ThreadNode
```

---

## ty — Type Checking

ty is Astral's type checker, built in Rust on the Salsa incremental
framework. We use it instead of mypy.

### Why ty, Not mypy

1. **Speed.** 10-60x faster cold, 80-500x incremental. Our full
   source + tests check runs in under a second.
2. **Checks all code by default.** mypy silently skips unannotated
   functions unless `--strict` is enabled. ty checks everything.
3. **Gradual guarantee.** Removing an annotation never causes a new
   type error. Adding annotations only narrows errors.
4. **Ecosystem alignment.** Same vendor as `ruff` and `uv`. Single
   `pyproject.toml` pattern.
5. **Editor integration.** `ty server` is a language server with
   near-instant feedback.

### Trade-offs to Know

- ty is beta (`0.0.x`) — occasional version-to-version breakage.
- ~15% typing specification conformance vs mypy's near-complete.
  Advanced protocol and variance patterns may not work yet.
- Different suppression syntax: `# ty: ignore[rule-name]`, not
  `# type: ignore`.

### Usage

```bash
# Check source + tests
uv run ty check src/kernel_lore_mcp tests/python

# CI mode: exit 1 on warnings
uv run ty check --error-on-warning src/kernel_lore_mcp tests/python

# Concise output (one line per diagnostic)
uv run ty check --output-format concise src/kernel_lore_mcp tests/python

# GitHub Actions annotations
uv run ty check --output-format github src/kernel_lore_mcp tests/python

# Watch mode while developing
uv run ty check --watch src/kernel_lore_mcp

# Auto-insert suppression comments for existing violations
uv run ty check --add-ignore src/kernel_lore_mcp
```

### Configuration (from our `pyproject.toml`)

```toml
[tool.ty.src]
include = ["src/kernel_lore_mcp", "tests/python"]

[tool.ty.rules]
unresolved-import = "ignore"
```

Why `unresolved-import = "ignore"`:
- We import from `kernel_lore_mcp._core` (the PyO3 extension).
  Stubs live at `src/kernel_lore_mcp/_core.pyi`, but ty needs the
  wheel built before it can fully resolve the native module. Silencing
  `unresolved-import` avoids false positives in CI between
  `uv sync` and `maturin develop`.
- Stubs still drive autocompletion and inference; we're only
  suppressing the "module not found" diagnostic, not its attribute
  inference.

### Key Diagnostics

**Errors** (definite runtime failures):

| Rule | What It Catches |
|------|-----------------|
| `invalid-assignment` | Incompatible type assigned to variable |
| `invalid-argument-type` | Wrong argument type passed to function |
| `invalid-return-type` | Return value doesn't match annotation |
| `unresolved-reference` | Undefined variable or name |
| `unresolved-attribute` | Attribute doesn't exist on type |
| `call-non-callable` | Calling a non-callable object |
| `missing-argument` | Required function argument not provided |
| `index-out-of-bounds` | Literal index exceeds tuple/sequence length |

**Warnings** (potential issues):

| Rule | What It Catches |
|------|-----------------|
| `possibly-missing-attribute` | Attribute may not exist in some branches |
| `deprecated` | Using `@deprecated` APIs |

### Suppression Comments

```python
# Suppress a specific rule on one line
value = dynamic_dispatch()  # ty: ignore[invalid-assignment]

# When a line is intentionally wrong for a test
with pytest.raises(TypeError):
    frozen_model.field = "new"  # ty: ignore[invalid-assignment]
```

### Type Narrowing

When tests or code touch an `X | None` field, narrow first:

```python
# Bad — ty reports unsupported-operator on str | None
assert "mainline" in hit.subject_tags[0]  # subject_tags: tuple[str, ...] | None

# Good — narrow first, then check
assert hit.subject_tags is not None
assert "mainline" in hit.subject_tags[0]
```

### PyO3 Stubs

`src/kernel_lore_mcp/_core.pyi` provides typed signatures for every
public `#[pyfunction]` and `#[pyclass]` in the Rust extension. The
`py.typed` marker lives next to it. See
[pyo3-maturin.md](pyo3-maturin.md) for stub conventions.

---

## Pre-Commit Script

Save as `scripts/precommit.sh`, run before every commit, or wire into
your shell as a git hook. This is the canonical form for this project:

```bash
#!/usr/bin/env bash
# scripts/precommit.sh — full pre-commit QA for kernel-lore-mcp
set -euo pipefail

PY_PKG="src/kernel_lore_mcp"
PY_TESTS="tests/python"
RUST_SRC="src"   # src/lib.rs, src/*.rs, src/bin/*

echo "=== Rust: fmt ==="
cargo fmt --all -- --check

echo "=== Rust: clippy ==="
cargo clippy --all-targets -- -D warnings

echo "=== Rust: test (pure Rust, no PyO3) ==="
cargo test --no-default-features

echo "=== Python: rebuild extension (release) ==="
uv run maturin develop --release

echo "=== Python: format ==="
uv run ruff format --check "$PY_PKG" "$PY_TESTS"

echo "=== Python: lint ==="
uv run ruff check "$PY_PKG" "$PY_TESTS"

echo "=== Python: type check ==="
uv run ty check "$PY_PKG" "$PY_TESTS"

echo "=== Python: tests ==="
uv run pytest "$PY_TESTS" -v --tb=short

echo "=== OK ==="
```

Run it:

```bash
chmod +x scripts/precommit.sh
./scripts/precommit.sh
```

In CI:

```bash
uv sync --group dev --frozen
./scripts/precommit.sh
```

Do not use `--no-verify` to skip hooks. If a hook fails, fix the issue,
re-stage, and commit — **do not** `git commit --amend` after a hook
failure (the commit did not actually happen; amend would modify the
previous commit instead).

---

## Editor Setup

### ty Language Server

```bash
# Start LSP
uv run ty server
```

Supported in: VS Code (`astral-sh.ty` extension), Neovim
(nvim-lspconfig), Zed (built-in), PyCharm (2025.3+), Emacs (Eglot).

### ruff LSP

Handled by the official `ruff` extension in VS Code, `ruff-lsp` in
Neovim, and built-in integrations elsewhere.

---

## Common Patterns

### Typing for Our Patterns

```python
# Discriminated union for tier provenance on a search hit
from typing import Literal
from pydantic import BaseModel

class MetadataProvenance(BaseModel, frozen=True):
    tier: Literal["metadata"] = "metadata"
    fields_matched: tuple[str, ...]

class TrigramProvenance(BaseModel, frozen=True):
    tier: Literal["trigram"] = "trigram"
    confirmed_by_regex: bool

class BM25Provenance(BaseModel, frozen=True):
    tier: Literal["bm25"] = "bm25"
    score: float

type TierProvenance = MetadataProvenance | TrigramProvenance | BM25Provenance
```

```python
# Type narrowing with TypeIs for router dispatch
from typing import TypeIs

def is_regex_query(term: str) -> TypeIs[str]:
    return len(term) >= 2 and term.startswith("/") and term.endswith("/")
```

```python
# @override on every FastMCP customization
from typing import override

from fastmcp import FastMCP

class KernelLoreServer(FastMCP):
    @override
    async def run_async(self, *args, **kwargs) -> None:
        ...
```

### Handling Dynamic Types

When ty cannot infer a type (e.g. raw JSON response), annotate
explicitly:

```python
# Good — explicit annotation helps ty
response: dict[str, object] = await client.get("/api/data")
items: list[str] = [str(x) for x in response.get("items", [])]

# Bad — ty sees: str | list[Unknown]
items = response.get("items", [])
```

---

## Cross-references

- [language.md](language.md) — target-version rationale (`py312` floor)
- [testing.md](testing.md) — pytest configuration and fixtures
- [uv.md](uv.md) — the toolchain that runs all of this
- [pyo3-maturin.md](pyo3-maturin.md) — `.pyi` stubs for ty
- [Rust counterpart](../rust/code-quality.md) — `cargo fmt`, `clippy`,
  test matrix
