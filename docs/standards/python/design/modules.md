# Module and Package Design

> Adapted from KAOS `docs/python/design/modules.md`. Trimmed to the
> single-package reality of `kernel-lore-mcp`.
>
> See also: [`../index.md`](../index.md), [`boundaries.md`](boundaries.md),
> [`dependencies.md`](dependencies.md).

A module is a `.py` file. A package is a directory with `__init__.py`.
`kernel-lore-mcp` ships **one** Python package (`kernel_lore_mcp`) plus
one Rust extension (`kernel_lore_mcp._core`). This doc is about how
that package is laid out and how it stays navigable as we add MCP
tools.

---

## The canonical layout

```
src/kernel_lore_mcp/
    __init__.py         # lazy _core import + native_version()
    __main__.py         # python -m kernel_lore_mcp → argparse + build_server
    _core.pyi           # stubs for the Rust extension
    server.py           # build_server() — FastMCP app assembly
    config.py           # Settings (pydantic-settings, env prefix KLMCP_)
    logging_.py         # structlog config; stdio → stderr
    models.py           # pydantic response models (outputSchema)
    tools/              # one file per MCP tool
    resources/          # blind_spots://coverage, etc.
    routes/             # /status, /metrics via @mcp.custom_route
```

That's it. No nested namespaces, no hidden sub-frameworks. The Rust
crate lives alongside at `src/*.rs`; maturin handles the mixed layout.

### Why one package

KAOS has 15 packages. We have one. The project's domain is narrow (an
MCP server over one data source) and the architectural split is
vertical: Python at the edge, Rust for the indices. Splitting into
`kernel-lore-server`, `kernel-lore-ingest`, `kernel-lore-router`
buys nothing — they'd all depend on `_core` anyway. The `reindex`
binary lives in Rust (`src/bin/reindex.rs`); it is not a separate
Python package.

---

## When to create a new file

### Signs a file is too large

A file has grown past its useful size when it contains multiple
unrelated concerns that change for different reasons. Concrete
thresholds:

- **Under 300 lines:** fine as a single file.
- **300–800 lines:** fine if the file has one coherent purpose (e.g.
  all `lore_search` tool logic).
- **Over 800 lines:** look for multiple independent subsystems. If
  yes, split. If it's one big tool handler that talks to Rust via
  `_core`, it's probably still one concern.

### Signs a file is too small

Don't create a file for a single helper used by one neighbor. If
`_parse_cursor()` is only called from `tools/search.py`, it belongs in
that file, not in `tools/_cursor_utils.py`.

Test: if the new file would have zero imports from outside its
immediate sibling, it probably does not justify its own module.

---

## The `tools/` subpackage

Every v1 MCP tool gets one file under `tools/`:

```
tools/
    __init__.py          # re-exports the registration helpers
    search.py            # lore_search
    thread.py            # lore_thread
    patch.py             # lore_patch
    activity.py          # lore_activity
    message.py           # lore_message
    series_versions.py   # lore_series_versions
    patch_diff.py        # lore_patch_diff
```

Each tool file exports an `async def lore_<name>(...)` function that:

1. Accepts pydantic-validated parameters (FastMCP handles the parse).
2. Returns a pydantic `BaseModel` subclass from `models.py`.
3. Contains no MCP-registration code.

Registration happens **explicitly** in `server.build_server()`:

```python
# server.py
from kernel_lore_mcp.tools.search import lore_search

mcp.tool(
    lore_search,
    annotations={"readOnlyHint": True, "idempotentHint": True},
)
```

This is the project rule (see CLAUDE.md): no side-effect-import
registration. Adding a tool means (a) creating `tools/<name>.py` and
(b) adding one `mcp.tool(...)` call in `server.py`. Auditing the
registered surface = read `server.py`.

### When a tool file exceeds 800 lines

Extract helpers downward, not sideways:

- Query grammar helpers → call into `_core` (Rust router).
- Cursor parsing, URL building, snippet trimming → private functions
  inside the tool file, or `tools/_shared.py` if used by ≥2 tools.

Don't split one tool across two files unless the split is along a
natural boundary (e.g. `thread.py` + `thread_rendering.py`).

---

## `resources/` and `routes/`

Same pattern as `tools/`. One file per MCP resource, one file per HTTP
route. Registration is explicit in `server.build_server()`:

```python
# resources/blind_spots.py
async def blind_spots_coverage() -> BlindSpotsCoverage: ...

# routes/status.py
async def status_handler(request: Request) -> JSONResponse: ...

# server.py
mcp.resource("blind_spots://coverage", blind_spots_coverage)
mcp.custom_route("/status", methods=["GET"])(status_handler)
```

---

## `__init__.py` as public API

The package-level `__init__.py` is deliberately tiny. The external
surface of `kernel_lore_mcp` is MCP tools, not a Python API. Internal
imports should be qualified (`from kernel_lore_mcp.models import
SearchResponse`), not routed through the package root.

Current `__init__.py`:

```python
__all__ = ["__version__", "native_version"]
__version__ = "0.1.0"

if TYPE_CHECKING:
    from kernel_lore_mcp import _core

def __getattr__(name: str) -> Any:
    if name == "_core":
        from kernel_lore_mcp import _core as mod
        return mod
    raise AttributeError(...)

def native_version() -> str:
    from kernel_lore_mcp import _core
    return _core.version()
```

Two points worth noting:

1. **`_core` is imported lazily.** Ruff, ty, pytest collection, and
   `--help` all work without a built wheel. Only code that actually
   touches `_core` forces the build.
2. **`__all__` is mandatory and sorted.** It documents the public
   surface and controls `from kernel_lore_mcp import *`.

### Subpackage `__init__.py`

`tools/__init__.py`, `resources/__init__.py`, `routes/__init__.py`
re-export the handler callables so `server.py` has short imports:

```python
# tools/__init__.py
from kernel_lore_mcp.tools.search import lore_search
# ... one line per tool

__all__ = [
    "lore_activity",
    "lore_message",
    "lore_patch",
    # ...
]
```

Keep sorted alphabetically.

### What NOT to export

- Internal helpers (anything prefixed `_`).
- Private modules (`tools/_shared.py`).
- Star imports. Ruff `F403` catches these.

---

## Test organization

Tests mirror source layout:

```
tests/python/
    conftest.py              # shared fixtures (in-process fastmcp.Client)
    fixtures/                # synthetic lore shards for ingest tests
    unit/
        test_config.py
        test_models.py
        test_logging.py
        tools/
            test_search.py
            test_thread.py
    integration/
        test_server_roundtrip.py   # fastmcp.Client end-to-end
```

### `unit/` vs `integration/`

- **`unit/`** — no network, no spawned processes. May use `_core`
  against in-memory fixtures or synthetic shards under
  `tests/python/fixtures/`.
- **`integration/`** — full MCP handshake via `fastmcp.Client`
  in-process. Still no external network; grokmirror is never hit from
  tests.

### `conftest.py` for shared fixtures

Central `tests/python/conftest.py` provides a `built_server` fixture
that runs `build_server()` against a `tmp_path`-backed `Settings` and
returns it wrapped in `fastmcp.Client`:

```python
@pytest.fixture
async def client(tmp_path: Path) -> AsyncIterator[Client]:
    settings = Settings(data_dir=tmp_path / "data")
    server = build_server(settings)
    async with Client(server) as c:
        yield c
```

---

## Anti-patterns

### God modules

One file containing models, tool handlers, Rust bindings, and CLI.
Happens when someone adds "just one more thing" to `server.py`.

**Fix:** split by concern. Models in `models.py`, tools in `tools/`,
CLI in `__main__.py`, server assembly in `server.py`.

### `helpers.py` / `utils.py`

Grab-bag modules that collect unrelated functions. Name communicates
nothing.

**Fix:** name the file after what it does. `cursor.py` for HMAC
cursor helpers, `snippets.py` for snippet trimming, etc.

### Side-effect-import tool registration

```python
# WRONG
# tools/search.py
from kernel_lore_mcp.server import mcp  # circular hazard

@mcp.tool
async def lore_search(...): ...
```

**Fix:** plain `async def` in the tool file; `server.py` imports and
calls `mcp.tool(fn, annotations=...)`. This is the project rule
(CLAUDE.md "Session-specific guidance").

### Circular imports

`tools/search.py` imports from `server.py`, `server.py` imports from
`tools/search.py`.

**Fix:** tools never import from `server.py`. They import from
`models.py`, `config.py`, and `_core`. Anything pointing back up the
tree is a design smell.

### One class per file

Creating `hit.py` with only `class SearchHit`, `response.py` with only
`class SearchResponse`, etc.

**Fix:** group related pydantic models together. `models.py` holds
the wire-facing shapes; split only if it crosses ~800 lines.

### Nesting without purpose

`tools/lore/search/core/handler.py`. Four levels for one function.

**Fix:** flatten to `tools/search.py` until there's a real reason to
nest.

---

## Summary

| Decision | Rule |
|---|---|
| New file? | Split when multiple unrelated concerns live together. |
| New subpackage? | Only `tools/`, `resources/`, `routes/`. Don't invent more. |
| `__init__.py` exports? | Sorted `__all__`; public callables only. |
| Import direction? | `server → tools → models/_core`. Never up. |
| Tool registration? | Explicit in `server.build_server()`. Never by import side effect. |
| Tests? | Mirror source; `unit/` offline, `integration/` in-process FastMCP. |
