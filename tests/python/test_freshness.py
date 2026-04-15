"""Sprint 0 / I — every tool response carries a populated Freshness.

After ingest bumps `<data_dir>/state/generation` from 0 → 1, every
`Freshness` block should surface:
  * `generation` ≥ 1
  * `as_of` = generation-file mtime (within a few seconds of now)
  * `lag_seconds` ≥ 0
  * `last_ingest_utc` = `as_of` (alias kept for wire-compat)

Fresh data_dir with no ingest leaves `as_of`/`lag_seconds` = None
and `generation` = 0 so the client can detect "no data yet."
"""

from __future__ import annotations

import os
from collections.abc import AsyncIterator
from datetime import UTC, datetime
from pathlib import Path

import pytest
import pytest_asyncio
from fastmcp import Client

from kernel_lore_mcp import _core
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.server import build_server
from tests.python.fixtures import make_synthetic_shard


@pytest_asyncio.fixture
async def client_with_data(tmp_path: Path) -> AsyncIterator[tuple[Client, Path]]:
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
        run_id="run-freshness",
    )
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        async with Client(build_server()) as c:
            yield c, data_dir
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


def test_build_freshness_on_empty_data_dir(tmp_path: Path) -> None:
    reader = _core.Reader(tmp_path)
    fresh = build_freshness(reader)
    assert fresh.generation == 0
    assert fresh.as_of is None
    assert fresh.lag_seconds is None
    assert fresh.last_ingest_utc is None


def test_build_freshness_after_ingest(tmp_path: Path) -> None:
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
        run_id="run-fresh-unit",
    )
    reader = _core.Reader(data_dir)
    fresh = build_freshness(reader)

    assert fresh.generation == 1
    assert fresh.as_of is not None
    assert fresh.last_ingest_utc == fresh.as_of
    assert fresh.lag_seconds is not None
    assert fresh.lag_seconds >= 0
    # Within a reasonable slack of wall-clock; generation file was just written.
    now = datetime.now(tz=UTC)
    assert (now - fresh.as_of).total_seconds() < 60


@pytest.mark.asyncio
async def test_lore_search_surfaces_freshness(
    client_with_data: tuple[Client, Path],
) -> None:
    client, _ = client_with_data
    result = await client.call_tool("lore_search", {"query": "ksmbd"})
    fresh = result.data.freshness
    assert fresh.generation == 1
    assert fresh.as_of is not None
    assert fresh.lag_seconds is not None and fresh.lag_seconds >= 0


@pytest.mark.asyncio
async def test_lore_activity_surfaces_freshness(
    client_with_data: tuple[Client, Path],
) -> None:
    client, _ = client_with_data
    result = await client.call_tool(
        "lore_activity",
        {"file": "fs/smb/server/smbacl.c"},
    )
    fresh = result.data.freshness
    assert fresh.generation == 1
    assert fresh.as_of is not None


@pytest.mark.asyncio
async def test_lore_message_surfaces_freshness(
    client_with_data: tuple[Client, Path],
) -> None:
    client, _ = client_with_data
    result = await client.call_tool("lore_message", {"message_id": "m1@x"})
    fresh = result.data.freshness
    assert fresh.generation == 1
    assert fresh.as_of is not None


@pytest.mark.asyncio
async def test_lore_eq_surfaces_freshness(
    client_with_data: tuple[Client, Path],
) -> None:
    client, _ = client_with_data
    result = await client.call_tool(
        "lore_eq",
        {"field": "from_addr", "value": "alice@example.com"},
    )
    fresh = result.data.freshness
    assert fresh.generation == 1
    assert fresh.as_of is not None
