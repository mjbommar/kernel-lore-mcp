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

INDEX_GENERATION = Gauge(
    "kernel_lore_mcp_index_generation",
    "Current index generation counter.",
    registry=REGISTRY,
)


async def metrics_endpoint(request: Request) -> Response:
    INDEX_GENERATION.set(get_status().get("generation", 0))
    body = generate_latest(REGISTRY)
    return Response(content=body, media_type=CONTENT_TYPE_LATEST)
