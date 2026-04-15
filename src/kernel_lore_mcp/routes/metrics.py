"""`/metrics` Prometheus exposition.

Minimal v1 surface:
  * `kernel_lore_mcp_tool_calls_total{tool, status}` — counter
  * `kernel_lore_mcp_tool_latency_seconds{tool}` — histogram
  * `kernel_lore_mcp_index_generation` — gauge mirroring
    `/status.generation`

Bind localhost-only by default. Exposed via `@mcp.custom_route`.
"""

from __future__ import annotations

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

TOOL_CALLS = Counter(
    "kernel_lore_mcp_tool_calls_total",
    "Total MCP tool invocations.",
    labelnames=("tool", "status"),
    registry=REGISTRY,
)

TOOL_LATENCY = Histogram(
    "kernel_lore_mcp_tool_latency_seconds",
    "Wall-clock latency of MCP tool invocations.",
    labelnames=("tool",),
    registry=REGISTRY,
)


def record_tool_call(tool_name: str, elapsed_seconds: float, status: str = "ok") -> None:
    """Record one tool invocation in the Prometheus metrics.

    Called from the timeout wrapper after every tool call so metrics
    are wired automatically without per-tool boilerplate.
    """
    TOOL_CALLS.labels(tool=tool_name, status=status).inc()
    TOOL_LATENCY.labels(tool=tool_name).observe(elapsed_seconds)


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
    status = get_status()
    INDEX_GENERATION.set(status.get("generation", 0))
    age = status.get("last_ingest_age_seconds")
    LAST_INGEST_AGE.set(age if age is not None else -1)
    CONFIGURED_INTERVAL.set(status.get("configured_interval_seconds", 0))
    ok = status.get("freshness_ok")
    FRESHNESS_OK.set(1 if ok is True else (0 if ok is False else -1))
    body = generate_latest(REGISTRY)
    return Response(content=body, media_type=CONTENT_TYPE_LATEST)
