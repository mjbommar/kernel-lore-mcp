"""Sprint 0 / J.2 — every tool description carries a cost-class hint.

Agents pick tools in part by cost. The hint lives in the function
docstring as a single line:

    Cost: <cheap|moderate|expensive> — expected p95 <N> ms.

Missing the hint lets the agent treat an expensive regex scan like
a cheap point-lookup. Lock it in via a registry-level check.
"""

from __future__ import annotations

import re

import pytest
from fastmcp import Client

from kernel_lore_mcp.server import build_server

_COST_LINE = re.compile(
    r"Cost:\s*(cheap|moderate|expensive)\s*—\s*expected\s+p95\s+\d+",
)


@pytest.mark.asyncio
async def test_every_tool_description_declares_cost_class() -> None:
    async with Client(build_server()) as c:
        tools = await c.list_tools()

    missing: list[str] = []
    for t in tools:
        if not t.name.startswith("lore_"):
            continue
        description = t.description or ""
        if not _COST_LINE.search(description):
            missing.append(f"{t.name}: {description!r}")

    assert not missing, "tools missing `Cost: <class> — expected p95 ...` hint:\n" + "\n".join(
        missing
    )
