"""lore_thread — pull a full conversation by any message-id within it."""

from __future__ import annotations

from typing import Annotated, Literal

from pydantic import Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import not_found
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import ThreadMessage, ThreadResponse
from kernel_lore_mcp.timeout import run_with_timeout
from kernel_lore_mcp.tools.message import _split_prose_patch

_CONCISE_MESSAGES = 10


async def lore_thread(
    message_id: Annotated[str, Field(min_length=1, max_length=512)],
    max_messages: Annotated[int, Field(ge=1, le=500)] = 200,
    response_format: Annotated[
        Literal["concise", "detailed"],
        Field(
            description=(
                "'concise' (default) caps at 10 messages and omits prose+patch "
                "bodies; 'detailed' returns the full thread including bodies."
            ),
        ),
    ] = "concise",
) -> ThreadResponse:
    """Walk in_reply_to / references to return the full thread.

    Cost: moderate — expected p95 300 ms (graph walk + N body fetches).
    """
    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    rows = await run_with_timeout(reader.thread, message_id, max_messages)
    if not rows:
        raise not_found(what="thread seed", message_id=message_id)

    total_rows = len(rows)
    if response_format == "concise":
        rows = rows[:_CONCISE_MESSAGES]

    messages: list[ThreadMessage] = []
    for row in rows:
        prose: str | None = None
        patch: str | None = None
        if response_format == "detailed":
            body = await run_with_timeout(reader.fetch_body, row["message_id"])
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

    # Enforce the configured max-bytes cap. Measure the concatenated
    # prose+patch text; if it exceeds the limit, truncate messages.
    total_bytes = sum(
        len((m.prose or "").encode()) + len((m.patch or "").encode()) for m in messages
    )
    if total_bytes > settings.thread_response_max_bytes:
        # Keep trimming from the tail until under budget.
        while messages and total_bytes > settings.thread_response_max_bytes:
            dropped = messages.pop()
            total_bytes -= len((dropped.prose or "").encode()) + len((dropped.patch or "").encode())

    truncated = (
        total_rows >= max_messages
        or (response_format == "concise" and total_rows > _CONCISE_MESSAGES)
        or total_rows > len(messages)
    )
    return ThreadResponse(
        root_message_id=rows[0]["message_id"],
        messages=messages,
        truncated=truncated,
        freshness=build_freshness(reader),
    )
