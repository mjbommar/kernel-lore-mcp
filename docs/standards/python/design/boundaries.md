# Boundaries and Interfaces

> Adapted from KAOS `docs/python/design/boundaries.md`. Reshaped
> around the three boundaries that actually exist in this project.
>
> See also: [`../index.md`](../index.md), [`modules.md`](modules.md),
> [`errors.md`](errors.md),
> [`../libraries/pydantic.md`](../libraries/pydantic.md).

A boundary is where data crosses a trust threshold — entering from
the outside world (untrusted) or leaving our module (committed to a
contract). Validate once at the edge; operate on typed data
everywhere else.

---

## The three boundaries in kernel-lore-mcp

```
┌─────────────────────────────────────────────────────────┐
│  (A) MCP wire boundary                                  │
│      JSON-RPC ↔ pydantic models via FastMCP             │
├─────────────────────────────────────────────────────────┤
│  (B) PyO3 boundary                                      │
│      Python dict / primitives ↔ Rust structs            │
├─────────────────────────────────────────────────────────┤
│  (C) HTTP route boundary (custom_route: /status,        │
│      /metrics). No REST tool surface in v1.             │
└─────────────────────────────────────────────────────────┘

        Config boundary (pydantic-settings): loaded once at
        startup in __main__.py, injected everywhere downward.
```

Everything above these edges is untrusted. Everything inside is typed.

---

## (A) The MCP wire boundary

FastMCP parses JSON-RPC tool calls into pydantic models if the tool
function signature declares pydantic types; and it serializes the
return value automatically when the return type is a `BaseModel`.

### The rule

**Pydantic in, pydantic out.** No bare `dict[str, Any]` on tool
signatures, no bare `dict` returns.

```python
# tools/search.py
from kernel_lore_mcp.models import SearchRequest, SearchResponse

async def lore_search(request: SearchRequest) -> SearchResponse:
    # FastMCP has already validated `request` against SearchRequest
    # FastMCP will auto-derive outputSchema from SearchResponse and
    # emit structuredContent + a text summary.
    ...
```

A bare `dict` return silently collapses to a `TextContent` block
with stringified JSON (no `outputSchema`, no `structuredContent`).
Callers lose tool-output validation on the LLM side. This is the
failure mode CLAUDE.md calls out.

### Why the wire boundary matters

- **Schema advertisement.** Pydantic → JSON Schema → MCP
  `inputSchema` / `outputSchema` → what the LLM sees in the tool
  catalog.
- **Validation.** Bad inputs raise `pydantic.ValidationError`, which
  FastMCP converts to `isError: true` with `INVALID_PARAMS` + the
  field path + message. The LLM can self-correct.
- **Stable contract.** `models.py` is the specification of the tool
  surface. Reading it tells you the whole wire format.

See [`../libraries/pydantic.md`](../libraries/pydantic.md) for
ConfigDict profiles, `Field(...)` constraints, and discriminated
unions.

### Where validation errors go

See [`errors.md`](errors.md). Pydantic `ValidationError` caught at
the tool boundary → MCP `isError: true` with `INVALID_PARAMS`.
Pre-validated inputs never need re-checking inside tool bodies.

### Keyword-only optional parameters

Tool functions should put required params first and keyword-only
params after `*`:

```python
async def lore_search(
    q: str,
    *,
    list_: str | None = None,
    cursor: str | None = None,
    max_results: int = 20,
) -> SearchResponse: ...
```

(Usually you'll pass a single `SearchRequest` pydantic model instead.
Both work with FastMCP.)

---

## (B) The PyO3 boundary

`kernel_lore_mcp._core` is the Rust extension. It exposes a small set
of Python-callable functions that take primitives / dicts / bytes and
return primitives / dicts / bytes. Crossing this boundary is
expensive enough to care about.

### The shape rule

Keep the boundary **wide and infrequent**. Call into Rust once per
tool invocation with all the data it needs; don't ping-pong.

```python
# Good — one call, one dict back
hits = await asyncio.to_thread(
    _core.run_search,
    query_ast_json,        # str — already serialized from pydantic
    cursor_bytes,          # bytes | None — opaque to Python
    max_results,           # int
)
# hits is list[dict[str, Any]] — wrap into SearchHit at the edge

# Bad — crossing per-row
for hit_id in candidate_ids:
    row = _core.fetch_row(hit_id)   # N boundary crossings
```

### What crosses

| Direction | Shape | Notes |
|---|---|---|
| Python → Rust | `str`, `int`, `bool`, `bytes`, `dict` | Query AST as JSON string, cursor as bytes. Avoid large Python objects. |
| Rust → Python | `list[dict]`, `bytes`, primitives | Rust returns plain dicts; Python wraps into pydantic at the edge. |
| Errors | `PyErr` via `impl From<Error> for PyErr` | `Error::QueryParse`, `Error::RegexComplexity`, `Error::InvalidCursor` → `PyValueError`. Everything else → `PyRuntimeError`. See `src/error.rs`. |

The `impl From<Error> for PyErr` in `src/error.rs` is the **only**
translation point between Rust's error type and Python exceptions.
Don't bypass it with `PyErr::new_err` at random call sites.

### Wrapping the return at the edge

Rust returns plain `dict` rows (easier and cheaper than building
`PyObject` that matches pydantic). The Python tool function wraps:

```python
raw = await asyncio.to_thread(_core.run_search, ...)
return SearchResponse(
    results=[SearchHit.model_validate(row) for row in raw["hits"]],
    query_tiers_hit=raw["tiers"],
    freshness=Freshness.model_validate(raw["freshness"]),
    ...
)
```

`model_validate` is the right tool here: the data comes from our own
Rust code so it's mostly trusted, but going through validation
catches drift between `_core.pyi` and the Rust return shape (which
is the most common way the PyO3 boundary breaks silently).

### Stubs live in `_core.pyi`

Every function `_core` exports must appear in `src/kernel_lore_mcp/_core.pyi`
with a precise signature. This is how `ty` and IDEs see the boundary.
Keeping stubs in lockstep with `src/lib.rs` is a build-time invariant.

---

## (C) The FastAPI / custom_route boundary

FastMCP mounts a Starlette app for Streamable HTTP transport and lets
us add custom routes via `@mcp.custom_route`. The v1 surface is:

- `GET /status` — freshness, last ingest, stale lists. 30s-cached.
- `GET /metrics` — prometheus text exposition. Localhost bind only.

**No REST tool endpoints in v1.** CLAUDE.md is explicit: don't mount
a FastAPI surface. If we ever do, it goes here, and every handler
follows the same three-part contract:

1. Validate request (Starlette `Request` / pydantic).
2. Call into the same tool functions that FastMCP calls — no
   parallel codepaths.
3. Return `JSONResponse` with the pydantic model `.model_dump()`.

### Auth / rate limit at this boundary

Rate-limit-per-IP lives at the custom_route / transport level, not
inside tool handlers. Tool handlers should not know the transport.

---

## The config boundary

Configuration is inbound. Load it **once**, at the edge, and inject
typed objects inward.

```python
# __main__.py — the edge
settings = Settings()                         # reads env + .env
configure_logging(transport=args.transport, level=args.log_level)
server = build_server(settings)
await server.run_async(transport=args.transport)
```

### `Settings` rules

- Subclass `BaseSettings` from `pydantic-settings`.
- `SettingsConfigDict(env_prefix="KLMCP_", env_file=".env", extra="ignore")`.
- Secrets (`KLMCP_CURSOR_KEY`) use `SecretStr`.
- `extra="ignore"` so new env vars from updated code don't crash
  older deployments.
- Never read `os.environ` anywhere but `config.py`.

See [`../libraries/pydantic.md`](../libraries/pydantic.md) §8 for
the full settings pattern.

### Where settings are consumed

- `build_server(settings)` passes them to `FastMCP(...)` and closes
  over them for tool handlers that need `cursor_signing_key` or
  `query_wall_clock_ms`.
- Tool handlers receive `Settings` via closure, not by reading env.
- `_core` receives only the path/int/bool primitives it needs, never
  the `Settings` object itself.

### Per-request overrides

Not supported in v1. If it lands later (MCP `_meta` config), the
override layer sits at the tool boundary and builds a `Settings`
variant before calling into the handler.

---

## Function signature design

Signatures are contracts. A good signature tells the caller exactly
what goes in and what comes out without reading the body.

### Accept specific types

```python
# Good
async def lore_search(request: SearchRequest) -> SearchResponse: ...

# Bad — caller has no idea what keys are expected
async def lore_search(params: dict[str, Any]) -> dict[str, Any]: ...
```

### Return specific types

Pydantic model or a concrete dataclass. Never `dict[str, Any]` at a
module boundary.

### Keyword-only for optional parameters

```python
async def fetch_thread(
    message_id: str,
    *,
    include_patches: bool = True,
    max_bytes: int = 5_242_880,
) -> ThreadResponse: ...
```

### The `settings` parameter pattern

Internal helpers that need configuration accept settings as the last
keyword-only argument:

```python
async def _parse_cursor(
    raw: str,
    *,
    settings: Settings,
) -> CursorState: ...
```

This keeps the helper testable (you pass a synthetic `Settings` in
unit tests) and keeps `os.environ` out of helper code entirely.

---

## Error boundaries

Full rules in [`errors.md`](errors.md). Summary:

| Layer | Error handling |
|---|---|
| `_core` (Rust) | Raise `Error` variants. `From<Error> for PyErr` in `src/error.rs` maps them to `PyValueError` / `PyRuntimeError`. |
| Python helpers | Let exceptions propagate. Don't wrap in tool-specific errors mid-stack. |
| Tool handler (MCP boundary) | Let pydantic `ValidationError` bubble up (FastMCP handles it). For expected failure modes, raise `fastmcp.ToolError`. For unexpected failures, raise and let FastMCP convert to `isError: true`. |

---

## Testing at boundaries

Test the contract, not the internals.

### MCP wire boundary

Use `fastmcp.Client` in-process. You call `lore_search` exactly the
way an LLM would; you assert on `SearchResponse` fields.

```python
async def test_lore_search_returns_hits(client: Client) -> None:
    result = await client.call_tool("lore_search", {"q": "KVM: x86"})
    assert not result.isError
    resp = SearchResponse.model_validate(result.structured_content)
    assert resp.results
    assert resp.freshness.last_ingest_utc is not None
```

### PyO3 boundary

Test via the Python helper that wraps `_core`. Unit tests build a
tiny synthetic index under `tmp_path`, call `_core.run_search`
directly, and assert on the returned dict shape. These tests catch
stubs-vs-Rust drift.

### Custom route boundary

`fastmcp.Client` exposes the Starlette app. Use Starlette's
`TestClient` for `/status` and `/metrics`.

---

## Anti-patterns

### Bare dicts across the MCP boundary

```python
# WRONG
async def lore_search(q: str) -> dict[str, Any]:
    return {"results": [...]}
```

No `outputSchema`, no validation, no structured content. Use pydantic.

### `os.environ` deep in a handler

```python
# WRONG
async def lore_search(request: SearchRequest) -> SearchResponse:
    key = os.environ["KLMCP_CURSOR_KEY"]
```

**Fix:** `Settings` is loaded in `__main__.py` and passed via the
`build_server(settings)` closure.

### Ping-ponging across the PyO3 boundary

Calling `_core` once per row, or 100 tiny `_core.*` calls in one
handler. Each crossing is pythonobject→Rust conversion plus GIL
arithmetic. Batch.

### Translating Rust errors in multiple places

Only `src/error.rs` converts `Error` → `PyErr`. Don't build ad-hoc
`PyValueError::new_err(...)` in other `.rs` files.

### A FastAPI surface "because we might need it"

CLAUDE.md: deferred past v1. Don't mount it speculatively. If demand
lands, it goes behind `@mcp.custom_route` and reuses the tool
functions.

---

## Summary

| Principle | Rule |
|---|---|
| Validate at the edge | Pydantic at the MCP wire boundary. `impl From<Error> for PyErr` at the PyO3 boundary. |
| Typed signatures | Accept specific types, return specific types. Never pass `dict[str, Any]` through multiple hops. |
| Settings at the edge | Load `Settings` in `__main__.py`. Pass typed objects inward. Never read `os.environ` elsewhere. |
| Wide, infrequent PyO3 calls | One call per tool invocation with all the data it needs. Batch. |
| No speculative REST | `@mcp.custom_route` for `/status` + `/metrics` only in v1. |
