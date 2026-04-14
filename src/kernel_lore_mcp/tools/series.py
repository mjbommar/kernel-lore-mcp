"""lore_series_timeline — follow a series version chain."""

from __future__ import annotations

import asyncio
from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.mapping import row_to_timeline_entry
from kernel_lore_mcp.models import Freshness, SeriesTimelineResponse


async def lore_series_timeline(
    message_id: Annotated[str, Field(min_length=1, max_length=512)],
) -> SeriesTimelineResponse:
    """Given any message-id, return sibling versions of the same series
    (matched by normalized subject + from_addr + list), ordered by
    `(series_version, series_index)`.
    """
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    rows = await asyncio.to_thread(reader.series_timeline, message_id)
    entries = [row_to_timeline_entry(r) for r in rows]
    return SeriesTimelineResponse(entries=entries, freshness=Freshness())
