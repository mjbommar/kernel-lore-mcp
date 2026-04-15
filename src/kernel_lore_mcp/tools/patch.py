"""lore_patch — fetch raw patch text for one message-id.

For browsing prose+patch together, use lore_message; for cross-version
diffing use lore_patch_diff.
"""

from __future__ import annotations

import asyncio
from typing import Annotated

from fastmcp.exceptions import ToolError
from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import Freshness, PatchResponse
from kernel_lore_mcp.tools.message import _split_prose_patch


async def lore_patch(
    message_id: Annotated[str, Field(min_length=1, max_length=512)],
) -> PatchResponse:
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    row = await asyncio.to_thread(reader.fetch_message, message_id)
    if row is None:
        raise ToolError(f"message_id {message_id!r} not found")
    if not row.get("has_patch"):
        raise ToolError(f"message_id {message_id!r} has no patch payload")

    body = await asyncio.to_thread(reader.fetch_body, message_id)
    if body is None:
        raise ToolError(
            f"body for {message_id!r} missing from compressed store "
            "(metadata and store out of sync)"
        )
    try:
        body_text = body.decode("utf-8")
    except UnicodeDecodeError:
        body_text = body.decode("latin-1", errors="replace")
    _, patch = _split_prose_patch(body_text)
    if patch is None:
        raise ToolError(
            f"message_id {message_id!r} parsed has_patch=true at ingest "
            "but no diff --git payload survived re-decode"
        )

    return PatchResponse(
        hit=row_to_search_hit(row, tier_provenance=["metadata"]),
        patch=patch,
        body_sha256=row["body_sha256"],
        freshness=Freshness(),
    )
