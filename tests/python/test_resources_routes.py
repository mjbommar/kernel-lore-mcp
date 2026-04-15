"""End-to-end tests for the blind-spots MCP resource and the
custom HTTP routes (`/status`, `/metrics`).

The MCP `Client` covers the resource path; Starlette's TestClient
exercises the FastMCP-mounted HTTP routes without standing up
uvicorn.
"""

from __future__ import annotations

import os
from collections.abc import AsyncIterator, Iterator
from pathlib import Path

import pytest
import pytest_asyncio
from fastmcp import Client
from starlette.testclient import TestClient

from kernel_lore_mcp import _core
from kernel_lore_mcp.routes import status as status_mod
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
        run_id="run-0001",
    )
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    status_mod.clear_cache()
    try:
        async with Client(build_server()) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)
        status_mod.clear_cache()


@pytest.fixture
def http_client(tmp_path: Path) -> Iterator[TestClient]:
    """Mount FastMCP's HTTP app and exercise our custom routes."""
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
        run_id="run-0001",
    )
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    status_mod.clear_cache()
    try:
        app = build_server().http_app()
        with TestClient(app) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)
        status_mod.clear_cache()


@pytest.mark.asyncio
async def test_blind_spots_resource_listed(client: Client) -> None:
    resources = await client.list_resources()
    uris = {str(r.uri) for r in resources}
    assert "blind-spots://coverage" in uris


@pytest.mark.asyncio
async def test_blind_spots_resource_body_warns_on_declassification(client: Client) -> None:
    contents = await client.read_resource("blind-spots://coverage")
    text = "".join(getattr(c, "text", "") or "" for c in contents)
    assert "declassified" in text
    assert "security@kernel.org" in text


def test_status_route_reports_generation_and_per_list(http_client: TestClient) -> None:
    r = http_client.get("/status")
    assert r.status_code == 200
    body = r.json()
    assert body["service"] == "kernel-lore-mcp"
    assert body["generation"] >= 1
    assert body["last_ingest_utc"] is not None
    assert "linux-cifs" in body["per_list"]
    shards = body["per_list"]["linux-cifs"]
    assert len(shards) == 1
    assert shards[0]["shard"] == "0"
    assert len(shards[0]["head_oid"]) == 40
    assert body["blind_spots_ref"] == "blind-spots://coverage"


def test_metrics_route_serves_prometheus_text(http_client: TestClient) -> None:
    r = http_client.get("/metrics")
    assert r.status_code == 200
    body = r.text
    # The gauge is published unconditionally; counters appear only after
    # at least one tool call but the HELP/TYPE lines for them are still
    # present in the registry's exposition.
    assert "kernel_lore_mcp_index_generation" in body
    assert "kernel_lore_mcp_tool_calls_total" in body
