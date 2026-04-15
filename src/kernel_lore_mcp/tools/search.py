"""lore_search — free-text BM25 search over prose + subject.

Backed by the tantivy tier (`body_prose` + `subject_normalized`
fields, `kernel_prose` analyzer, `IndexRecordOption::WithFreqs` — no
positions, no stemmer). For literal substrings inside patch hunks
use `lore_patch_search` instead.
"""

from __future__ import annotations

import asyncio
from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import Freshness, SearchResponse


async def lore_search(
    query: Annotated[
        str,
        Field(
            min_length=1,
            max_length=512,
            description=(
                "Free-text query against prose bodies + subjects. Phrase "
                'queries ("...") are rejected — use lore_patch_search for '
                "code. See docs/mcp/query-routing.md."
            ),
        ),
    ],
    limit: Annotated[int, Field(ge=1, le=200)] = 25,
    cursor: Annotated[str | None, Field(description="Reserved for pagination.")] = None,
) -> SearchResponse:
    _ = cursor  # TODO(router): HMAC-signed pagination

    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    rows = await asyncio.to_thread(reader.prose_search, query, limit)
    hits = [
        _with_score(row_to_search_hit(r, tier_provenance=["bm25"], is_exact_match=False), r)
        for r in rows
    ]
    return SearchResponse(
        results=hits,
        next_cursor=None,
        query_tiers_hit=["bm25"] if hits else [],
        default_applied=[],
        freshness=Freshness(),
    )


def _with_score(hit, row):
    hit.score = row.get("_score")
    return hit
