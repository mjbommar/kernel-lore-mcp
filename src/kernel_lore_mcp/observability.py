"""Server-side observability helpers.

This module wires Prometheus accounting at the FastMCP request/tool
boundary so overload is visible even when a call fails before the
tool body runs.
"""

from __future__ import annotations

import time
from typing import Any

import mcp.types as mt
from fastmcp.exceptions import NotFoundError, ToolError
from fastmcp.server.middleware import CallNext, Middleware, MiddlewareContext
from pydantic import ValidationError as PydanticValidationError

from kernel_lore_mcp.errors import LoreError
from kernel_lore_mcp.routes.metrics import (
    record_request,
    record_tool_call,
    tool_request_scope,
)


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
            record_request(method, time.monotonic() - started, status_for_exception(exc))
            raise
        record_request(method, time.monotonic() - started, "ok")
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
            record_tool_call(tool_name, time.monotonic() - started, status_for_exception(exc))
            raise
        record_tool_call(tool_name, time.monotonic() - started, "ok")
        return result
