"""lore_thread — pull a full conversation by any message-id within it."""

from __future__ import annotations

import asyncio
from typing import Annotated

from fastmcp.exceptions import ToolError
from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import Freshness, ThreadMessage, ThreadResponse
from kernel_lore_mcp.tools.message import _split_prose_patch


async def lore_thread(
    message_id: Annotated[str, Field(min_length=1, max_length=512)],
    max_messages: Annotated[int, Field(ge=1, le=500)] = 200,
) -> ThreadResponse:
    """Walk in_reply_to / references to return the full thread."""
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    rows = await asyncio.to_thread(reader.thread, message_id, max_messages)
    if not rows:
        raise ToolError(f"message_id {message_id!r} not found in indexed corpus")

    messages: list[ThreadMessage] = []
    for row in rows:
        body = await asyncio.to_thread(reader.fetch_body, row["message_id"])
        prose: str | None = None
        patch: str | None = None
        if body is not None:
            try:
                text = body.decode("utf-8")
            except UnicodeDecodeError:
                text = body.decode("latin-1", errors="replace")
            prose, patch = _split_prose_patch(text)
        messages.append(
            ThreadMessage(
                hit=row_to_search_hit(row, tier_provenance=["metadata"]),
                prose=prose,
                patch=patch,
            )
        )

    return ThreadResponse(
        root_message_id=rows[0]["message_id"],
        messages=messages,
        truncated=len(rows) >= max_messages,
        freshness=Freshness(),
    )
