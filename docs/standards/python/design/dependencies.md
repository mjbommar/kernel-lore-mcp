# Dependency and Import Discipline

> Adapted from KAOS `docs/python/design/dependencies.md`. Trimmed
> because we have one package instead of 15; the import-DAG rules
> apply within the tree.
>
> See also: [`../index.md`](../index.md), [`modules.md`](modules.md),
> [`boundaries.md`](boundaries.md).

Rules for managing dependencies, import ordering, lazy loading, and
optional feature gating inside `kernel_lore_mcp`.

---

## The in-tree import DAG

Imports must flow downward:

```
__main__.py
    │
    ▼
server.py  ──────────▶  tools/*.py  ──▶  models.py   ──▶  _core (Rust)
    │                       │            config.py
    ├─▶ resources/*.py ─────┤            logging_.py
    └─▶ routes/*.py    ─────┘
```

Rules:

- `server.py` imports from `tools/`, `resources/`, `routes/`,
  `config.py`, plus the `FastMCP` framework.
- `tools/*.py` import from `models.py`, `config.py`, and `_core`.
- `tools/*.py` must **never** import from `server.py`. That's the
  single most common way to create a circular import in a FastMCP
  app. See [`modules.md`](modules.md) — tool registration is
  explicit in `server.build_server()`, not by side-effect.
- `models.py`, `config.py`, `logging_.py` import only stdlib +
  pydantic + structlog.

If you ever want an upward import (a helper in `tools/` importing from
`server.py`), that's a design smell. Extract the shared code
downward into `models.py` or a dedicated helper module.

### The sibling rule

Tool files must not import each other.

```python
# WRONG
# tools/patch.py
from kernel_lore_mcp.tools.search import _some_helper
```

**Why:** it creates hidden coupling and makes it impossible to add a
tool without auditing siblings. If two tools need the same helper,
it lives in `models.py`, `config.py`, or a purpose-named helper like
`cursor.py` that both import.

Same rule for `resources/*.py` and `routes/*.py`.

---

## Lazy imports

Import heavy libraries inside function bodies, not at module top
level. This keeps `--help` fast and avoids forcing a built wheel for
tooling that doesn't need one.

### `_core` is lazy by convention

The Rust extension is loaded the first time it's touched, not at
import time:

```python
# kernel_lore_mcp/__init__.py
def __getattr__(name: str) -> Any:
    if name == "_core":
        from kernel_lore_mcp import _core as mod
        return mod
    raise AttributeError(...)
```

This means `import kernel_lore_mcp` works without a built wheel —
ruff, ty, pytest collection, and `python -m kernel_lore_mcp --help`
all succeed without maturin having run.

Inside tool handlers, import `_core` at the top of the function:

```python
# tools/search.py
async def lore_search(request: SearchRequest) -> SearchResponse:
    from kernel_lore_mcp import _core  # lazy

    raw = await asyncio.to_thread(_core.run_search, ...)
    ...
```

Or at module top if the tool *always* uses `_core` — that's fine too,
as long as you accept that importing the tool module forces the wheel.

### CLI stays lazy

`__main__.py` parses args first, builds the server second:

```python
def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(prog="kernel-lore-mcp")
    parser.add_argument("--transport", choices=["stdio", "http"], default="stdio")
    # ...
    args = parser.parse_args(argv)

    # Heavy imports after argparse
    from kernel_lore_mcp.config import Settings
    from kernel_lore_mcp.logging_ import configure
    from kernel_lore_mcp.server import build_server

    settings = Settings()
    configure(transport=args.transport, level=args.log_level)
    server = build_server(settings)
    asyncio.run(server.run_async(transport=args.transport))
```

`kernel-lore-mcp --help` should return in <100ms with no wheel built.

### What counts as "heavy"

| Library | Reason |
|---|---|
| `kernel_lore_mcp._core` | Rust extension; requires built wheel |
| `fastmcp` | Pulls in mcp SDK + pydantic graph; ~0.5s import |
| `prometheus_client` | Only needed by `/metrics` route |

`pydantic`, `pydantic-settings`, `structlog`, stdlib — fine at top
level.

---

## `TYPE_CHECKING` for type-only imports

Use `from __future__ import annotations` + `if TYPE_CHECKING:` for
imports needed only for annotations.

```python
from __future__ import annotations

from typing import TYPE_CHECKING

from pydantic import BaseModel

if TYPE_CHECKING:
    from kernel_lore_mcp import _core          # type stubs only
    from kernel_lore_mcp.config import Settings
```

Rules:

1. Always pair with `from __future__ import annotations`. Without
   it, Python evaluates annotations at class definition time and the
   `TYPE_CHECKING` guard is useless.
2. Put `if TYPE_CHECKING:` after all real imports, before any code.
   Ruff `I` enforces this.
3. Never use a `TYPE_CHECKING`-only import at runtime (function
   bodies, default values, `isinstance()` checks). If you need the
   type at runtime, import it for real.

### Common uses in this tree

- **Annotating `_core` types in Python helpers** so ty can see them
  without forcing a built wheel at Python-side import time.
- **Settings type hints** in helper modules that accept `Settings`
  parameters but don't construct them.

---

## Optional-dependency gating

Not heavily used in v1; the project has fixed runtime deps. But the
pattern is worth documenting in case we add optional features later
(e.g. an experimental embeddings tier).

### The pattern

```python
try:
    import some_optional_dep
    _HAS_OPTIONAL = True
except ImportError:
    _HAS_OPTIONAL = False

def feature() -> Result:
    if not _HAS_OPTIONAL:
        raise ImportError(
            "This feature requires `some_optional_dep`. "
            "Install with: uv sync --extra experimental"
        )
    ...
```

Rules:

1. Flag variable is a module-level constant, prefixed `_HAS_`.
2. The `except` clause catches only `ImportError`, not `Exception`.
3. When the missing dep makes the function useless, raise
   `ImportError` with an install hint **at call time**, not at
   import time.

### Declaring optional deps

```toml
# pyproject.toml
[project.optional-dependencies]
# reserved for future optional features; none in v1

[dependency-groups]
dev = ["pytest>=8", "pytest-asyncio", "ruff", "ty"]
```

Never put dev tools in `[project.optional-dependencies]` — that
publishes them as installable extras. Pytest, ruff, ty belong in
`[dependency-groups]`.

---

## Adding new dependencies

Every new dependency is a long-term commitment. Before adding one:

| Question | Action if No |
|---|---|
| Is it well-maintained? (commits in last 6 months) | Don't add. |
| Is the license permissive? (MIT, Apache-2.0, BSD) | Don't add. |
| Could we implement this in ~500 lines? | Implement in-house. |
| Does it duplicate something already in the stack? | Use what we have. |
| Is it needed at runtime, or dev-only? | If dev-only, `[dependency-groups] dev`. |

Stack pins (see [`../../../CLAUDE.md`](../../../CLAUDE.md)):

- Python 3.12+ floor (abi3-py312), 3.14 preferred
- `fastmcp` 3.2.4, `mcp` 1.27, `pydantic` v2, `pydantic-settings`
- `structlog`, stdlib otherwise
- No `httpx` unless/until we call out to Patchwork or similar; see
  [`../libraries/httpx.md`](../libraries/httpx.md).

Any bump to pinned versions is a project decision, not a casual
`uv lock --upgrade`. Log the reason in the commit message.

---

## Import organization

Imports are grouped and separated by blank lines:

1. **Standard library**
2. **Third-party packages** (pydantic, structlog, fastmcp)
3. **Local package** (kernel_lore_mcp)

Enforced by ruff rule `I` (isort). `pyproject.toml` declares
first-party:

```toml
[tool.ruff.lint.isort]
known-first-party = ["kernel_lore_mcp"]
```

### Example

```python
from __future__ import annotations     # always first

import asyncio
from pathlib import Path
from typing import TYPE_CHECKING

import structlog
from fastmcp import FastMCP
from pydantic import BaseModel

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.models import SearchResponse

if TYPE_CHECKING:
    from kernel_lore_mcp import _core
```

Rules:

- **Absolute imports only.** No `from . import`.
- **One `from X import Y` per line** when importing multiple names.
  Ruff enforces.
- **No import aliases** unless avoiding a name collision.
- **`from __future__ import annotations`** before everything else.

---

## Anti-patterns

### Circular imports

```python
# tools/search.py
from kernel_lore_mcp.server import mcp  # creates a cycle

# server.py
from kernel_lore_mcp.tools.search import lore_search
```

**Fix:** tools are plain `async def`. Registration happens in
`server.build_server()`. Tools never import from `server.py`.

### Import-time side effects

```python
# WRONG — opens a file at import time
with open("/etc/kernel-lore/secret") as f:
    _CURSOR_KEY = f.read()
```

Module-level code only defines classes, functions, and constants.
Everything else goes in `Settings` or inside function bodies.

### Star imports

```python
from kernel_lore_mcp.models import *   # F403, prohibited
```

Breaks static analysis, hides origins, creates silent collisions.

### Reaching into `_core` internals

`_core` exposes a typed surface via `_core.pyi`. Don't import from
`_core.some_submodule` that isn't in the `.pyi`. If you need a
function that isn't in the stub, add it to `src/lib.rs` and the stub
together.

### `os.environ` in library code

```python
# WRONG — inside a tool handler
api_url = os.environ["LORE_MIRROR_BASE"]
```

**Fix:** extend `Settings`, thread through via the
`build_server(settings)` closure.

---

## Summary

| Rule | Why |
|---|---|
| Imports flow downward in the tree | Keeps the module graph acyclic. |
| Sibling tool files don't import each other | No hidden coupling; shared helpers go lower. |
| `_core` is lazy-imported | Tooling works without a built wheel. |
| `TYPE_CHECKING` for type-only imports | ty sees types without runtime cost. |
| No `os.environ` outside `config.py` | Single edge; testable settings. |
