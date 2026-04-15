"""lore_patch_search — substring search over patch / diff bodies.

Answers: "find me every patch that mentions `smb_check_perm_dacl`
inside a diff hunk." Backed by the trigram tier (fst + roaring) and
confirmed against the decompressed body for byte-exact matches.
"""

from __future__ import annotations

import asyncio
from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.kwic import build_snippet_from_body
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import SearchResponse


async def lore_patch_search(
    needle: Annotated[
        str,
        Field(
            min_length=3,
            max_length=512,
            description=(
                "Literal byte string to find inside patch bodies. Trigram-filtered (min length 3)."
            ),
        ),
    ],
    list: Annotated[
        str | None,
        Field(description="Restrict to one mailing list (e.g. `linux-cifs`)."),
    ] = None,
    limit: Annotated[int, Field(ge=1, le=200)] = 50,
) -> SearchResponse:
    """Literal-substring search over patch bodies; returns confirmed hits only.

    Cost: moderate — expected p95 300 ms (trigram candidate + real-match confirm).
    """
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    rows = await asyncio.to_thread(reader.patch_search, needle, list, limit)
    hits = []
    for r in rows:
        body = await asyncio.to_thread(reader.fetch_body, r["message_id"])
        snippet = build_snippet_from_body(body, needle, r.get("body_sha256"))
        hits.append(row_to_search_hit(r, tier_provenance=["trigram"], snippet=snippet))
    return SearchResponse(
        results=hits,
        next_cursor=None,
        query_tiers_hit=["trigram"] if hits else [],
        default_applied=[],
        freshness=build_freshness(reader),
    )
