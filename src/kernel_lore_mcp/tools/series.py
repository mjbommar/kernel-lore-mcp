"""lore_series_timeline — follow a series version chain."""

from __future__ import annotations

from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_timeline_entry
from kernel_lore_mcp.models import SeriesTimelineResponse
from kernel_lore_mcp.timeout import run_with_timeout

_MAX_TIMELINE_ROWS = 200


async def lore_series_timeline(
    message_id: Annotated[str, Field(min_length=1, max_length=512)],
) -> SeriesTimelineResponse:
    """Given any message-id, return sibling versions of the same series
    (matched by normalized subject + from_addr + list), ordered by
    `(series_version, series_index)`.

    Cost: cheap — expected p95 5 ms on corpora with over.db tid
    backfilled (single `scan_eq(Tid)` + post-filter). Older docs
    advertised 50 ms; the measured number on a 17.6M-row corpus is
    well under that.
    """
    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    rows = await run_with_timeout(reader.series_timeline, message_id)
    entries = [row_to_timeline_entry(r) for r in rows[:_MAX_TIMELINE_ROWS]]
    return SeriesTimelineResponse(entries=entries, freshness=build_freshness(reader))
