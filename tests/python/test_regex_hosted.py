"""Hosted-mode posture tests for `lore_regex`.

Public-hosted deployments should reject unsafe regex shapes quickly
instead of spending the full wall-clock budget on full-corpus patch or
prose scans.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from pathlib import Path

import pytest
import pytest_asyncio
from fastmcp import Client
from fastmcp.exceptions import ToolError

from kernel_lore_mcp import _core
from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.server import build_server
from tests.python.fixtures import make_synthetic_shard


@pytest_asyncio.fixture
async def hosted_client(tmp_path: Path) -> AsyncIterator[Client]:
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
        run_id="run-hosted-regex",
    )
    async with Client(build_server(Settings(data_dir=data_dir, mode="hosted"))) as c:
        yield c


@pytest.mark.asyncio
async def test_hosted_regex_requires_list(hosted_client: Client) -> None:
    with pytest.raises(ToolError, match="hosted_restriction"):
        await hosted_client.call_tool(
            "lore_regex",
            {
                "field": "subject",
                "pattern": r"\[PATCH",
            },
        )


@pytest.mark.asyncio
async def test_hosted_regex_rejects_patch_scans(hosted_client: Client) -> None:
    with pytest.raises(ToolError, match="hosted_restriction"):
        await hosted_client.call_tool(
            "lore_regex",
            {
                "field": "patch",
                "pattern": r"smb_check_perm_dacl\(",
                "list": "linux-cifs",
            },
        )


@pytest.mark.asyncio
async def test_hosted_regex_allows_list_scoped_subject_scan(hosted_client: Client) -> None:
    result = await hosted_client.call_tool(
        "lore_regex",
        {
            "field": "subject",
            "pattern": r"\[PATCH v3 1/2\]",
            "list": "linux-cifs",
            "anchor_required": True,
        },
    )
    assert [h.message_id for h in result.data.results] == ["m1@x"]
