"""Server-side observability helpers.

This module wires Prometheus accounting at the FastMCP request/tool
boundary so overload is visible even when a call fails before the
tool body runs.
"""

from __future__ import annotations

import time
from typing import Any

import mcp.types as mt
import structlog
from fastmcp.exceptions import NotFoundError, ToolError
from fastmcp.server.context import Context as FastMCPContext
from fastmcp.server.middleware import CallNext, Middleware, MiddlewareContext
from pydantic import ValidationError as PydanticValidationError

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import LoreError
from kernel_lore_mcp.logging_ import profiling_thresholds
from kernel_lore_mcp.routes.metrics import (
    record_request,
    record_tool_call,
    tool_request_scope,
)

log = structlog.get_logger(__name__)


def status_for_exception(exc: Exception) -> str:
    """Best-effort status label for request/tool metrics."""
    if isinstance(exc, LoreError):
        return exc.code
    if isinstance(exc, NotFoundError):
        return "not_found"
    if isinstance(exc, PydanticValidationError):
        return "validation_error"
    if isinstance(exc, ToolError):
        return "tool_error"
    return exc.__class__.__name__.lower()


def _request_context_fields(ctx: FastMCPContext | None) -> dict[str, object]:
    if ctx is None:
        return {}
    fields: dict[str, object] = {}
    if ctx.transport is not None:
        fields["transport"] = ctx.transport
    try:
        fields["request_id"] = ctx.request_id
    except RuntimeError:
        pass
    try:
        fields["session_id"] = ctx.session_id
    except RuntimeError:
        pass
    if ctx.client_id is not None:
        fields["client_id"] = ctx.client_id
    return fields


def _is_warning_status(status: str) -> bool:
    return status in {"query_timeout", "rate_limited", "tool_error", "error"}


def _log_request_event(
    context: MiddlewareContext[Any],
    *,
    elapsed_seconds: float,
    status: str,
) -> None:
    method = context.method or "unknown"
    if method == "tools/call":
        return

    settings = get_settings()
    threshold = profiling_thresholds(settings.mode).request_seconds
    slow = elapsed_seconds >= threshold
    if status == "ok" and not slow:
        return

    log_method = log.warning if _is_warning_status(status) else log.info
    log_method(
        "mcp request completed",
        method=method,
        status=status,
        mode=settings.mode,
        slow=slow,
        elapsed_ms=round(elapsed_seconds * 1000, 3),
        **_request_context_fields(context.fastmcp_context),
    )


def _log_tool_event(
    context: MiddlewareContext[mt.CallToolRequestParams],
    *,
    elapsed_seconds: float,
    status: str,
) -> None:
    settings = get_settings()
    threshold = profiling_thresholds(settings.mode).tool_seconds
    slow = elapsed_seconds >= threshold
    if status == "ok" and not slow:
        return

    log_method = log.warning if _is_warning_status(status) else log.info
    log_method(
        "mcp tool completed",
        tool=context.message.name,
        status=status,
        mode=settings.mode,
        slow=slow,
        elapsed_ms=round(elapsed_seconds * 1000, 3),
        **_request_context_fields(context.fastmcp_context),
    )


class MetricsMiddleware(Middleware):
    """Record end-to-end MCP request and tool-call metrics."""

    async def on_request(
        self,
        context: MiddlewareContext[mt.Request[Any, Any]],
        call_next: CallNext[mt.Request[Any, Any], Any],
    ) -> Any:
        method = context.method or "unknown"
        started = time.monotonic()
        try:
            result = await call_next(context)
        except Exception as exc:
            status = status_for_exception(exc)
            elapsed = time.monotonic() - started
            record_request(method, elapsed, status)
            _log_request_event(context, elapsed_seconds=elapsed, status=status)
            raise
        elapsed = time.monotonic() - started
        record_request(method, elapsed, "ok")
        _log_request_event(context, elapsed_seconds=elapsed, status="ok")
        return result

    async def on_call_tool(
        self,
        context: MiddlewareContext[mt.CallToolRequestParams],
        call_next: CallNext[mt.CallToolRequestParams, Any],
    ) -> Any:
        tool_name = context.message.name
        started = time.monotonic()
        try:
            with tool_request_scope(started):
                result = await call_next(context)
        except Exception as exc:
            status = status_for_exception(exc)
            elapsed = time.monotonic() - started
            record_tool_call(tool_name, elapsed, status)
            _log_tool_event(context, elapsed_seconds=elapsed, status=status)
            raise
        elapsed = time.monotonic() - started
        record_tool_call(tool_name, elapsed, "ok")
        _log_tool_event(context, elapsed_seconds=elapsed, status="ok")
        return result
