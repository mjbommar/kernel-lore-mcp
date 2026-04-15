"""Phase 13a-fuzzy — Levenshtein fuzzy substring search.

Tests cover: fuzzy_edits=0 matches existing behavior, fuzzy_edits=1
finds single-char typos, fuzzy_edits=2 widens further, the fuzzy
flag surfaces in `default_applied`.
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
async def client_with_data(tmp_path: Path) -> AsyncIterator[Client]:
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
        run_id="fuzzy-test",
    )
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        async with Client(build_server()) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


@pytest.mark.asyncio
async def test_fuzzy_edits_zero_matches_exact(client_with_data: Client) -> None:
    exact = await client_with_data.call_tool(
        "lore_patch_search",
        {"needle": "smb_check_perm_dacl", "fuzzy_edits": 0},
    )
    assert exact.data.results, "exact search should find the fixture"
    assert not exact.data.default_applied


@pytest.mark.asyncio
async def test_fuzzy_edits_one_finds_typo(client_with_data: Client) -> None:
    # Introduce a single-char typo: "smb_check_perm_dacX" (last char wrong)
    result = await client_with_data.call_tool(
        "lore_patch_search",
        {"needle": "smb_check_perm_dacX", "fuzzy_edits": 1},
    )
    mids = {h.message_id for h in result.data.results}
    assert "m1@x" in mids, "fuzzy_edits=1 should find m1 despite the single-char typo"
    assert "fuzzy_edits=1" in " ".join(result.data.default_applied)


@pytest.mark.asyncio
async def test_fuzzy_edits_zero_misses_typo(client_with_data: Client) -> None:
    # Same typo but exact mode — should miss.
    result = await client_with_data.call_tool(
        "lore_patch_search",
        {"needle": "smb_check_perm_dacX", "fuzzy_edits": 0},
    )
    assert not result.data.results, "exact search should not find the typo"


@pytest.mark.asyncio
async def test_fuzzy_edits_two_finds_two_char_typo(client_with_data: Client) -> None:
    # Two-char typo: "smb_check_perm_daXX"
    result = await client_with_data.call_tool(
        "lore_patch_search",
        {"needle": "smb_check_perm_daXX", "fuzzy_edits": 2},
    )
    mids = {h.message_id for h in result.data.results}
    assert "m1@x" in mids


@pytest.mark.asyncio
async def test_fuzzy_edits_cap_at_two(client_with_data: Client) -> None:
    # The MCP schema caps at le=2; pydantic validation should reject 3.
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError):
        await client_with_data.call_tool(
            "lore_patch_search",
            {"needle": "smb_check_perm_dacl", "fuzzy_edits": 3},
        )
