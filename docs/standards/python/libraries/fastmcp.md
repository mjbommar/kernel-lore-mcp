# FastMCP

> No KAOS source; project-specific.
>
> See also: [`../index.md`](../index.md),
> [`../design/boundaries.md`](../design/boundaries.md),
> [`../design/errors.md`](../design/errors.md),
> [`pydantic.md`](pydantic.md),
> [`structlog.md`](structlog.md).

FastMCP (pinned at **3.2.4**, see CLAUDE.md) is the MCP framework
we use. The standalone `fastmcp` package, **not** the vendored
`mcp.server.fastmcp` which has diverged. `mcp` (1.27) is still a
dependency for type imports only.

This doc covers the subset we actually use: server assembly, tool
registration, resources, custom routes, transport, and in-process
testing.

---

## 1. Installing

In `pyproject.toml`:

```toml
[project]
dependencies = [
    "fastmcp==3.2.4",
    "mcp==1.27",       # type imports only
    "pydantic>=2.7",
    "pydantic-settings>=2.2",
    "structlog>=24.1",
]
```

Pinned exact. Any bump is a project decision (CLAUDE.md).

---

## 2. Server assembly

Server construction happens in `server.build_server()`:

```python
# src/kernel_lore_mcp/server.py
from fastmcp import FastMCP

from kernel_lore_mcp.config import Settings

INSTRUCTIONS = """\
Search and retrieve messages from the Linux kernel mailing list
archives (lore.kernel.org). ... Freshness: lore runs 1-5 minutes
behind vger. ...
"""


def build_server(settings: Settings | None = None) -> FastMCP:
    settings = settings or Settings()
    mcp: FastMCP = FastMCP(name="kernel-lore", instructions=INSTRUCTIONS)

    # Explicit tool registration — the project rule. See below.
    from kernel_lore_mcp.tools.search import lore_search
    mcp.tool(
        lore_search,
        annotations={"readOnlyHint": True, "idempotentHint": True},
    )

    # ... more tools, resources, routes ...
    return mcp
```

### `name` and `instructions`

- `name="kernel-lore"` is the server identity the client sees.
- `instructions=...` is a multi-line system-level hint to the LLM.
  This is where we tell the model about lore coverage, freshness
  lag, and the `blind_spots://coverage` resource. Keep it concise
  and action-oriented.

---

## 3. Tool registration — **explicit only**

### The rule

Tool registration in this project is **always explicit**:

```python
# tools/search.py — just a function, no decorator
async def lore_search(request: SearchRequest) -> SearchResponse:
    ...

# server.py — register explicitly
from kernel_lore_mcp.tools.search import lore_search

mcp.tool(
    lore_search,
    annotations={"readOnlyHint": True, "idempotentHint": True},
)
```

**Not this** (CLAUDE.md's explicitly-banned pattern):

```python
# DO NOT — side-effect-import registration
from kernel_lore_mcp.server import mcp

@mcp.tool
async def lore_search(...): ...
```

### Why explicit

- **Auditable surface.** Reading `server.py` shows every tool in
  scope. No hunting through a `tools/` tree to find decorators.
- **No circular imports.** Tools don't need to reach back up to the
  server module.
- **Test isolation.** A test fixture can build a `FastMCP` with a
  subset of tools by skipping registrations. Side-effect imports
  make that awkward.

### Annotations

Every v1 tool is read-only, so every registration carries:

```python
annotations={
    "readOnlyHint": True,
    "idempotentHint": True,
}
```

`readOnlyHint` tells the client the tool doesn't mutate state.
`idempotentHint` signals that repeating the call is safe. Both are
part of the MCP 2025-11 spec. See CLAUDE.md: "All read-only. All
annotate `readOnlyHint: true`."

### Input & output schemas

FastMCP derives both from the handler's type hints:

```python
async def lore_search(request: SearchRequest) -> SearchResponse:
    ...
```

- `SearchRequest` (pydantic) → JSON Schema → `inputSchema` in the MCP
  tool catalog.
- `SearchResponse` (pydantic) → JSON Schema → `outputSchema` →
  FastMCP emits `structuredContent` on success.

**Always return a pydantic `BaseModel`.** Bare dicts collapse to
`TextContent` with stringified JSON. See [`pydantic.md`](pydantic.md) §1.

### Handler signatures

Two forms both work; we prefer the single-model form:

```python
# Preferred — one pydantic model as the request
async def lore_search(request: SearchRequest) -> SearchResponse: ...

# Also works — individual parameters (FastMCP builds a schema from them)
async def lore_patch(
    message_id: str,
    *,
    include_context: bool = True,
) -> PatchResponse: ...
```

Multi-parameter form is fine for simple tools with 1–2 arguments. For
anything with >2 fields, a request model is clearer and gives you
`@model_validator` / `@field_validator`.

---

## 4. Resources

Resources expose read-only data at stable URIs. Our one in v1 is
`blind_spots://coverage`.

```python
# resources/blind_spots.py
from kernel_lore_mcp.models import BlindSpotsCoverage

async def blind_spots_coverage() -> BlindSpotsCoverage:
    """Return the static list of sources that lore does NOT cover."""
    return BlindSpotsCoverage(
        categories=[...],
        last_reviewed_utc=...,
    )

# server.py
from kernel_lore_mcp.resources.blind_spots import blind_spots_coverage

mcp.resource("blind_spots://coverage")(blind_spots_coverage)
```

Per CLAUDE.md: blind_spots is an MCP **resource**, not a per-response
payload. The LLM fetches it once per session; it never goes in a
per-search response.

---

## 5. Custom HTTP routes

`@mcp.custom_route` adds Starlette routes to the Streamable HTTP
transport app. v1 uses two:

```python
# routes/status.py
from starlette.requests import Request
from starlette.responses import JSONResponse

async def status(request: Request) -> JSONResponse:
    # ... build freshness payload ...
    return JSONResponse(payload.model_dump())


# routes/metrics.py — prometheus_client
from prometheus_client import generate_latest, CONTENT_TYPE_LATEST
from starlette.responses import Response

async def metrics(request: Request) -> Response:
    return Response(generate_latest(), media_type=CONTENT_TYPE_LATEST)


# server.py
from kernel_lore_mcp.routes.status import status
from kernel_lore_mcp.routes.metrics import metrics

mcp.custom_route("/status", methods=["GET"])(status)
mcp.custom_route("/metrics", methods=["GET"])(metrics)
```

Notes:

- `/status` is 30s-cached inside the handler (see
  [`../design/concurrency.md`](../design/concurrency.md) for the
  cache pattern).
- `/metrics` should be exposed on localhost only — enforce at nginx
  or bind level, not inside the handler.
- Custom routes are only active under HTTP transport. Under stdio
  they don't do anything.

---

## 6. Transports

FastMCP supports stdio and Streamable HTTP. Per CLAUDE.md: **no SSE**
(deprecated April 1, 2026).

### Running

```python
# __main__.py
def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(prog="kernel-lore-mcp")
    parser.add_argument("--transport", choices=["stdio", "http"], default="stdio")
    parser.add_argument("--log-level", default="INFO")
    args = parser.parse_args(argv)

    from kernel_lore_mcp.config import Settings
    from kernel_lore_mcp.logging_ import configure
    from kernel_lore_mcp.server import build_server

    settings = Settings()
    configure(transport=args.transport, level=args.log_level)
    server = build_server(settings)

    if args.transport == "stdio":
        asyncio.run(server.run_async(transport="stdio"))
    else:
        asyncio.run(server.run_async(
            transport="http",
            host=settings.bind,
            port=settings.port,
        ))
```

### stdio transport

- MCP JSON-RPC framing on stdin/stdout.
- **Nothing else may touch stdout.** Every `print`, every stdlib
  logger, every structlog call must go to **stderr** or the protocol
  is corrupted.
- Default transport for `claude-code`, `codex`, `cursor` when the
  server is spawned as a subprocess.
- Default bind does not apply.

### HTTP transport (Streamable HTTP)

- The current MCP transport spec. SSE deprecated.
- Default bind `127.0.0.1` (Settings default). `KLMCP_BIND=0.0.0.0`
  for public deploy.
- Can run behind multi-worker uvicorn. Each worker reloads the
  tantivy reader independently via the state generation counter (see
  [`../design/concurrency.md`](../design/concurrency.md)).

### The stdio stdout rule

**If you write a single non-JSON byte to stdout under stdio, the
client disconnects.** Sources of accidental stdout writes:

- `print(...)` anywhere in tool handlers or library code.
- Any stdlib `logging` call whose handler defaults to `sys.stdout`.
- A misconfigured structlog logger. See
  [`structlog.md`](structlog.md) for the configure-for-stderr
  pattern.
- Third-party library warnings (`warnings.warn`) that go through
  stderr by default but can be redirected — leave them alone.

Our `logging_.configure()` sets the stdlib root to stream stderr and
structlog's `PrintLoggerFactory(file=sys.stderr)`. That's the single
configuration point. Don't bypass it.

---

## 7. In-process testing with `fastmcp.Client`

FastMCP ships an in-process client that speaks the full MCP protocol
directly against a `FastMCP` instance — no subprocess, no sockets.

```python
# tests/python/conftest.py
import pytest
from fastmcp import Client

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.server import build_server


@pytest.fixture
async def mcp_client(tmp_path):
    settings = Settings(data_dir=tmp_path / "data")
    server = build_server(settings)
    async with Client(server) as client:
        yield client
```

### Using it in tests

```python
# tests/python/integration/test_search.py
async def test_search_returns_hits(mcp_client: Client) -> None:
    result = await mcp_client.call_tool(
        "lore_search",
        {"q": "KVM: x86 nested", "max_results": 5},
    )
    assert not result.isError
    # Structured content is auto-derived from SearchResponse
    data = result.structured_content
    assert "results" in data
    assert len(data["results"]) <= 5
```

### What this gets you

- Full MCP protocol coverage (tool listing, call, error shapes).
- No network, no subprocess. Fast.
- Exact same codepath as production; only the transport differs.

### Integration vs. unit

- **`tests/python/unit/`** — test tool handlers directly as `async def`
  functions. No MCP layer.
- **`tests/python/integration/`** — test via `fastmcp.Client`. Catches
  schema drift, annotation bugs, and wire-format regressions.

Both live in the same tree. See
[`../design/modules.md`](../design/modules.md) for test layout.

---

## 8. Error shapes on the wire

Recap — full treatment in [`../design/errors.md`](../design/errors.md):

| Source | Wire shape |
|---|---|
| `pydantic.ValidationError` from the request model | `isError: true`, code `-32602` (INVALID_PARAMS), field path + message |
| `fastmcp.ToolError` raised in the handler | `isError: true`, human-readable message, no code |
| Uncaught `Exception` in the handler | `isError: true`, code `-32603` (INTERNAL_ERROR), server-side traceback logged by structlog |

Handler authors raise `ToolError` for user-fixable problems (bad
query, missing patch) with **three-part** messages. They let
unexpected exceptions propagate.

---

## 9. Anti-patterns

### Side-effect-import registration

Already covered. The ban is in CLAUDE.md and here. The lint rule is
"registrations live in `server.build_server()`."

### Returning bare dicts

```python
# BAD
async def lore_search(request: SearchRequest) -> dict[str, Any]:
    return {"results": [...]}
```

No `outputSchema`, no `structuredContent`. Use pydantic.

### stdout in stdio mode

Any of these will corrupt the stdio transport:

- `print("debug")` inside a handler.
- Stdlib logger that writes to `sys.stdout`.
- `traceback.print_exc()` (writes to stderr — OK, but check your
  environment).

### SSE transport

Deprecated, do not add. HTTP transport is Streamable HTTP only.

### `mcp.server.fastmcp`

The vendored `mcp.server.fastmcp` has diverged from the standalone
`fastmcp`. Use `from fastmcp import FastMCP`, not
`from mcp.server.fastmcp import FastMCP`. CLAUDE.md is explicit.

### Tool handlers doing I/O synchronously

```python
# BAD — blocks the event loop
async def lore_search(request: SearchRequest) -> SearchResponse:
    raw = _core.run_search(...)   # sync, 200ms
    ...

# GOOD
async def lore_search(request: SearchRequest) -> SearchResponse:
    raw = await asyncio.to_thread(_core.run_search, ...)
    ...
```

See [`../design/concurrency.md`](../design/concurrency.md).

---

## 10. Quick reference

| Task | Pattern |
|---|---|
| Build server | `FastMCP(name=..., instructions=...)` in `server.build_server()` |
| Register a tool | `mcp.tool(fn, annotations={"readOnlyHint": True, ...})` — never a decorator |
| Register a resource | `mcp.resource("scheme://path")(fn)` |
| Register an HTTP route | `mcp.custom_route("/path", methods=["GET"])(fn)` |
| Input validation | Pydantic on the handler signature; FastMCP reports INVALID_PARAMS |
| Expected-error path | Raise `fastmcp.ToolError` with a three-part message |
| stdio transport | `server.run_async(transport="stdio")`; stdout reserved for MCP framing |
| HTTP transport | `server.run_async(transport="http", host=settings.bind, port=settings.port)` |
| Testing | `async with Client(server) as mcp_client:` |
