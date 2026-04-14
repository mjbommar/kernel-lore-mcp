"""End-to-end MCP tool tests: ingest → server → in-process Client.

This is the v0.5 acceptance gate. If all three paths work, an agent
can reach structured metadata about lore over MCP without any
external infrastructure.
"""

from __future__ import annotations

import os
from collections.abc import AsyncIterator
from pathlib import Path

import pytest
import pytest_asyncio
from fastmcp import Client

from kernel_lore_mcp import _core
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

    # Point the server's Settings at this tmp data_dir via env.
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        mcp = build_server()
        async with Client(mcp) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


@pytest.mark.asyncio
async def test_tools_listed(client: Client) -> None:
    tools = await client.list_tools()
    names = {t.name for t in tools}
    assert {
        "lore_search",
        "lore_activity",
        "lore_message",
        "lore_expand_citation",
        "lore_series_timeline",
    }.issubset(names)

    # readOnlyHint on every tool.
    for t in tools:
        if t.name.startswith("lore_"):
            assert t.annotations is not None
            assert t.annotations.readOnlyHint is True


@pytest.mark.asyncio
async def test_lore_activity_by_file(client: Client) -> None:
    result = await client.call_tool(
        "lore_activity",
        {"file": "fs/smb/server/smbacl.c"},
    )
    data = result.data
    assert data.total == 1
    row = data.rows[0]
    assert row.message_id == "m1@x"
    assert row.list == "linux-cifs"
    assert "carol@example.com" in " ".join(row.reviewed_by)
    assert row.cc_stable and "stable@" in row.cc_stable[0]
    assert row.lore_url == "https://lore.kernel.org/linux-cifs/m1@x/"
    assert row.cite_key.startswith("linux-cifs/2026-04/")


@pytest.mark.asyncio
async def test_lore_activity_requires_file_or_function(client: Client) -> None:
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError, match="lore_activity requires"):
        await client.call_tool("lore_activity", {})


@pytest.mark.asyncio
async def test_lore_message_returns_prose_and_patch(client: Client) -> None:
    result = await client.call_tool("lore_message", {"message_id": "m1@x"})
    data = result.data
    assert data.hit.message_id == "m1@x"
    assert data.hit.has_patch is True
    assert data.prose is not None
    assert "Prose here" in data.prose
    assert data.patch is not None
    assert data.patch.startswith("diff --git ")
    assert data.body_length > 0


@pytest.mark.asyncio
async def test_lore_message_not_found(client: Client) -> None:
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError, match="not found"):
        await client.call_tool("lore_message", {"message_id": "nope@x"})


@pytest.mark.asyncio
async def test_lore_expand_citation_via_fixes_sha(client: Client) -> None:
    result = await client.call_tool(
        "lore_expand_citation",
        {"token": "deadbeef01234567"},
    )
    data = result.data
    assert len(data.results) == 1
    assert data.results[0].message_id == "m1@x"
    assert data.results[0].tier_provenance == ["metadata"]
    assert data.results[0].is_exact_match is True


@pytest.mark.asyncio
async def test_lore_expand_citation_via_message_id(client: Client) -> None:
    result = await client.call_tool(
        "lore_expand_citation",
        {"token": "<m2@x>"},
    )
    data = result.data
    assert len(data.results) == 1
    assert data.results[0].message_id == "m2@x"


@pytest.mark.asyncio
async def test_lore_series_timeline(client: Client) -> None:
    result = await client.call_tool(
        "lore_series_timeline",
        {"message_id": "m1@x"},
    )
    data = result.data
    # m1 and m2 have different subject_normalized ("tighten ACL bounds"
    # vs "follow-up"), so each is its own singleton series.
    assert len(data.entries) == 1
    assert data.entries[0].message_id == "m1@x"
    assert data.entries[0].series_version == 3
    assert data.entries[0].series_index == "1/2"
