# Error Design and Handling

> Adapted from KAOS `docs/python/design/errors.md`. The hierarchy
> here is simpler: FastMCP owns the MCP-wire error shape, and
> Rust-side errors come through the `impl From<Error> for PyErr` in
> `src/error.rs`.
>
> See also: [`../index.md`](../index.md), [`boundaries.md`](boundaries.md),
> [`../libraries/fastmcp.md`](../libraries/fastmcp.md).

Errors are the most important text the server produces. An LLM
reading an error message needs to know: what went wrong, how to fix
it, and what alternative exists. Same need applies at 2am when a
human is debugging.

---

## The three-part error rule

Every MCP error message must include:

1. **What went wrong** — specific failure
2. **How to fix it** — actionable recovery
3. **Alternative approach** — a different tool or query shape, when
   applicable

```python
# Good — all three parts
raise ToolError(
    f"query exceeded {settings.query_wall_clock_ms}ms wall-clock cap. "
    "Narrow the query by adding list:, rt:, or f: operators. "
    "For broad discovery use lore_activity, which scans metadata only."
)

# Bad — no actionable recovery
raise ToolError("query timed out")
```

### When "alternative" genuinely doesn't apply

The third part can collapse into additional diagnostic guidance:

```python
raise ToolError(
    f"cursor signature mismatch. "
    "The cursor was signed with a different KLMCP_CURSOR_KEY; "
    "start a new query without the cursor."
)
```

Never return an error that leaves the agent with zero next steps.

---

## The error hierarchy in this project

Three sources of errors, each with a clear translation point:

```
┌──────────────────────────────────────────────────────────────┐
│  (1) pydantic ValidationError                                │
│       FastMCP catches → isError=true + INVALID_PARAMS        │
├──────────────────────────────────────────────────────────────┤
│  (2) Rust Error (src/error.rs)                                │
│       impl From<Error> for PyErr                              │
│       QueryParse / RegexComplexity / InvalidCursor → PyValueError │
│       everything else → PyRuntimeError                        │
│       ↓                                                       │
│       Python tool handler catches PyValueError → ToolError    │
│       PyRuntimeError propagates → FastMCP INTERNAL_ERROR      │
├──────────────────────────────────────────────────────────────┤
│  (3) fastmcp.ToolError                                        │
│       Expected tool-level failures (cap hit, bad operator     │
│       combo, freshness unavailable). Tool authors raise       │
│       these directly with three-part messages.                │
└──────────────────────────────────────────────────────────────┘
```

We **do not** define a `KernelLoreError` base class or a ladder of
Python exception subclasses. The Rust `Error` enum is the canonical
one; Python code either lets it propagate (for internal failures) or
translates it to `ToolError` with a better message.

---

## (1) Pydantic validation at the MCP boundary

FastMCP validates tool inputs against the pydantic model declared on
the handler signature. Bad input → `pydantic.ValidationError` →
`isError: true` with `code: -32602` (INVALID_PARAMS) and the field
path + message.

```python
# models.py
class SearchRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    q: str = Field(min_length=1, max_length=10_000)
    max_results: int = Field(default=20, ge=1, le=200)
    cursor: str | None = None

# tools/search.py
async def lore_search(request: SearchRequest) -> SearchResponse:
    # If we got here, request is already valid.
    ...
```

A missing `q`, a `max_results` over 200, or an unknown field all fail
at the boundary with a structured error the LLM can self-correct.
No hand-rolled validation inside the handler.

### Don't re-validate

```python
# WRONG — pydantic already did this
async def lore_search(request: SearchRequest) -> SearchResponse:
    if not request.q:
        raise ValueError("q is required")   # unreachable
    if request.max_results > 200:
        raise ValueError("max_results too large")   # unreachable
```

If pydantic's validation isn't strong enough, tighten the `Field(...)`
constraints or add a `@field_validator`. Don't duplicate in handlers.

### Field constraints double as LLM documentation

Every `Field(..., description=...)` flows into JSON Schema, which
flows into the tool catalog the LLM sees. Spend time on descriptions.

---

## (2) Rust errors across the PyO3 boundary

The Rust side has one error enum in `src/error.rs`:

```rust
pub enum Error {
    QueryParse(String),
    RegexComplexity(String),
    QueryTimeout { limit_ms: u64 },
    InvalidCursor(String),
    Io(std::io::Error),
    Gix(String),
    MailParse(String),
    Tantivy(tantivy::TantivyError),
    Arrow(arrow::error::ArrowError),
    Parquet(parquet::errors::ParquetError),
    State(String),
}

impl From<Error> for PyErr {
    fn from(e: Error) -> Self {
        match e {
            Error::QueryParse(_)
            | Error::RegexComplexity(_)
            | Error::InvalidCursor(_) => PyValueError::new_err(e.to_string()),
            _ => PyRuntimeError::new_err(e.to_string()),
        }
    }
}
```

This is the **only** place Rust → Python error translation happens.
Don't build ad-hoc `PyValueError::new_err(...)` in other Rust files.

### Three actionable classes

- `Error::QueryParse` — bad query grammar. User fixable.
- `Error::RegexComplexity` — regex can't compile to DFA. User fixable.
- `Error::InvalidCursor` — cursor tampered / wrong signing key. User
  fixable (drop the cursor).

All three become `PyValueError` in Python. The tool handler catches
`ValueError` and converts to `ToolError` with a three-part message:

```python
async def lore_search(request: SearchRequest) -> SearchResponse:
    from kernel_lore_mcp import _core

    try:
        raw = await asyncio.to_thread(_core.run_search, ...)
    except ValueError as exc:
        # User-facing Rust errors — translate with context
        raise ToolError(
            f"{exc}. "
            "Check the query grammar (see docs/mcp/query-routing.md); "
            "for regex, stick to DFA-compatible patterns "
            "(no backreferences, no nested quantifiers)."
        ) from exc

    return SearchResponse.model_validate(raw)
```

### Infrastructure failures

Everything else in the Rust enum (`Io`, `Tantivy`, `Arrow`, `Parquet`,
`State`, `Gix`, `MailParse`) is an infrastructure failure. These
become `PyRuntimeError`. The tool handler **does not** catch them —
let them propagate. FastMCP turns an uncaught exception into
`isError: true` with code `-32603` (INTERNAL_ERROR) and the message.
structlog records the traceback server-side.

```python
# The right amount of handling for internal failures: none.
raw = await asyncio.to_thread(_core.run_search, ...)
# If tantivy explodes, let it explode. structlog captures the
# traceback; FastMCP reports INTERNAL_ERROR to the client.
```

### Timeout

`Error::QueryTimeout` is borderline. It's a user-visible condition
("your query is too expensive") but the Rust side raises it as
`PyRuntimeError`. The Python wrapper can either:

- Let it propagate as INTERNAL_ERROR (acceptable for v1 since we have
  an `asyncio.wait_for` layer that beats it to the punch with
  `ToolError`).
- Or catch and translate:

```python
except RuntimeError as exc:
    if "wall-clock" in str(exc):   # Error::QueryTimeout message
        raise ToolError(
            f"{exc}. "
            "Narrow the query with list:, rt:, or f:. "
            "For broad metadata-only scans use lore_activity."
        ) from exc
    raise
```

Prefer the `asyncio.wait_for` path (see [`concurrency.md`](concurrency.md))
so the message is built in one place.

---

## (3) `fastmcp.ToolError` for expected tool-level failures

When a tool invocation is well-formed but the operation cannot
succeed for reasons the caller should fix, raise
`fastmcp.ToolError`. This produces `isError: true` with the message
visible to the LLM.

```python
from fastmcp import ToolError

async def lore_patch(message_id: str) -> PatchResponse:
    from kernel_lore_mcp import _core

    raw = await asyncio.to_thread(_core.fetch_patch, message_id)
    if raw is None:
        raise ToolError(
            f"message {message_id} has no patch. "
            "Check has_patch in lore_search results before calling lore_patch. "
            "For plain prose messages use lore_message."
        )
    return PatchResponse.model_validate(raw)
```

### When to use `ToolError` vs let it propagate

| Situation | Path |
|---|---|
| Pydantic validation failure | Let it propagate. FastMCP → INVALID_PARAMS. |
| User-fixable Rust error (`PyValueError`) | Catch, re-raise as `ToolError` with three-part message. |
| Infrastructure Rust error (`PyRuntimeError`) | Let propagate. FastMCP → INTERNAL_ERROR. |
| Expected absence (no such message, no patch, no thread) | `ToolError` with three-part message. |
| Policy cap hit (max_bytes, max_results) | `ToolError`; include the cap value in the message. |
| Unknown failure mid-handler | Let propagate. Don't swallow. |

---

## Input validation: return error, never raise ad-hoc

Validation that can't be expressed in pydantic constraints (cross-
field, business-rule) goes in a `@model_validator(mode="after")` on
the request model:

```python
class SearchRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    q: str = Field(min_length=1)
    cursor: str | None = None
    max_results: int = Field(default=20, ge=1, le=200)

    @model_validator(mode="after")
    def _cursor_implies_no_max_results_change(self) -> SearchRequest:
        # When resuming from a cursor, max_results must match the original.
        # Enforced by pydantic before we reach the handler.
        ...
        return self
```

If it genuinely has to live in the handler (depends on runtime
state), return `ToolError` with three parts. Never raise a bare
`ValueError` from inside a tool handler — it becomes INTERNAL_ERROR,
which is wrong for user-fixable problems.

---

## Never expose internals in MCP errors

```python
# WRONG — leaks filesystem path and stack trace
raise ToolError(f"failed to read /var/lib/kernel-lore/shards/linux-cifs/... : {exc}")

# WRONG — raw tantivy error reaches the LLM
raise ToolError(str(tantivy_exc))

# RIGHT — translate to user-visible concept
raise ToolError(
    "index temporarily unavailable during reload. "
    "Retry in a few seconds; /status exposes the generation counter."
)
```

Log the real traceback via structlog. Send only the actionable
message to the wire.

---

## Logging errors with context

All logging goes through `structlog`. See
[`../libraries/structlog.md`](../libraries/structlog.md) for the
stdio-stderr rule — critical.

### Log the exception, send only the message

```python
logger = structlog.get_logger(__name__)

async def lore_search(request: SearchRequest) -> SearchResponse:
    try:
        raw = await asyncio.to_thread(_core.run_search, ...)
    except ValueError as exc:
        logger.warning(
            "search_query_rejected",
            q=request.q[:200],
            reason=str(exc),
        )
        raise ToolError(
            f"{exc}. Check query grammar at docs/mcp/query-routing.md."
        ) from exc
```

### Use `logger.exception` / traceback for unexpected failures

For the FastMCP-level INTERNAL_ERROR path, FastMCP already logs the
traceback. If you have context the logger wouldn't have (query text,
cursor state), bind it to the logger in an earlier middleware or at
the call site.

### Never log secrets

`KLMCP_CURSOR_KEY` is `SecretStr`. Never `.get_secret_value()` in a
log call. The cursor contents are fine to log (they're signed, not
secret); the key is not.

---

## The `from exc` chain

Always chain when re-raising as a higher-level error:

```python
except ValueError as exc:
    raise ToolError(
        f"{exc}. Check the query grammar."
    ) from exc   # preserves __cause__
```

Without `from exc`, Python sets `__context__` implicitly and produces
the "During handling of the above exception..." message in
tracebacks. Explicit `from exc` gives the cleaner "The above
exception was the direct cause of the following exception" chain.
FastMCP doesn't forward the chain to MCP clients (good — internals
stay internal), but server-side tracebacks need it.

---

## Anti-patterns

### Bare `except:`

```python
# WRONG — catches KeyboardInterrupt, SystemExit
try:
    raw = await asyncio.to_thread(_core.run_search, ...)
except:
    raise ToolError("something went wrong")
```

Always `except Exception`, and usually narrower.

### Silent swallowing

```python
# WRONG — index drift goes undetected
try:
    raw = await asyncio.to_thread(_core.run_search, ...)
except Exception:
    raw = {"hits": [], "tiers": []}
```

If you must swallow, log it with enough context to reconstruct.

### Generic messages

```python
raise ToolError("something went wrong")
raise ToolError("failed")
raise ToolError(f"error: {exc}")
```

Every MCP error answers what + how + (usually) alternative.

### Raising `ValueError` from a tool handler

```python
# WRONG — becomes INTERNAL_ERROR, LLM can't self-correct
async def lore_patch(message_id: str) -> PatchResponse:
    if not message_id.startswith("<"):
        raise ValueError("not a message-id")
```

**Fix:** either the pydantic model enforces it (preferred) or raise
`ToolError` with guidance.

### Catch-all hiding bugs

```python
# WRONG — hides AttributeError, TypeError (real bugs)
try:
    return SearchResponse.model_validate(raw)
except Exception:
    return SearchResponse(results=[], ...)   # silent drift
```

Tool handlers can catch broadly (they're the boundary), but only
when they translate to `ToolError` with a meaningful message.
Silent fallback to an empty response hides index bugs.

### Duplicating Rust error translation

Only `src/error.rs` converts `Error` → `PyErr`. If you find yourself
writing `PyValueError::new_err(...)` in `router.rs` or `trigram.rs`,
add a variant to `Error` instead.

---

## Checklist for a new tool handler

1. Request model in `models.py` has strict `ConfigDict(extra="forbid")`
   and rich `Field(...)` constraints.
2. Cross-field invariants via `@model_validator(mode="after")` on the
   request model.
3. Handler is `async def`, returns a pydantic response model.
4. `_core` calls go through `asyncio.to_thread`.
5. Expected absences (no such message, no patch) → `ToolError` with
   three parts.
6. `PyValueError` from Rust → caught, translated to `ToolError`,
   chained with `from exc`.
7. `PyRuntimeError` from Rust → propagate; FastMCP reports
   INTERNAL_ERROR; structlog has the traceback.
8. Never expose filesystem paths, SQL, or raw stack traces in
   `ToolError` messages.

---

## Summary

| Error class | Source | Path to MCP wire |
|---|---|---|
| `pydantic.ValidationError` | Tool input | FastMCP → `isError: true` + INVALID_PARAMS |
| `PyValueError` from `_core` | Bad query / regex / cursor | Handler catches → `ToolError` with three-part message |
| `PyRuntimeError` from `_core` | Infra failure | Propagates → FastMCP INTERNAL_ERROR; logged with traceback |
| `fastmcp.ToolError` | Tool author's choice | → `isError: true` with the message visible to the LLM |
