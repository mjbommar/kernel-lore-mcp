"""lore_search — discovery tool.

Returns a typed `SearchResponse` so FastMCP emits `outputSchema` +
`structuredContent` on the wire. Bare dict returns collapse to a
text blob.

Implementation is a placeholder until the Rust router lands
(TODO.md phase 1).
"""

from __future__ import annotations

from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.models import Freshness, SearchResponse


async def lore_search(
    query: Annotated[
        str,
        Field(
            min_length=1,
            max_length=512,
            description=(
                "lei-compatible query. See `docs/mcp/query-routing.md` for the supported grammar."
            ),
        ),
    ],
    limit: Annotated[int, Field(ge=1, le=200)] = 25,
    cursor: Annotated[str | None, Field(description="Opaque HMAC-signed cursor.")] = None,
) -> SearchResponse:
    """Search the kernel-lore index.

    Returns an empty `SearchResponse` today; wires to the native router
    once it's implemented.
    """
    _ = (query, limit, cursor)
    return SearchResponse(
        results=[],
        next_cursor=None,
        query_tiers_hit=[],
        default_applied=[],
        freshness=Freshness(),
    )
