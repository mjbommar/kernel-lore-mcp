# httpx

> Adapted from KAOS `docs/python/libraries/httpx.md`. Aggressively
> trimmed: we don't have a KAOS-style `HttpClient` wrapper and the
> v1 use cases are minimal.
>
> See also: [`../index.md`](../index.md),
> [`../design/concurrency.md`](../design/concurrency.md),
> [`../design/errors.md`](../design/errors.md).

httpx is **not currently a hard dependency** of `kernel-lore-mcp`. All
lore data arrives via `grokmirror` (out-of-process, separate systemd
unit). If and when we add v1.x features that need outbound HTTP —
`/status` checks against an external source, Patchwork API v2
lookups, anything similar — this is the pattern to use.

If we do add it, pin `httpx>=0.28` in `pyproject.toml`. No
`requests`, no `aiohttp`, no `urllib3` direct usage.

---

## 1. When (and whether) to add httpx

Candidate use cases in v1.x and beyond:

| Use case | Needed? |
|---|---|
| Pull lore shards | No — `grokmirror` handles this, not Python. |
| `/status` external freshness check | Possibly — ping `lore.kernel.org/all/_/text/help/raw` to confirm upstream is reachable. |
| Patchwork API v2 cross-reference | Possibly — resolve `Link:` trailers to Patchwork series metadata. |
| Streaming large downloads | No — we never download large things from Python. |

Before adding httpx: is there actually a Python-side network call, or
can it stay on the Rust side? If Rust does the networking (unlikely
in v1), use `reqwest` there. Don't bridge the boundary unless Python
is the natural owner of the call.

---

## 2. Installing

```toml
# pyproject.toml (only once a use case actually lands)
[project]
dependencies = [
    # ...
    "httpx>=0.28",
]
```

No `httpx[http2]` — our candidate use cases don't need HTTP/2 and the
extra pulls in `h2`.

---

## 3. The short-lived client pattern

For one-shot calls (status check, single Patchwork lookup), use a
scoped `async with`. No long-lived global client.

```python
import httpx

async def check_upstream(url: str, *, settings: Settings) -> bool:
    async with httpx.AsyncClient(
        timeout=httpx.Timeout(connect=5.0, read=10.0, write=5.0, pool=5.0),
        headers={"User-Agent": "kernel-lore-mcp/0.1 (+https://...)"},
    ) as client:
        try:
            resp = await client.head(url)
        except httpx.HTTPError:
            return False
    return resp.status_code < 500
```

### Why short-lived

- Use case is one call per request.
- No connection reuse benefit (different hosts, different cadence).
- `async with` guarantees cleanup on exception.

If a v2 feature needs many requests to the same host (e.g. batch
Patchwork lookups), graduate to a long-lived client owned by a
service object. Not in v1.

---

## 4. Timeouts — always four components

```python
timeout = httpx.Timeout(
    connect=5.0,    # TCP + TLS handshake
    read=10.0,      # Gap between data chunks from server
    write=5.0,      # Gap between chunks to server
    pool=5.0,       # Waiting for a connection slot
)
```

| Component | Typical | Symptom when too low |
|---|---|---|
| `connect` | 5–15s | `httpx.ConnectTimeout` on slow DNS |
| `read` | 10–30s | `httpx.ReadTimeout` on slow responses |
| `write` | 5–15s | `httpx.WriteTimeout` on large request bodies |
| `pool` | 5–10s | `httpx.PoolTimeout` under concurrency |

### Defaults for this project

Keep everything under the per-query wall-clock cap
(`Settings.query_wall_clock_ms`, default 5000ms). If an outbound HTTP
call would take longer than the wall-clock budget for its parent
tool, the call is too slow for synchronous use — either cache it or
do it out-of-band.

### Bad

```python
# No timeout — can hang forever
client = httpx.AsyncClient()

# Single float — hides which phase is slow
client = httpx.AsyncClient(timeout=10.0)
```

---

## 5. Retries

When (not if) we add outbound HTTP, retry logic goes with it. No
retry library — a small dataclass + loop, same shape as everywhere
else.

```python
from dataclasses import dataclass
import asyncio
import random

import httpx


@dataclass(frozen=True)
class RetryPolicy:
    max_retries: int = 3
    backoff_base: float = 1.0
    max_backoff: float = 60.0
    retryable_status: frozenset[int] = frozenset({429, 500, 502, 503, 504})

    def backoff(self, attempt: int) -> float:
        raw = min(self.backoff_base * (2 ** attempt), self.max_backoff)
        return raw * (0.5 + random.random())   # jitter


async def get_with_retry(url: str, *, policy: RetryPolicy = RetryPolicy()) -> httpx.Response:
    async with httpx.AsyncClient(timeout=httpx.Timeout(5.0, read=10.0, write=5.0, pool=5.0)) as client:
        last_exc: Exception | None = None
        for attempt in range(policy.max_retries + 1):
            try:
                resp = await client.get(url)
            except (httpx.ConnectError, httpx.ConnectTimeout, httpx.ReadTimeout) as exc:
                last_exc = exc
            else:
                if resp.status_code not in policy.retryable_status:
                    return resp
                last_exc = httpx.HTTPStatusError(f"{resp.status_code}", request=resp.request, response=resp)

            if attempt >= policy.max_retries:
                raise last_exc
            await asyncio.sleep(policy.backoff(attempt))
        raise RuntimeError("unreachable")
```

### What to retry

| Error / status | Retry? | Why |
|---|---|---|
| `httpx.ConnectTimeout` | Yes | DNS or net transient |
| `httpx.ReadTimeout` | Yes | Server overloaded, transient |
| `httpx.ConnectError` | Yes | Network blip |
| HTTP 429 | Yes, respect `Retry-After` | Server asking for backoff |
| HTTP 500, 502, 503, 504 | Yes | Transient server issues |
| HTTP 401, 403 | No | Won't self-heal |
| HTTP 404 | No | Resource doesn't exist |
| HTTP 400 | No | Our bug or user's bug |

### Rules

- Cap max backoff (`max_backoff=60.0`). Unbounded exponentials
  produce absurd waits.
- Add jitter (`delay * (0.5 + random.random())`). Without jitter,
  clients synchronize and thunder the server.
- Never retry auth errors.
- Log every retry with attempt count (structlog: `warning`,
  `attempt=N`).
- Respect `Retry-After` from 429 responses — use the header value
  instead of exponential backoff when present.

---

## 6. Error handling

Don't build a custom exception hierarchy for httpx errors unless a
use case actually needs it. Our standard: catch at the tool boundary
and translate to `ToolError` with a three-part message (see
[`../design/errors.md`](../design/errors.md)):

```python
async def lookup_patchwork_series(link: str) -> PatchworkSeries | None:
    try:
        resp = await get_with_retry(link)
        resp.raise_for_status()
    except (httpx.HTTPError, httpx.HTTPStatusError) as exc:
        logger.warning("patchwork_lookup_failed", link=link, reason=str(exc))
        return None   # absence → caller decides UX
    return PatchworkSeries.model_validate(resp.json())
```

Up at the tool handler:

```python
series = await lookup_patchwork_series(link)
if series is None:
    # Communicate as a tool-level result, not an error — the LLM can
    # decide whether to proceed without Patchwork context.
    return PatchResponse(..., patchwork_series=None)
```

For fatal upstream failures that the user needs to know about:

```python
except httpx.HTTPStatusError as exc:
    raise ToolError(
        f"Patchwork API returned {exc.response.status_code}. "
        "The upstream service may be down; retry in a few minutes. "
        "lore_search results remain usable without Patchwork context."
    ) from exc
```

### httpx exception hierarchy (for reference)

```
httpx.HTTPError
  httpx.HTTPStatusError       # raise_for_status() on 4xx/5xx
  httpx.RequestError
    httpx.TransportError
      httpx.TimeoutException
        httpx.ConnectTimeout  # TCP/TLS handshake timeout
        httpx.ReadTimeout     # server stopped sending data
        httpx.WriteTimeout
        httpx.PoolTimeout     # pool exhausted
      httpx.NetworkError
        httpx.ConnectError    # DNS / refused
        httpx.ReadError       # connection dropped
```

Catch the narrowest error you can actually recover from. Fall back to
`httpx.HTTPError` only at the catch-all.

---

## 7. Streaming

Not used in v1. If we ever need to stream a large response:

```python
async with client.stream("GET", url) as resp:
    resp.raise_for_status()
    async for chunk in resp.aiter_bytes():
        handle(chunk)
```

Rules:

- Always `async with client.stream(...)` as context manager.
- `aiter_bytes()` for binary, `aiter_text()` for text, `aiter_lines()`
  for NDJSON.
- Break early when you have enough data — do not drain the stream
  for a preview.

---

## 8. Anti-patterns

### No timeout

```python
# WRONG
client = httpx.AsyncClient()
```

Hangs forever on a stalled server. Always set `httpx.Timeout(...)`.

### Blocking sync HTTP from async

```python
# WRONG — blocks the event loop
import requests
async def handler():
    return requests.get(url).json()
```

Use `httpx.AsyncClient` and `await`.

### Long-lived client for one-off calls

Creating an `AsyncClient` at module import time "to save on setup"
when each call is a different host. Each `AsyncClient()` has a
connection pool you're not using. Use `async with` per call.

### Ignoring `Retry-After`

Exponential backoff on a 429 when the server gave you `Retry-After: 30`
is rude. Parse it.

```python
retry_after = resp.headers.get("Retry-After")
if retry_after:
    try:
        await asyncio.sleep(float(retry_after))
    except ValueError:
        # HTTP-date form — rare in practice; fall through to backoff
        pass
```

### Putting httpx in v1 "just in case"

Don't add dependencies speculatively (see
[`../design/dependencies.md`](../design/dependencies.md)). If no code
calls httpx, it's not a dep.

---

## 9. Quick reference

| Task | Pattern |
|---|---|
| One-shot GET | `async with httpx.AsyncClient(timeout=httpx.Timeout(...)) as c: await c.get(url)` |
| Timeouts | `httpx.Timeout(connect=5, read=10, write=5, pool=5)` |
| Retry | Small dataclass + loop with jittered exponential backoff |
| Respect 429 | Parse `Retry-After`, fall back to backoff otherwise |
| Streaming | `async with client.stream("GET", url) as resp: async for chunk in resp.aiter_bytes()` |
| Error translation | Catch at tool boundary, raise `ToolError` with three-part message; log the real reason via structlog |
