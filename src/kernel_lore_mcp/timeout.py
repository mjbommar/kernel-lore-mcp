"""Shared query-timeout wrapper.

Every MCP tool that calls into the Rust reader should wrap its
`asyncio.to_thread(...)` call through `run_with_timeout` so the
5s wall-clock cap from `config.py::query_wall_clock_ms` is
enforced uniformly. Without this, a hung Rust call blocks the
async event loop indefinitely.

Inner runtime is also recorded in Prometheus via
`record_tool_runtime` so `/metrics` can separate tool-body work
from request/dispatch overhead.
"""

from __future__ import annotations

import asyncio
import time
from collections.abc import Callable
from typing import Any

import structlog

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import LoreError
from kernel_lore_mcp.health import suggest_retry_after_seconds
from kernel_lore_mcp.logging_ import profiling_thresholds

log = structlog.get_logger(__name__)


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
    from kernel_lore_mcp.routes.metrics import record_tool_runtime

    settings = get_settings()
    ms = timeout_ms or settings.query_wall_clock_ms
    tool_name = getattr(fn, "__name__", str(fn))
    started = time.monotonic()
    try:
        result = await asyncio.wait_for(
            asyncio.to_thread(fn, *args),
            timeout=ms / 1000.0,
        )
        elapsed = time.monotonic() - started
        record_tool_runtime(tool_name, elapsed, "ok")
        if elapsed >= profiling_thresholds(settings.mode).tool_seconds:
            log.info(
                "tool runtime slow",
                operation=tool_name,
                mode=settings.mode,
                runtime_ms=round(elapsed * 1000, 3),
                timeout_ms=ms,
            )
        return result
    except TimeoutError:
        elapsed = time.monotonic() - started
        record_tool_runtime(tool_name, elapsed, "timeout")
        retry_after_seconds, server_note = suggest_retry_after_seconds(
            data_dir=settings.data_dir,
            timed_out=True,
            query_wall_clock_ms=ms,
        )
        log.warning(
            "tool runtime timeout",
            operation=tool_name,
            mode=settings.mode,
            runtime_ms=round(elapsed * 1000, 3),
            timeout_ms=ms,
        )
        raise LoreError(
            "query_timeout",
            (
                f"query exceeded the {ms} ms wall-clock cap"
                + (f"; {server_note}" if server_note else "")
            ),
            echoed_input=echoed_input or {},
            retry_after_seconds=retry_after_seconds,
        ) from None
    except LoreError as exc:
        elapsed = time.monotonic() - started
        record_tool_runtime(tool_name, elapsed, exc.code)
        if exc.code != "query_timeout" and elapsed >= profiling_thresholds(settings.mode).tool_seconds:
            log.info(
                "tool runtime completed",
                operation=tool_name,
                mode=settings.mode,
                status=exc.code,
                runtime_ms=round(elapsed * 1000, 3),
                timeout_ms=ms,
            )
        raise
    except Exception:
        elapsed = time.monotonic() - started
        record_tool_runtime(tool_name, elapsed, "error")
        log.warning(
            "tool runtime failed",
            operation=tool_name,
            mode=settings.mode,
            runtime_ms=round(elapsed * 1000, 3),
            timeout_ms=ms,
        )
        raise


__all__ = ["run_with_timeout"]
