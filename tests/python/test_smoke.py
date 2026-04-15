"""Smoke tests for the MCP scaffold.

These ride the build-all-the-things road:
 * the Rust extension module loads
 * the FastMCP app assembles
 * the in-process Client can round-trip `tools/list`
 * `lore_search` returns a typed SearchResponse (even if empty today)
"""

from __future__ import annotations

import pytest
from fastmcp import Client


def test_native_version_matches_package() -> None:
    import kernel_lore_mcp

    assert kernel_lore_mcp.native_version() == kernel_lore_mcp.__version__


@pytest.mark.asyncio
async def test_tools_list(client: Client) -> None:
    tools = await client.list_tools()
    names = {t.name for t in tools}
    assert "lore_search" in names


@pytest.mark.asyncio
async def test_lore_search_empty_response(client: Client) -> None:
    # With no data ingested under the default data_dir, BM25 returns
    # no rows — still a valid SearchResponse shape.
    import os
    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        os.environ["KLMCP_DATA_DIR"] = tmp
        try:
            result = await client.call_tool(
                "lore_search", {"query": "unlikely_term_xyzzy", "limit": 5}
            )
        finally:
            os.environ.pop("KLMCP_DATA_DIR", None)

    assert result.data is not None
    assert result.data.results == []
    assert result.data.next_cursor is None
    # Outputs carry the contract fields we care about in CLAUDE.md.
    assert result.data.blind_spots_ref == "blind-spots://coverage"
