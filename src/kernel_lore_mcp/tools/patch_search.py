"""lore_patch_search — substring search over patch / diff bodies.

Answers: "find me every patch that mentions `smb_check_perm_dacl`
inside a diff hunk." Backed by the trigram tier (fst + roaring) and
confirmed against the decompressed body for byte-exact matches.

When `fuzzy_edits > 0`, the confirmation step uses SIMD-accelerated
Levenshtein substring search (triple_accel) so single-character
typos and renamed identifiers are found. Cap at 2 on the hosted
instance; local users uncapped.
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
                "Literal byte string to find inside patch bodies. "
                "Trigram-filtered (min length 3). When fuzzy_edits > 0, "
                "finds approximate matches within the edit distance."
            ),
        ),
    ],
    list: Annotated[
        str | None,
        Field(description="Restrict to one mailing list (e.g. `linux-cifs`)."),
    ] = None,
    limit: Annotated[int, Field(ge=1, le=200)] = 50,
    fuzzy_edits: Annotated[
        int,
        Field(
            ge=0,
            le=2,
            description=(
                "Levenshtein edit-distance tolerance for fuzzy matching. "
                "0 = exact (default). 1 = single-char typos. 2 = two-edit "
                "variants. Higher values widen the candidate set and are "
                "capped at 2 on the hosted instance."
            ),
        ),
    ] = 0,
) -> SearchResponse:
    """Substring search over patch bodies (exact or fuzzy).

    Cost: moderate — expected p95 300 ms exact, ~400 ms fuzzy_edits=1.
    SIMD-accelerated Levenshtein confirmation via triple_accel.
    """
    from kernel_lore_mcp import _core
    from kernel_lore_mcp.errors import LoreError

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    timeout_s = settings.query_wall_clock_ms / 1000.0
    try:
        rows = await asyncio.wait_for(
            asyncio.to_thread(reader.patch_search, needle, list, limit, fuzzy_edits),
            timeout=timeout_s,
        )
    except TimeoutError:
        raise LoreError(
            "query_timeout",
            f"query exceeded the {settings.query_wall_clock_ms} ms wall-clock cap",
            echoed_input={"needle": needle, "fuzzy_edits": fuzzy_edits},
            retry_after_seconds=5,
        ) from None
    hits = []
    for r in rows:
        body = await asyncio.to_thread(reader.fetch_body, r["message_id"])
        snippet = build_snippet_from_body(body, needle, r.get("body_sha256"))
        hits.append(row_to_search_hit(r, tier_provenance=["trigram"], snippet=snippet))

    default_applied: list[str] = []
    if fuzzy_edits > 0:
        default_applied.append(f"fuzzy_edits={fuzzy_edits}")

    return SearchResponse(
        results=hits,
        next_cursor=None,
        query_tiers_hit=["trigram"] if hits else [],
        default_applied=default_applied,
        freshness=build_freshness(reader),
    )
