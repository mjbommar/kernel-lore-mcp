"""End-to-end tests for the Phase-7 low-level retrieval primitives.

Covers: lore_eq, lore_in_list, lore_count, lore_substr_subject,
lore_substr_trailers, lore_regex, lore_diff. The agentic intent is
that an LLM can stack any 2-3 of these to answer a question without
us baking in a higher-level workflow.
"""

from __future__ import annotations

import os
from collections.abc import AsyncIterator
from pathlib import Path

import pytest
import pytest_asyncio
from fastmcp import Client
from fastmcp.exceptions import ToolError

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
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        async with Client(build_server()) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


@pytest.mark.asyncio
async def test_eq_by_from_addr(client: Client) -> None:
    result = await client.call_tool(
        "lore_eq",
        {"field": "from_addr", "value": "alice@example.com"},
    )
    data = result.data
    mids = {h.message_id for h in data.results}
    assert mids == {"m1@x", "m2@x"}
    assert data.total == 2


@pytest.mark.asyncio
async def test_eq_on_touched_file_set_membership(client: Client) -> None:
    result = await client.call_tool(
        "lore_eq",
        {"field": "touched_files", "value": "fs/smb/server/smb2pdu.c"},
    )
    data = result.data
    assert [h.message_id for h in data.results] == ["m2@x"]


@pytest.mark.asyncio
async def test_eq_unknown_field_rejected(client: Client) -> None:
    with pytest.raises(ToolError, match="unknown field"):
        await client.call_tool("lore_eq", {"field": "nonsense", "value": "x"})


@pytest.mark.asyncio
async def test_in_list_unions_values(client: Client) -> None:
    result = await client.call_tool(
        "lore_in_list",
        {
            "field": "touched_files",
            "values": ["fs/smb/server/smbacl.c", "fs/smb/server/smb2pdu.c"],
        },
    )
    mids = {h.message_id for h in result.data.results}
    assert mids == {"m1@x", "m2@x"}


@pytest.mark.asyncio
async def test_count_returns_summary(client: Client) -> None:
    result = await client.call_tool(
        "lore_count",
        {"field": "from_addr", "value": "alice@example.com"},
    )
    data = result.data
    assert data.count == 2
    assert data.distinct_authors == 1
    assert data.earliest_unix_ns is not None
    assert data.latest_unix_ns >= data.earliest_unix_ns
    assert data.earliest_utc is not None and data.latest_utc is not None


@pytest.mark.asyncio
async def test_substr_subject_case_insensitive(client: Client) -> None:
    result = await client.call_tool("lore_substr_subject", {"needle": "KSMBD"})
    mids = {h.message_id for h in result.data.results}
    assert mids == {"m1@x", "m2@x"}


@pytest.mark.asyncio
async def test_substr_trailers_finds_via_fixes_substring(client: Client) -> None:
    result = await client.call_tool(
        "lore_substr_trailers",
        {"name": "fixes", "value_substring": "deadbeef"},
    )
    assert [h.message_id for h in result.data.results] == ["m1@x"]


@pytest.mark.asyncio
async def test_substr_trailers_unknown_name_rejected(client: Client) -> None:
    with pytest.raises(ToolError, match="unknown trailer name"):
        await client.call_tool(
            "lore_substr_trailers",
            {"name": "nonsense", "value_substring": "x"},
        )


@pytest.mark.asyncio
async def test_regex_subject_anchored(client: Client) -> None:
    result = await client.call_tool(
        "lore_regex",
        {
            "field": "subject",
            "pattern": r"\[PATCH v3 1/2\]",
            "anchor_required": False,
        },
    )
    assert [h.message_id for h in result.data.results] == ["m1@x"]


@pytest.mark.asyncio
async def test_regex_unsafe_pattern_rejected(client: Client) -> None:
    # Backref: not DFA-buildable.
    with pytest.raises(ToolError, match="DFA"):
        await client.call_tool(
            "lore_regex",
            {"field": "subject", "pattern": r"(\w+)\1", "anchor_required": False},
        )


@pytest.mark.asyncio
async def test_regex_anchor_required_blocks_dotstar(client: Client) -> None:
    with pytest.raises(ToolError, match="anchored-only"):
        await client.call_tool(
            "lore_regex",
            {"field": "subject", "pattern": ".*ksmbd.*", "anchor_required": True},
        )


@pytest.mark.asyncio
async def test_regex_against_patch_field(client: Client) -> None:
    result = await client.call_tool(
        "lore_regex",
        {
            "field": "patch",
            "pattern": r"smb_check_perm_dacl\(",
            "anchor_required": False,
        },
    )
    assert [h.message_id for h in result.data.results] == ["m1@x"]


@pytest.mark.asyncio
async def test_diff_patch_mode_emits_unified(client: Client) -> None:
    result = await client.call_tool(
        "lore_diff",
        {"a": "m1@x", "b": "m2@x", "mode": "patch"},
    )
    data = result.data
    assert data.a.message_id == "m1@x"
    assert data.b.message_id == "m2@x"
    assert data.mode == "patch"
    # m1's patch touches smbacl.c, m2's touches smb2pdu.c — diff must
    # mention both file paths somewhere.
    assert "smbacl.c" in data.diff
    assert "smb2pdu.c" in data.diff


@pytest.mark.asyncio
async def test_diff_rejects_same_mid(client: Client) -> None:
    with pytest.raises(ToolError, match="must be different"):
        await client.call_tool(
            "lore_diff",
            {"a": "m1@x", "b": "m1@x", "mode": "patch"},
        )


@pytest.mark.asyncio
async def test_diff_unknown_mode_rejected(client: Client) -> None:
    with pytest.raises(ToolError, match="unknown diff mode"):
        await client.call_tool(
            "lore_diff",
            {"a": "m1@x", "b": "m2@x", "mode": "wat"},
        )


@pytest.mark.asyncio
async def test_primitives_listed_in_tools(client: Client) -> None:
    tools = await client.list_tools()
    names = {t.name for t in tools}
    assert {
        "lore_eq",
        "lore_in_list",
        "lore_count",
        "lore_substr_subject",
        "lore_substr_trailers",
        "lore_regex",
        "lore_diff",
    }.issubset(names)
