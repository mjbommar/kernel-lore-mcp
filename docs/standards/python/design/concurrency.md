# Concurrency Patterns

> Adapted from KAOS `docs/python/design/concurrency.md`. Our runtime
> is FastMCP on asyncio; the CPU work lives in Rust. The rules reflect
> that.
>
> See also: [`../index.md`](../index.md), [`boundaries.md`](boundaries.md),
> [`errors.md`](errors.md).

FastMCP runs on asyncio. Every tool handler is `async def`. The CPU
work — tantivy queries, trigram intersections, mail parsing — lives
in Rust under `_core`. The discipline below is how those two worlds
meet without freezing the event loop or breaking thread-safety
assumptions.

---

## Async-first mandate

All MCP tool handlers are `async def`. FastMCP awaits them.

```python
# tools/search.py
async def lore_search(request: SearchRequest) -> SearchResponse:
    ...
```

Sync handlers "work" (FastMCP handles both), but they block the event
loop for the full duration. For anything that touches `_core`, the
network, or disk, you want async.

### When to use async

- **MCP tool handlers** — always.
- **HTTP routes** (`/status`, `/metrics`) — always.
- **Anything that awaits I/O or `asyncio.to_thread`** — always.

### Stay sync for

- **Pure validation / transformation** — pydantic parsing, cursor
  encoding, JSON shaping. Fast, memory-only.
- **Helpers that never touch `_core` or I/O** — keep them `def`, call
  them directly from `async def` handlers.

---

## The Rust call discipline

`_core.*` functions are synchronous from Python's perspective. They
can take tens or hundreds of milliseconds (tantivy search, trigram
candidate expansion, parquet column scan). **Never call them directly
from an `async def` handler without offloading.**

### Rule: `asyncio.to_thread` for every `_core` call

```python
# Good
async def lore_search(request: SearchRequest) -> SearchResponse:
    from kernel_lore_mcp import _core

    raw = await asyncio.to_thread(
        _core.run_search,
        request.model_dump_json(),
        cursor_bytes,
        request.max_results,
    )
    return SearchResponse.model_validate(raw)

# Bad — blocks the event loop for the full query duration
async def lore_search_bad(request: SearchRequest) -> SearchResponse:
    from kernel_lore_mcp import _core
    raw = _core.run_search(...)  # freezes every other coroutine
    ...
```

`asyncio.to_thread` (stdlib, 3.9+) runs the function in the default
thread-pool executor. The Rust code must release the GIL
(`Python::detach` in PyO3 0.28 — renamed from `allow_threads` in
0.27) so other Python threads continue running while the query
executes.

### Why this matters

- **Under stdio transport:** one event loop services one MCP client.
  Blocking the loop on a 200ms tantivy query = no progress reports,
  no heartbeats, no other tool calls.
- **Under HTTP transport:** one event loop services many clients.
  Blocking it on any single query blocks all of them.
- **Under multi-worker uvicorn:** each worker has its own event loop,
  but within a worker the rule still holds.

### NO rayon inside the asyncio reactor

Rust code called via `asyncio.to_thread` runs in a single worker
thread from the asyncio default pool. If the Rust function then
fan-outs work with `rayon::par_iter`, rayon uses its own global
thread pool. This is fine as long as:

1. The rayon work is CPU-bound (it is — tantivy shard queries are).
2. We don't spawn a rayon pool *inside* the reactor (we don't — rayon
   has a lazy-initialized global pool).
3. We don't block the asyncio-worker thread on a synchronous Python
   callback from within rayon workers — that would deadlock via the
   GIL.

What we explicitly **don't** do: call `_core.run_search` from inside
a rayon-spawned task on the Rust side that then re-enters Python.
That path is a recipe for a `PyO3` deadlock. `_core` functions are
called once from Python, do all their work in Rust, and return.

### ProcessPool? No.

Tantivy readers, the trigram `fst`, and the zstd dictionary are all
memory-mapped and cheap to share across threads. Spinning up a
`ProcessPoolExecutor` means paying serialization costs on every call
and duplicating the hot index state. Keep it in one process.

---

## Concurrent I/O

When you do have multiple independent awaits, run them concurrently.

### `asyncio.gather` — fan-out with results

```python
async def composite_lookup(message_ids: list[str]) -> list[Message]:
    return await asyncio.gather(*[
        asyncio.to_thread(_core.fetch_message, mid)
        for mid in message_ids
    ])
```

Good for batching independent `_core` calls when a tool naturally
aggregates across several message IDs. Bound concurrency with a
semaphore if the list could be large.

### `asyncio.TaskGroup` — structured concurrency (3.11+)

For composite operations where any failure should cancel the rest:

```python
async def lore_thread(message_id: str) -> ThreadResponse:
    async with asyncio.TaskGroup() as tg:
        meta_task = tg.create_task(asyncio.to_thread(_core.thread_metadata, message_id))
        bodies_task = tg.create_task(asyncio.to_thread(_core.thread_bodies, message_id))

    return ThreadResponse(metadata=meta_task.result(), bodies=bodies_task.result())
```

Use when partial failure is meaningless. For `lore_search` with
optional tier dispatch, `gather(..., return_exceptions=True)` is
usually the better fit.

### Bounded concurrency with semaphores

If a tool batches N `_core` calls and N could be large:

```python
async def batch_fetch(ids: list[str], *, concurrency: int = 8) -> list[Message]:
    sem = asyncio.Semaphore(concurrency)

    async def _one(mid: str) -> Message:
        async with sem:
            return await asyncio.to_thread(_core.fetch_message, mid)

    return await asyncio.gather(*[_one(mid) for mid in ids])
```

The semaphore bounds how many threads from the default executor we
tie up at once.

---

## Thread safety

### `_core` contract

The Rust side is thread-safe by construction (Rust's `Send`/`Sync` are
compile-time-checked). Tantivy's `Searcher`, the trigram `fst`, the
zstd dict, and the Arrow readers are all `Sync` and can be called
concurrently from multiple threads. The Rust code releases the GIL
before doing any real work.

This means we **don't need a Python-side lock** around `_core` calls
even when multiple uvicorn workers or multiple asyncio tasks hit the
index simultaneously.

### Multi-worker uvicorn

Streamable HTTP transport can run behind multi-worker uvicorn. Each
worker is a separate process with its own memory-mapped view of the
index tiers. Coherence comes from the `state::generation` counter in
Rust, not from any Python-side coordination:

- On each query, Rust checks the generation file (`stat`) and calls
  `reader.reload()` if the counter advanced.
- Two workers racing to reload is harmless; tantivy's reload is
  idempotent and atomic.

So: no `asyncio.Lock`, no `threading.Lock` inside Python for index
coherence. The invariant is maintained on the Rust side.

### When we do need a lock

The only places a Python-side lock matters:

- **Shared rate-limit state** (per-IP counters in custom_route
  middleware). Use `asyncio.Lock` scoped to a single worker; cross-
  worker rate limiting would need Redis or similar. Not in v1.
- **Cached `/status` payload.** Built once per 30s per worker.
  `asyncio.Lock` around the cache refresh is enough.

```python
class StatusCache:
    def __init__(self) -> None:
        self._lock = asyncio.Lock()
        self._value: StatusPayload | None = None
        self._expires_at: float = 0.0

    async def get(self) -> StatusPayload:
        now = time.monotonic()
        if self._value is not None and now < self._expires_at:
            return self._value
        async with self._lock:
            # double-check after acquiring
            if self._value is not None and time.monotonic() < self._expires_at:
                return self._value
            self._value = await self._rebuild()
            self._expires_at = time.monotonic() + 30.0
            return self._value
```

Never use `threading.Lock` inside `async def` — it blocks the event
loop.

---

## Timeouts

Every tool has a hard wall-clock cap. `Settings.query_wall_clock_ms`
defaults to 5000ms.

### Pattern: `asyncio.wait_for`

```python
async def lore_search(request: SearchRequest, *, settings: Settings) -> SearchResponse:
    try:
        raw = await asyncio.wait_for(
            asyncio.to_thread(_core.run_search, ...),
            timeout=settings.query_wall_clock_ms / 1000.0,
        )
    except asyncio.TimeoutError:
        raise ToolError(
            "query exceeded wall-clock limit; "
            "tighten the query (add list:, rt:) or reduce max_results"
        )
    return SearchResponse.model_validate(raw)
```

Note: `asyncio.wait_for` cancels the coroutine but the underlying
thread running `_core.run_search` is not interruptible. The Rust
router also enforces its own internal wall-clock via a `QueryTimeout`
error variant. Double belt-and-braces:

- Rust-side: `Error::QueryTimeout { limit_ms }` fires at the router
  level and returns early.
- Python-side: `asyncio.wait_for` as a floor in case something in Rust
  blocks.

---

## Async context managers

Use `async with` for lifecycle management of resources with async
setup / teardown.

### HTTP client lifecycle (if/when we add httpx)

```python
async with httpx.AsyncClient(timeout=settings.http_timeout) as client:
    resp = await client.get(url)
# client is closed, pool released
```

Scope the context to the minimum necessary lifetime. Don't hold
clients open across a full request if they're only used in one
helper.

---

## The sync-in-async trap

The most common asyncio bug: calling blocking code from an `async def`
function. KAOS-level rules apply here unchanged.

| Blocking call | Async replacement |
|---|---|
| `time.sleep(n)` | `await asyncio.sleep(n)` |
| `requests.get(url)` | httpx async (if we add it) |
| `open(f).read()` for a large file | `await asyncio.to_thread(Path(f).read_text)` |
| Any `_core.*` function | `await asyncio.to_thread(_core.fn, ...)` |

### Identifying blocking calls

1. Enable asyncio debug mode: `PYTHONASYNCIODEBUG=1`. Logs warnings
   when a coroutine holds the loop for >100ms.
2. Profile with wall-clock. If an `async def` takes 500ms and makes
   no `await` calls, it's blocking.

---

## Anti-patterns

### Blocking the event loop

```python
# BAD — freezes all concurrent requests
async def bad_search(request: SearchRequest) -> SearchResponse:
    raw = _core.run_search(...)   # no asyncio.to_thread
    return SearchResponse.model_validate(raw)
```

### Unbounded concurrency

```python
# BAD — 10,000 concurrent _core calls starve the thread pool
async def bad_batch(ids: list[str]) -> list[Message]:
    return await asyncio.gather(*[
        asyncio.to_thread(_core.fetch_message, mid) for mid in ids
    ])
```

**Fix:** semaphore.

### `threading.Lock` inside `async def`

```python
# BAD — blocks the event loop while waiting for the lock
async def bad_cache_get(self, key: str) -> Value:
    with self._threading_lock:
        return await self._fetch(key)
```

**Fix:** `asyncio.Lock`.

### Holding the GIL across a heavy Rust call

On the Rust side, forgetting `py.detach(|| { ... })` around the hot
path means the Python-side `asyncio.to_thread` offload buys nothing —
the GIL is still held, no other Python thread runs. Called out
explicitly in CLAUDE.md.

### Nested `asyncio.run`

```python
# BAD — "This event loop is already running"
async def bad_nested():
    return asyncio.run(other_coro())

# GOOD
async def good_nested():
    return await other_coro()
```

---

## Decision tree

```
Does the operation touch _core, disk, or the network?
  Yes → async def; use asyncio.to_thread for sync _core calls
  No ↓

Is it CPU-bound and >10ms?
  Yes → sync def; call via asyncio.to_thread from async callers
  No ↓

Is it a fast in-memory transform (pydantic validate, cursor encode)?
  Yes → sync def, called directly from async handlers
```

---

## Summary

| Primitive | Use |
|---|---|
| `asyncio.to_thread(_core.fn, ...)` | Every `_core` call. Never direct. |
| `asyncio.gather` | Fan-out independent `_core` calls. |
| `asyncio.TaskGroup` | Composite operation where partial failure is meaningless. |
| `asyncio.Semaphore` | Bound batch sizes hitting the thread pool. |
| `asyncio.Lock` | Protect async shared state (status cache, rate-limit counters). |
| `asyncio.wait_for` | Wall-clock cap layered over the Rust-side timeout. |
| `threading.Lock` | Not used in v1 Python. Rust side handles its own sync. |
