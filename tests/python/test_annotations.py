"""Sprint 0 / J.1 — every tool declares the full annotation quad + title.

MCP tool annotations gate whether an agent will pick a tool (the
`title` is shown in pickers) and how the client sandboxes it
(`readOnlyHint`, `destructiveHint`, `idempotentHint`,
`openWorldHint`). Missing any of them silently degrades UX.
"""

from __future__ import annotations

import pytest
from fastmcp import Client

from kernel_lore_mcp.server import build_server

EXPECTED_TOOL_NAMES = {
    "lore_search",
    "lore_activity",
    "lore_message",
    "lore_expand_citation",
    "lore_series_timeline",
    "lore_patch_search",
    "lore_thread",
    "lore_patch",
    "lore_patch_diff",
    "lore_explain_patch",
    "lore_eq",
    "lore_in_list",
    "lore_count",
    "lore_substr_subject",
    "lore_substr_trailers",
    "lore_regex",
    "lore_diff",
    "lore_nearest",
    "lore_similar",
}


@pytest.mark.asyncio
async def test_every_tool_has_full_annotation_quad() -> None:
    async with Client(build_server()) as c:
        tools = await c.list_tools()

    seen = {t.name for t in tools}
    missing = EXPECTED_TOOL_NAMES - seen
    assert not missing, f"expected tools absent from registry: {missing}"

    for t in tools:
        if not t.name.startswith("lore_"):
            continue
        ann = t.annotations
        assert ann is not None, f"{t.name}: no annotations"
        assert ann.readOnlyHint is True, f"{t.name}: readOnlyHint must be True"
        assert ann.destructiveHint is False, f"{t.name}: destructiveHint must be False"
        assert ann.idempotentHint is True, f"{t.name}: idempotentHint must be True"
        assert ann.openWorldHint is True, f"{t.name}: openWorldHint must be True"
        assert ann.title, f"{t.name}: title must be a non-empty string"
        # Title stays under ~50 chars so it fits in pickers without wrapping.
        assert len(ann.title) <= 60, f"{t.name}: title too long: {ann.title!r}"
