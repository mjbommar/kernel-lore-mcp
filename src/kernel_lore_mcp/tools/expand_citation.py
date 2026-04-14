"""lore_expand_citation — universal lookup by message-id | SHA | CVE."""

from __future__ import annotations

import asyncio
from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import ExpandCitationResponse, Freshness


async def lore_expand_citation(
    token: Annotated[
        str,
        Field(
            min_length=1,
            max_length=256,
            description="A Message-ID, a git commit SHA (>=7 hex chars), or a CVE ID.",
        ),
    ],
    limit: Annotated[int, Field(ge=1, le=100)] = 25,
) -> ExpandCitationResponse:
    """Resolve whatever the human handed us into concrete message rows."""
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    rows = await asyncio.to_thread(reader.expand_citation, token, limit)
    hits = [row_to_search_hit(r, tier_provenance=["metadata"]) for r in rows]
    return ExpandCitationResponse(results=hits, freshness=Freshness())
