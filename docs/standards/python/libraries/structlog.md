# structlog

> No KAOS source; project-specific.
>
> See also: [`../index.md`](../index.md),
> [`fastmcp.md`](fastmcp.md) (stdout rule),
> [`../design/errors.md`](../design/errors.md).

We use structlog for all logging. The config lives in
`src/kernel_lore_mcp/logging_.py`. The hard rule — which every other
design decision defers to — is that under **stdio transport,
everything goes to stderr**. stdout is reserved for MCP JSON-RPC
framing. A single stray byte on stdout disconnects the client.

---

## 1. The critical rule

Under **stdio transport**:

- `stdout` is MCP framing. Nothing else.
- `stderr` is the log stream. Everything goes here.
- Stdlib `logging` default handler must stream to `stderr`.
- structlog's `LoggerFactory` must also stream to `stderr`.

Under **HTTP transport**:

- Same rule applies (there's no framing conflict, but consistency
  wins). All logs to stderr.

This is configured once in `logging_.configure()`. Don't bypass it.

---

## 2. The configure function

Current `src/kernel_lore_mcp/logging_.py`:

```python
"""structlog configuration.

Invariant: in stdio transport everything structlog emits goes to
stderr. stdout is reserved for MCP JSON-RPC framing. A stray log
line on stdout corrupts the protocol.
"""
from __future__ import annotations

import logging
import sys

import structlog


def configure(*, transport: str, level: str = "INFO") -> None:
    log_level = getattr(logging, level.upper(), logging.INFO)

    # All stdlib logging -> stderr in both modes (MCP stdio owns stdout).
    logging.basicConfig(
        level=log_level,
        stream=sys.stderr,
        format="%(message)s",
    )

    processors: list[structlog.types.Processor] = [
        structlog.contextvars.merge_contextvars,
        structlog.processors.add_log_level,
        structlog.processors.TimeStamper(fmt="iso", utc=True),
    ]

    renderer: structlog.types.Processor
    if transport == "stdio":
        renderer = structlog.processors.JSONRenderer()
    else:
        renderer = structlog.processors.JSONRenderer()
    processors.append(renderer)

    structlog.configure(
        processors=processors,
        wrapper_class=structlog.make_filtering_bound_logger(log_level),
        logger_factory=structlog.PrintLoggerFactory(file=sys.stderr),
        cache_logger_on_first_use=True,
    )
```

Three things to notice:

1. `logging.basicConfig(stream=sys.stderr, ...)` pins the stdlib root
   to stderr — this covers any dependency that uses `logging`
   without going through structlog (e.g. uvicorn, httpx).
2. `PrintLoggerFactory(file=sys.stderr)` pins structlog itself to
   stderr.
3. `cache_logger_on_first_use=True` — bound loggers are resolved
   once and reused. Standard.

### JSON renderer in both modes

We currently emit JSON under both stdio and HTTP. Reasoning:

- Under stdio: JSON is unambiguous and machine-parseable; the client
  is a tool like `claude-code` that doesn't read our stderr anyway.
  Ops wants JSON.
- Under HTTP: systemd-journald and loki prefer JSON.

If we ever want pretty console output during dev (TTY attached), the
pattern is:

```python
renderer: structlog.types.Processor
if transport == "http" and sys.stderr.isatty():
    renderer = structlog.dev.ConsoleRenderer()
else:
    renderer = structlog.processors.JSONRenderer()
```

Not enabled in v1. Simpler wins.

---

## 3. Getting a logger

```python
import structlog

logger = structlog.get_logger(__name__)
```

One logger per module, bound to the module name. Use the structlog
getter, not `logging.getLogger(__name__)` — the latter bypasses the
processor chain.

---

## 4. Structured logging calls

### Basic — event name + keyword context

```python
logger.info(
    "search_request",
    q=request.q[:200],
    list_=request.list_,
    max_results=request.max_results,
)
```

The event name (`"search_request"`) is the first positional arg. It's
the grep anchor — keep it a lowercase underscore-separated stable
identifier, like a metric name.

Everything else is structured context, serialized into the JSON line:

```json
{
  "event": "search_request",
  "level": "info",
  "timestamp": "2026-04-14T18:42:11Z",
  "q": "KVM: x86 nested",
  "list_": null,
  "max_results": 20
}
```

### Errors

```python
try:
    raw = await asyncio.to_thread(_core.run_search, ...)
except ValueError as exc:
    logger.warning(
        "search_query_rejected",
        q=request.q[:200],
        reason=str(exc),
    )
    raise ToolError(...) from exc
```

For unexpected failures, structlog's `.exception()` attaches the
traceback:

```python
except Exception as exc:
    logger.exception("search_internal_failure", q=request.q[:200])
    raise
```

`.exception()` is equivalent to `.error()` + traceback capture.

---

## 5. Context propagation (contextvars)

structlog's `merge_contextvars` processor pulls context from a
`ContextVar` into every log call on the same task. Use it for
request-scoped context (client IP, rate-limit key, MCP request id):

```python
import structlog

log = structlog.get_logger(__name__)

async def handle_search(request):
    structlog.contextvars.bind_contextvars(
        request_id=request.headers.get("x-request-id"),
        client_ip=request.client.host,
    )
    try:
        log.info("search_request", q=request.q[:200])
        ...
    finally:
        structlog.contextvars.clear_contextvars()
```

Every `log.info` / `log.warning` inside that task picks up
`request_id` and `client_ip` automatically. Under multi-worker
uvicorn this is the cleanest way to correlate log lines with a
request.

For v1 we aren't binding contextvars in handlers yet (no middleware
layer). Add it when rate limiting lands.

---

## 6. What to log

### DO log

- **Request-level events**: tool name, arg summary (trimmed), result
  count, wall-clock.
- **Index events**: reader reloads, generation bumps, ingest
  completions.
- **Rejected queries**: query-parse failures, regex-too-complex,
  invalid cursor (with `reason`, without leaking the key).
- **Freshness staleness**: when a list crosses the staleness
  threshold.
- **Cap hits**: max_bytes truncation, max_results truncation.

### DON'T log

- The cursor signing key (`SecretStr.get_secret_value()` — ever).
- Full patch bodies or full email bodies. A 200-byte prefix is fine.
- User PII beyond what's already in the lore archive.
- "hello" / "world" debug noise. Ship it out before the PR.

### Length discipline

Truncate strings at the call site. A JSON line with a 10KB `q` field
is its own problem:

```python
logger.info("search_request", q=request.q[:200], max_results=request.max_results)
```

---

## 7. Integration with FastMCP / uvicorn

Both FastMCP and uvicorn use stdlib `logging`. `logging.basicConfig`
pins the root to stderr, so uvicorn's access log writes to stderr
too. That's correct under both transports.

### Quieting uvicorn access logs

In dev, uvicorn is noisy. If you want to suppress the per-request
access line, adjust the logger level after `logging.basicConfig`:

```python
logging.getLogger("uvicorn.access").setLevel(logging.WARNING)
```

Don't add this to `logging_.configure()` unless we decide project-
wide. For now, leave it at default and let ops tune at deploy.

### Capturing FastMCP internals

FastMCP uses `logging.getLogger("fastmcp")`. Whatever level you set
via `logging.basicConfig` applies. If you want structured logs from
FastMCP specifically, wrap the stdlib logger with structlog via
`structlog.stdlib.LoggerFactory` — **not** configured in v1. Keep it
simple until we need it.

---

## 8. Testing

Logging configuration is global state. Tests that assert on log
output should:

1. Call `configure(transport="http", level="DEBUG")` in a fixture.
2. Use `structlog.testing.capture_logs()` as a context manager.

```python
# tests/python/unit/test_search_logging.py
from structlog.testing import capture_logs

async def test_rejected_query_logs_warning(mcp_client):
    with capture_logs() as cap:
        await mcp_client.call_tool("lore_search", {"q": "/(a+)+/"})
    events = [e["event"] for e in cap]
    assert "search_query_rejected" in events
```

`capture_logs()` intercepts structlog events without going through
the stderr renderer, so your test output stays clean.

---

## 9. Anti-patterns

### stdout in stdio mode

Any of these will corrupt the stdio transport:

```python
print(f"got {len(results)} results")              # stdout
logging.info("result count: %d", len(results))    # stdout if default handler
structlog.get_logger(__name__).info(...)          # OK — configure() pins to stderr
```

`logging.basicConfig(stream=sys.stderr)` in `configure()` catches the
middle case. But if someone calls `logging.basicConfig()` again
elsewhere (with default args, which means stderr in modern Python,
but don't depend on it) — game over. Don't re-basicConfig.

### Logging secrets

```python
# WRONG
logger.info("auth_ok", key=settings.cursor_signing_key.get_secret_value())

# RIGHT
logger.info("auth_ok", cursor_key_present=settings.cursor_signing_key is not None)
```

### f-strings in log calls

```python
# Bad — pre-renders before processor chain sees it
logger.info(f"search request: {request.q}")

# Good — structured kwargs
logger.info("search_request", q=request.q[:200])
```

Pre-rendered f-strings defeat structured logging, lose context, and
can't be filtered by key.

### Multiple `configure()` calls

Don't call `configure()` twice. structlog caches bound loggers on
first use; reconfiguring post-facto leaves stale bound loggers
pointed at the old processor chain. `configure()` runs once from
`__main__.py`.

### Writing logs in `__init__.py`

Module import time is before `configure()` runs. Logging there
produces untimestamped mis-formatted output. Keep import-time code
import-time only.

---

## 10. Quick reference

| Task | Pattern |
|---|---|
| Configure once | `configure(transport=..., level=...)` in `__main__.py` |
| Get a logger | `structlog.get_logger(__name__)` |
| Event + context | `log.info("event_name", key1=val1, key2=val2)` |
| Error with context | `log.warning("event_rejected", reason=str(exc))` |
| Error with traceback | `log.exception("event_failed", ...)` |
| Request-scoped context | `structlog.contextvars.bind_contextvars(request_id=...)` |
| Assert on logs in tests | `structlog.testing.capture_logs()` |
| **Never** | Write to stdout under stdio; log `SecretStr.get_secret_value()`; pre-render f-strings |
