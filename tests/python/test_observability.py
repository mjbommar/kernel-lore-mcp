"""Observability wiring tests.

These cover the new hosted-readiness metrics path:
  * end-to-end MCP request status accounting
  * tool-call status accounting
  * inner runtime metric exposure
  * queue/admission wait + in-flight gauges for cost-class saturation
"""

from __future__ import annotations

import asyncio
import os
from collections.abc import AsyncIterator
from pathlib import Path

import mcp.types as mt
import pytest
import pytest_asyncio
from fastmcp import Client
from fastmcp.server.middleware import MiddlewareContext

from kernel_lore_mcp import _core, cost_class
from kernel_lore_mcp.errors import LoreError
from kernel_lore_mcp.observability import MetricsMiddleware
from kernel_lore_mcp.routes import status as status_mod
from kernel_lore_mcp.routes.metrics import REGISTRY, generate_latest, metrics_endpoint
from kernel_lore_mcp.server import build_server
from tests.python.fixtures import make_synthetic_shard


@pytest_asyncio.fixture
async def client(tmp_path: Path) -> AsyncIterator[Client]:
    shard_dir = tmp_path / "shards" / "0.git"
    shard_dir.parent.mkdir(parents=True)
    make_synthetic_shard(shard_dir)
    data_dir = tmp_path / "data"
    data_dir.mkdir()
    _core.ingest_shard(
        data_dir=data_dir,
        shard_path=shard_dir,
        list="linux-cifs",
        shard="0",
        run_id="obs-0001",
    )
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    status_mod.clear_cache()
    try:
        async with Client(build_server()) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)
        status_mod.clear_cache()


@pytest.mark.asyncio
async def test_tool_call_records_ok_request_and_runtime_metrics(
    client: Client,
) -> None:
    await client.call_tool(
        "lore_eq",
        {"field": "from_addr", "value": "alice@example.com"},
    )

    body = generate_latest(REGISTRY).decode()
    assert 'kernel_lore_mcp_requests_total{method="tools/call",status="ok"}' in body
    assert 'kernel_lore_mcp_tool_calls_total{status="ok",tool="lore_eq"}' in body
    assert "kernel_lore_mcp_request_latency_seconds" in body
    assert "kernel_lore_mcp_tool_latency_seconds" in body
    assert "kernel_lore_mcp_tool_runtime_seconds" in body


@pytest.mark.asyncio
async def test_rate_limited_tool_call_records_queue_wait_and_inflight(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setitem(cost_class._LIMITS, "moderate", 1)
    monkeypatch.setitem(
        cost_class._SEMAPHORES,
        "moderate",
        asyncio.Semaphore(1),
    )

    gate = asyncio.Event()

    async def queue_probe_tool() -> str:
        """Synthetic slow tool.

        Cost: moderate — expected p95 100 ms.
        """

        await gate.wait()
        return "ok"

    wrapped = cost_class.cost_limited(queue_probe_tool)
    mw = MetricsMiddleware()
    ctx = MiddlewareContext(
        message=mt.CallToolRequestParams(name="queue_probe_tool", arguments={}),
        source="client",
        type="request",
        method="tools/call",
    )

    async def _invoke(_: MiddlewareContext[mt.CallToolRequestParams]) -> str:
        return await wrapped()

    async def _invoke_via_middleware() -> str:
        return await mw.on_request(ctx, lambda inner: mw.on_call_tool(inner, _invoke))

    first = asyncio.create_task(_invoke_via_middleware())
    await asyncio.sleep(0.01)

    body = generate_latest(REGISTRY).decode()
    assert 'kernel_lore_mcp_tool_inflight{cost_class="moderate"} 1.0' in body

    with pytest.raises(LoreError) as exc_info:
        await _invoke_via_middleware()
    assert exc_info.value.code == "rate_limited"

    body = generate_latest(REGISTRY).decode()
    assert (
        'kernel_lore_mcp_tool_calls_total{status="rate_limited",tool="queue_probe_tool"}'
        in body
    )
    assert 'kernel_lore_mcp_requests_total{method="tools/call",status="rate_limited"}' in body
    assert (
        'kernel_lore_mcp_tool_queue_wait_seconds_bucket{cost_class="moderate",'
        'le="0.001",status="rate_limited",tool="queue_probe_tool"}'
    ) in body

    gate.set()
    await first

    await metrics_endpoint(None)  # type: ignore[arg-type]
    body = generate_latest(REGISTRY).decode()
    assert 'kernel_lore_mcp_tool_inflight{cost_class="moderate"} 0.0' in body
