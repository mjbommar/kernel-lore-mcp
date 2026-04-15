"""Shared query-timeout wrapper.

Every MCP tool that calls into the Rust reader should wrap its
`asyncio.to_thread(...)` call through `run_with_timeout` so the
5s wall-clock cap from `config.py::query_wall_clock_ms` is
enforced uniformly. Without this, a hung Rust call blocks the
async event loop indefinitely.

Every call is also recorded in Prometheus via `record_tool_call`
so /metrics reflects real tool usage without per-tool boilerplate.
"""

from __future__ import annotations

import asyncio
import time
from collections.abc import Callable
from typing import Any

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import LoreError


async def run_with_timeout[T](
    fn: Callable[..., T],
    *args: Any,
    timeout_ms: int | None = None,
    echoed_input: dict[str, Any] | None = None,
) -> T:
    """Run `fn(*args)` in a thread with the configured wall-clock cap.

    On timeout, raises `LoreError("query_timeout", ...)` with the
    echoed input so the agent can retry with a narrower query.
    """
    from kernel_lore_mcp.routes.metrics import record_tool_call

    settings = get_settings()
    ms = timeout_ms or settings.query_wall_clock_ms
    tool_name = getattr(fn, "__name__", str(fn))
    started = time.monotonic()
    try:
        result = await asyncio.wait_for(
            asyncio.to_thread(fn, *args),
            timeout=ms / 1000.0,
        )
        record_tool_call(tool_name, time.monotonic() - started, "ok")
        return result
    except TimeoutError:
        record_tool_call(tool_name, time.monotonic() - started, "timeout")
        raise LoreError(
            "query_timeout",
            f"query exceeded the {ms} ms wall-clock cap",
            echoed_input=echoed_input or {},
            retry_after_seconds=5,
        ) from None


__all__ = ["run_with_timeout"]
