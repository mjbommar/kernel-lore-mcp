"""Per-tool cost-class concurrency caps.

Motivation (production-hardening track): a single global
`rate_limit_per_ip_per_minute` treats a 0.2 ms `fetch_message`
the same as a 20 s `include_mentions`. Under anonymous multi-
tenant load that lets curious traffic saturate workers on the
expensive end of the spectrum.

Approach (minimal viable, per the backlog scope):
  - Parse the `Cost:` line from each tool's docstring to classify
    it as `cheap` / `moderate` / `expensive`. This already exists
    as a testable contract (`tests/python/test_cost_hints.py`).
  - Per-class in-flight counter (asyncio.Semaphore), sized for
    a 4-vCPU deploy. Over-capacity calls reject fast with a
    structured `rate_limited` LoreError — agents back off + retry.
  - No per-IP bookkeeping — single-process server, the semaphore
    IS the budget. A future commit can layer per-IP buckets on
    top for true multi-tenant fairness.

Class limits (tunable via env):
  - cheap:     1024 — effectively no cap for sub-10ms indexed paths
  - moderate:   32  — 4 vCPU × 8, covers BM25/trigram/router
  - expensive:   4  — the tools that fire full-Parquet scans or
                     external LLM calls; strict cap

Environment overrides:
  KLMCP_COST_CAP_CHEAP / _MODERATE / _EXPENSIVE
"""

from __future__ import annotations

import asyncio
import functools
import os
import re
from collections.abc import Awaitable, Callable
from typing import Literal, ParamSpec, TypeVar

from kernel_lore_mcp.errors import LoreError

CostClass = Literal["cheap", "moderate", "expensive"]

_COST_LINE = re.compile(
    r"Cost:\s*(cheap|moderate|expensive)\s*—",
    re.IGNORECASE,
)


def _env_int(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw is None:
        return default
    try:
        v = int(raw)
    except ValueError:
        return default
    return max(1, v)


_LIMITS: dict[CostClass, int] = {
    "cheap": _env_int("KLMCP_COST_CAP_CHEAP", 1024),
    "moderate": _env_int("KLMCP_COST_CAP_MODERATE", 32),
    "expensive": _env_int("KLMCP_COST_CAP_EXPENSIVE", 4),
}

_SEMAPHORES: dict[CostClass, asyncio.Semaphore] = {
    c: asyncio.Semaphore(n) for c, n in _LIMITS.items()
}


def cost_class_of(fn: Callable[..., object]) -> CostClass:
    """Extract the cost class from `fn`'s docstring. Defaults to
    `moderate` when the doc doesn't carry a `Cost:` line — safer
    than letting an un-annotated tool land in the `cheap` bucket
    by accident.
    """
    doc = fn.__doc__ or ""
    m = _COST_LINE.search(doc)
    if not m:
        return "moderate"
    return m.group(1).lower()  # type: ignore[return-value]


def current_inflight(cost: CostClass) -> int:
    """How many calls of `cost` are in flight right now. Test hook."""
    sem = _SEMAPHORES[cost]
    # asyncio.Semaphore._value is the remaining capacity; in-flight
    # count = configured_limit - remaining.
    return _LIMITS[cost] - sem._value  # type: ignore[attr-defined]


def _rate_limited(cost: CostClass, tool_name: str) -> LoreError:
    return LoreError(
        "rate_limited",
        (
            f"server is at capacity for `{cost}` tools "
            f"({_LIMITS[cost]} concurrent max). `{tool_name}` was "
            f"rejected to protect the worker pool from saturation."
        ),
        retry_after_seconds=5,
        echoed_input={"cost_class": cost, "tool": tool_name},
    )


P = ParamSpec("P")
R = TypeVar("R")


def cost_limited(
    fn: Callable[P, Awaitable[R]],
) -> Callable[P, Awaitable[R]]:
    """Wrap an async tool function with a concurrency cap keyed on
    its declared cost class. Rejects fast when the class semaphore
    is exhausted — queueing under load would grow tail latency
    unboundedly, which is worse than asking the agent to back off
    via a structured `rate_limited` error.
    """
    cost = cost_class_of(fn)
    sem = _SEMAPHORES[cost]
    tool_name = fn.__name__

    @functools.wraps(fn)
    async def wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
        # sem.locked() returns True only when capacity is zero.
        # Pair with a zero-timeout acquire for the "reject fast"
        # shape — acquire() would block if locked.
        try:
            await asyncio.wait_for(sem.acquire(), timeout=0.001)
        except TimeoutError as err:
            raise _rate_limited(cost, tool_name) from err
        try:
            return await fn(*args, **kwargs)
        finally:
            sem.release()

    return wrapper


__all__ = [
    "CostClass",
    "cost_class_of",
    "cost_limited",
    "current_inflight",
]
