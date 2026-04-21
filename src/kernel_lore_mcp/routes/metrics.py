"""`/metrics` Prometheus exposition.

Current surface:
  * `kernel_lore_mcp_requests_total{method,status}` — MCP request counter
  * `kernel_lore_mcp_request_latency_seconds{method,status}` — end-to-end MCP request latency
  * `kernel_lore_mcp_tool_calls_total{tool,status}` — tool-call counter
  * `kernel_lore_mcp_tool_latency_seconds{tool,status}` — end-to-end tool latency
  * `kernel_lore_mcp_tool_runtime_seconds{tool,status}` — inner reader/runtime latency
  * `kernel_lore_mcp_tool_queue_wait_seconds{tool,cost_class,status}` — time from request entry
    to tool admission (captures pre-tool overhead and fast-reject saturation)
  * `kernel_lore_mcp_tool_inflight{cost_class}` — current in-flight calls per cost class
  * `kernel_lore_mcp_index_generation` — gauge mirroring `/status.generation`

Bind localhost-only by default. Exposed via `@mcp.custom_route`.
"""

from __future__ import annotations

from contextlib import contextmanager
from contextvars import ContextVar
from typing import Iterator

from starlette.requests import Request
from starlette.responses import Response

from kernel_lore_mcp.routes.status import get_status

try:
    from prometheus_client import (
        CONTENT_TYPE_LATEST,
        CollectorRegistry,
        Counter,
        Gauge,
        Histogram,
        generate_latest,
    )
except ImportError:  # pragma: no cover — prometheus_client is a hard dep
    CONTENT_TYPE_LATEST = "text/plain; version=0.0.4"  # type: ignore[assignment]

    def generate_latest(_: object = None) -> bytes:  # type: ignore[misc]
        return b""

    class _Stub:
        def labels(self, **_: object) -> _Stub:
            return self

        def inc(self, *_: object) -> None: ...

        def observe(self, *_: object) -> None: ...

        def set(self, *_: object) -> None: ...

    Counter = Histogram = Gauge = _Stub  # type: ignore[misc, assignment]
    CollectorRegistry = object  # type: ignore[misc, assignment]


REGISTRY = CollectorRegistry()

_LATENCY_BUCKETS = (
    0.001,
    0.005,
    0.01,
    0.025,
    0.05,
    0.1,
    0.25,
    0.5,
    1.0,
    2.5,
    5.0,
    10.0,
)

REQUESTS = Counter(
    "kernel_lore_mcp_requests_total",
    "Total MCP requests handled by the server.",
    labelnames=("method", "status"),
    registry=REGISTRY,
)

REQUEST_LATENCY = Histogram(
    "kernel_lore_mcp_request_latency_seconds",
    "End-to-end MCP request latency.",
    labelnames=("method", "status"),
    buckets=_LATENCY_BUCKETS,
    registry=REGISTRY,
)

TOOL_CALLS = Counter(
    "kernel_lore_mcp_tool_calls_total",
    "Total MCP tool invocations.",
    labelnames=("tool", "status"),
    registry=REGISTRY,
)

TOOL_LATENCY = Histogram(
    "kernel_lore_mcp_tool_latency_seconds",
    "End-to-end wall-clock latency of MCP tool invocations.",
    labelnames=("tool", "status"),
    buckets=_LATENCY_BUCKETS,
    registry=REGISTRY,
)

TOOL_RUNTIME = Histogram(
    "kernel_lore_mcp_tool_runtime_seconds",
    "Inner runtime spent in reader / tool execution helpers.",
    labelnames=("tool", "status"),
    buckets=_LATENCY_BUCKETS,
    registry=REGISTRY,
)

TOOL_QUEUE_WAIT = Histogram(
    "kernel_lore_mcp_tool_queue_wait_seconds",
    "Time from request entry to tool admission / rejection.",
    labelnames=("tool", "cost_class", "status"),
    buckets=_LATENCY_BUCKETS,
    registry=REGISTRY,
)

TOOL_INFLIGHT = Gauge(
    "kernel_lore_mcp_tool_inflight",
    "Current in-flight tool calls per cost class.",
    labelnames=("cost_class",),
    registry=REGISTRY,
)

_CURRENT_TOOL_REQUEST_STARTED: ContextVar[float | None] = ContextVar(
    "kernel_lore_mcp_tool_request_started",
    default=None,
)

def record_request(method: str, elapsed_seconds: float, status: str = "ok") -> None:
    """Record one end-to-end MCP request."""
    REQUESTS.labels(method=method, status=status).inc()
    REQUEST_LATENCY.labels(method=method, status=status).observe(elapsed_seconds)


def record_tool_call(tool_name: str, elapsed_seconds: float, status: str = "ok") -> None:
    """Record one end-to-end tool invocation.

    Called from the FastMCP middleware around `tools/call`, so
    statuses are visible even when the request is rejected before the
    tool body runs.
    """
    TOOL_CALLS.labels(tool=tool_name, status=status).inc()
    TOOL_LATENCY.labels(tool=tool_name, status=status).observe(elapsed_seconds)


def record_tool_runtime(tool_name: str, elapsed_seconds: float, status: str = "ok") -> None:
    """Record inner runtime spent inside timeout-wrapped helpers."""
    TOOL_RUNTIME.labels(tool=tool_name, status=status).observe(elapsed_seconds)


def record_tool_queue_wait(
    tool_name: str,
    cost_class: str,
    elapsed_seconds: float,
    status: str = "ok",
) -> None:
    """Record the delay from request entry to tool admission."""
    TOOL_QUEUE_WAIT.labels(
        tool=tool_name,
        cost_class=cost_class,
        status=status,
    ).observe(elapsed_seconds)


def set_tool_inflight(cost_class: str, count: int) -> None:
    """Update the live in-flight gauge for one cost class."""
    TOOL_INFLIGHT.labels(cost_class=cost_class).set(count)


def current_tool_request_started() -> float | None:
    """Current per-tool request start time, if any."""
    return _CURRENT_TOOL_REQUEST_STARTED.get()


@contextmanager
def tool_request_scope(started: float) -> Iterator[None]:
    """Expose the current tool request's start time to deeper layers."""
    token = _CURRENT_TOOL_REQUEST_STARTED.set(started)
    try:
        yield
    finally:
        _CURRENT_TOOL_REQUEST_STARTED.reset(token)


INDEX_GENERATION = Gauge(
    "kernel_lore_mcp_index_generation",
    "Current index generation counter.",
    registry=REGISTRY,
)

LAST_INGEST_AGE = Gauge(
    "kernel_lore_mcp_last_ingest_age_seconds",
    "Seconds since the last successful ingest commit. -1 if no ingest yet.",
    registry=REGISTRY,
)

CONFIGURED_INTERVAL = Gauge(
    "kernel_lore_mcp_configured_interval_seconds",
    "Configured grokmirror pull interval.",
    registry=REGISTRY,
)

FRESHNESS_OK = Gauge(
    "kernel_lore_mcp_freshness_ok",
    "1 if last-ingest-age < 3x configured interval, 0 otherwise, -1 if unknown.",
    registry=REGISTRY,
)


async def metrics_endpoint(request: Request) -> Response:
    from kernel_lore_mcp.cost_class import current_inflight

    for cost_class in ("cheap", "moderate", "expensive"):
        set_tool_inflight(cost_class, current_inflight(cost_class))
    status = get_status()
    INDEX_GENERATION.set(status.get("generation", 0))
    age = status.get("last_ingest_age_seconds")
    LAST_INGEST_AGE.set(age if age is not None else -1)
    CONFIGURED_INTERVAL.set(status.get("configured_interval_seconds", 0))
    ok = status.get("freshness_ok")
    FRESHNESS_OK.set(1 if ok is True else (0 if ok is False else -1))
    body = generate_latest(REGISTRY)
    return Response(content=body, media_type=CONTENT_TYPE_LATEST)
