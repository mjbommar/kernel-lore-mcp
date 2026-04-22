"""End-to-end tests for the blind-spots MCP resource and the
custom HTTP routes (`/status`, `/metrics`).

The MCP `Client` covers the resource path; Starlette's TestClient
exercises the FastMCP-mounted HTTP routes without standing up
uvicorn.
"""

from __future__ import annotations

import fcntl
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


@pytest.mark.asyncio
async def test_coverage_stats_resource_listed(client: Client) -> None:
    resources = await client.list_resources()
    uris = {str(r.uri) for r in resources}
    assert "stats://coverage" in uris


@pytest.mark.asyncio
async def test_coverage_stats_resource_renders_corpus_facts(
    client: Client,
) -> None:
    contents = await client.read_resource("stats://coverage")
    text = "".join(getattr(c, "text", "") or "" for c in contents)
    # Headline facts the markdown must surface so an LLM can cite.
    assert "Total indexed messages" in text
    assert "linux-cifs" in text
    assert "Tier generations" in text
    # Complementary resource referenced for what is NOT in.
    assert "blind-spots://coverage" in text


def test_status_route_defaults_to_lightweight_shape(http_client: TestClient) -> None:
    r = http_client.get("/status")
    assert r.status_code == 200
    body = r.json()
    assert body["service"] == "kernel-lore-mcp"
    assert body["generation"] >= 1
    assert body["per_list_omitted"] is True
    assert "per_list" not in body


def test_status_route_reports_generation_and_per_list_when_requested(
    http_client: TestClient,
) -> None:
    r = http_client.get("/status?per_list=1")
    assert r.status_code == 200
    body = r.json()
    assert body["service"] == "kernel-lore-mcp"
    assert body["generation"] >= 1
    assert body["last_ingest_utc"] is not None
    assert body["per_list_omitted"] is False
    assert "linux-cifs" in body["per_list"]
    shards = body["per_list"]["linux-cifs"]
    assert len(shards) == 1
    assert shards[0]["shard"] == "0"
    assert len(shards[0]["head_oid"]) == 40
    assert body["blind_spots_ref"] == "blind-spots://coverage"


def test_status_route_reports_cadence_and_freshness(http_client: TestClient) -> None:
    r = http_client.get("/status")
    body = r.json()
    # New fields added with the 5-min cadence work. Freshness should be
    # OK (just-ingested; age << 3x interval).
    assert body["configured_interval_seconds"] == 300  # default policy
    assert body["last_ingest_age_seconds"] is not None
    assert body["last_ingest_age_seconds"] >= 0
    assert body["last_ingest_age_seconds"] < 3 * 300
    assert body["freshness_ok"] is True


def test_metrics_route_serves_prometheus_text(http_client: TestClient) -> None:
    r = http_client.get("/metrics")
    assert r.status_code == 200
    body = r.text
    # The gauge is published unconditionally; counters appear only after
    # at least one tool call but the HELP/TYPE lines for them are still
    # present in the registry's exposition.
    assert "kernel_lore_mcp_index_generation" in body
    assert "kernel_lore_mcp_tool_calls_total" in body


def test_metrics_route_publishes_freshness_gauges(http_client: TestClient) -> None:
    r = http_client.get("/metrics")
    body = r.text
    # Each gauge appears as a HELP + TYPE + value line.
    assert "kernel_lore_mcp_last_ingest_age_seconds" in body
    assert "kernel_lore_mcp_configured_interval_seconds" in body
    assert "kernel_lore_mcp_freshness_ok" in body
    # Configured interval should round-trip the policy default.
    assert "kernel_lore_mcp_configured_interval_seconds 300" in body
    # freshness_ok flips on after a live ingest.
    assert "kernel_lore_mcp_freshness_ok 1.0" in body
    assert "kernel_lore_mcp_writer_lock_present" in body
    assert "kernel_lore_mcp_sync_active" in body


def test_status_route_reports_freshness_false_on_stale_data(
    http_client: TestClient,
) -> None:
    """Dial the age back past 3x interval by backdating the generation
    file's mtime; /status must report freshness_ok=False so monitoring
    can alert.
    """
    import os as _os
    import time as _time

    from kernel_lore_mcp.config import Settings

    data_dir = Settings().data_dir
    gen_file = data_dir / "state" / "generation"
    # 3x default interval + buffer.
    stale_mtime = _time.time() - (3 * 300) - 60
    _os.utime(gen_file, (stale_mtime, stale_mtime))
    status_mod.clear_cache()

    body = http_client.get("/status").json()
    assert body["freshness_ok"] is False
    assert body["last_ingest_age_seconds"] > 3 * 300


def test_status_route_reports_live_sync_state(http_client: TestClient) -> None:
    from kernel_lore_mcp.config import Settings

    data_dir = Settings().data_dir
    state_dir = data_dir / "state"
    lock_file = (state_dir / "writer.lock").open("w+")
    fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    (state_dir / "sync.json").write_text(
        """
        {
          "active": true,
          "stage": "bm25_commit",
          "updated_unix_secs": 2000000000,
          "started_unix_secs": 1999999940,
          "workers": 2
        }
        """.strip()
    )
    status_mod.clear_cache()

    body = http_client.get("/status").json()
    assert body["writer_lock_present"] is True
    assert body["sync_active"] is True
    assert body["sync"]["active"] is True
    assert body["sync"]["stage"] == "bm25_commit"
    assert body["sync"]["writer_lock_present"] is True
    assert body["sync"]["started_utc"].startswith("2033-05-18T03:32:")
    fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)
    lock_file.close()
