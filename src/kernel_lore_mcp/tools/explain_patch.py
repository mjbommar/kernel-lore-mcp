"""lore_explain_patch — one-call deep view of a single patch.

Returns prose + patch + series timeline + direct replies, so an
agent doesn't have to chain four tool calls to write a sentence
about a patch.
"""

from __future__ import annotations

from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.errors import not_found
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_search_hit, row_to_timeline_entry
from kernel_lore_mcp.models import ExplainPatchResponse
from kernel_lore_mcp.timeout import run_with_timeout
from kernel_lore_mcp.tools.message import _split_prose_patch


async def lore_explain_patch(
    message_id: Annotated[str, Field(min_length=1, max_length=512)],
    max_downstream: Annotated[int, Field(ge=0, le=200)] = 25,
) -> ExplainPatchResponse:
    """One-call deep view of a single patch (prose + patch + series + replies).

    Cost: expensive — expected p95 800 ms (four composed operations).
    """
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    row = await run_with_timeout(reader.fetch_message, message_id)
    if row is None:
        raise not_found(what="message", message_id=message_id)

    body = await run_with_timeout(reader.fetch_body, message_id)
    prose: str | None = None
    patch: str | None = None
    if body is not None:
        try:
            text = body.decode("utf-8")
        except UnicodeDecodeError:
            text = body.decode("latin-1", errors="replace")
        prose, patch = _split_prose_patch(text)

    series_rows = await run_with_timeout(reader.series_timeline, message_id)
    series = [row_to_timeline_entry(r) for r in series_rows]

    # Downstream = direct replies that point at this message-id via
    # in_reply_to. We use the existing `thread` walker capped at
    # max_downstream + 1 (the seed itself counts as one).
    thread_rows = await run_with_timeout(reader.thread, message_id, max_downstream + 1)
    downstream = [
        row_to_search_hit(r, tier_provenance=["metadata"])
        for r in thread_rows
        if r["message_id"] != row["message_id"] and r.get("in_reply_to") == row["message_id"]
    ]

    return ExplainPatchResponse(
        hit=row_to_search_hit(row, tier_provenance=["metadata"]),
        prose=prose,
        patch=patch,
        series=series,
        downstream=downstream[:max_downstream],
        freshness=build_freshness(reader),
    )
