"""lore_patch_diff — diff two patch versions of the same series.

The "what changed between v2 and v3" workflow. Both inputs are
message-ids; the response carries both hits + a unified diff of the
patch payloads.
"""

from __future__ import annotations

import asyncio
import difflib
from typing import Annotated

from fastmcp.exceptions import ToolError
from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import Freshness, PatchDiffResponse
from kernel_lore_mcp.tools.message import _split_prose_patch


async def _fetch_patch(reader, mid: str) -> tuple[dict, str]:
    row = await asyncio.to_thread(reader.fetch_message, mid)
    if row is None:
        raise ToolError(f"message_id {mid!r} not found")
    body = await asyncio.to_thread(reader.fetch_body, mid)
    if body is None:
        raise ToolError(f"body for {mid!r} missing from compressed store")
    try:
        text = body.decode("utf-8")
    except UnicodeDecodeError:
        text = body.decode("latin-1", errors="replace")
    _, patch = _split_prose_patch(text)
    if patch is None:
        raise ToolError(f"message_id {mid!r} carries no patch payload")
    return row, patch


async def lore_patch_diff(
    a: Annotated[str, Field(min_length=1, max_length=512, description="Older message-id.")],
    b: Annotated[str, Field(min_length=1, max_length=512, description="Newer message-id.")],
) -> PatchDiffResponse:
    if a == b:
        raise ToolError("lore_patch_diff: a and b must be different message-ids")

    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    row_a, patch_a = await _fetch_patch(reader, a)
    row_b, patch_b = await _fetch_patch(reader, b)

    diff = "".join(
        difflib.unified_diff(
            patch_a.splitlines(keepends=True),
            patch_b.splitlines(keepends=True),
            fromfile=f"a/{row_a['message_id']}",
            tofile=f"b/{row_b['message_id']}",
            n=3,
        )
    )

    return PatchDiffResponse(
        a=row_to_search_hit(row_a, tier_provenance=["metadata"]),
        b=row_to_search_hit(row_b, tier_provenance=["metadata"]),
        diff=diff,
        freshness=Freshness(),
    )
