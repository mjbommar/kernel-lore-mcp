"""lore_search — full router with RRF fusion across metadata + trigram + BM25.

Grammar (lei-compatible subset):
  list:<name>      mailing list filter (metadata)
  dfn:<file>       diff filename (metadata)
  dfhh:<func>      diff hunk function (metadata)
  dfb:<term>       literal substring in patch content (trigram)
  mid:<id>         message-id exact (metadata)
  f:<term>         from address (metadata)
  fixes:<sha>      reverse-lookup patches mentioning this SHA (metadata)
  since:<unix-ns>  lower bound on date
  b:<term>         body term (BM25)
  <bare>           free text (BM25)

Quoted values are supported: `dfb:"some literal"`. Phrase queries on
prose remain rejected by the BM25 tier.
"""

from __future__ import annotations

import asyncio
from typing import Annotated, Literal

from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import SearchResponse

_CONCISE_HITS = 10


async def lore_search(
    query: Annotated[
        str,
        Field(
            min_length=1,
            max_length=512,
            description=(
                "lei-compatible query (subset). Examples: "
                '`dfn:fs/x.c`, `dfb:"smb_check_perm_dacl" list:linux-cifs`, '
                "`fixes:deadbeef`, `ksmbd dacl`. See "
                "docs/mcp/query-routing.md for the full grammar."
            ),
        ),
    ],
    limit: Annotated[int, Field(ge=1, le=200)] = 25,
    cursor: Annotated[str | None, Field(description="HMAC-signed pagination cursor.")] = None,
    response_format: Annotated[
        Literal["concise", "detailed"],
        Field(
            description=(
                "'concise' (default) caps the returned hits at 10 for a fast "
                "agent-budget-friendly summary; 'detailed' returns up to `limit`."
            ),
        ),
    ] = "concise",
) -> SearchResponse:
    """Fused router search across metadata + trigram + BM25 tiers (RRF).

    Cost: moderate — expected p95 300 ms on the typical synthetic corpus.
    """
    _ = cursor  # TODO(phase-5d): cursor consumption (router signs them already)

    from kernel_lore_mcp import _core
    from kernel_lore_mcp.errors import LoreError

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    timeout_s = settings.query_wall_clock_ms / 1000.0
    try:
        rows = await asyncio.wait_for(
            asyncio.to_thread(reader.router_search, query, limit),
            timeout=timeout_s,
        )
    except TimeoutError:
        raise LoreError(
            "query_timeout",
            f"query exceeded the {settings.query_wall_clock_ms} ms wall-clock cap",
            echoed_input={"query": query},
            retry_after_seconds=5,
        ) from None
    total_rows = len(rows)
    if response_format == "concise":
        rows = rows[:_CONCISE_HITS]

    hits = []
    tiers_seen: set[str] = set()
    for row in rows:
        provenance = list(row.get("_tier_provenance") or [])
        for t in provenance:
            tiers_seen.add(t)
        hit = row_to_search_hit(
            row,
            tier_provenance=provenance or ["metadata"],
            is_exact_match=bool(row.get("_is_exact_match", False)),
        )
        hit.score = row.get("_score")
        hits.append(hit)

    default_applied: list[str] = []
    if response_format == "concise" and total_rows > _CONCISE_HITS:
        default_applied.append(f"response_format=concise (showing top {_CONCISE_HITS})")
    return SearchResponse(
        results=hits,
        next_cursor=None,
        query_tiers_hit=sorted(tiers_seen),
        default_applied=default_applied,
        truncated_by_candidate_cap=(response_format == "concise" and total_rows > _CONCISE_HITS),
        freshness=build_freshness(reader),
    )
