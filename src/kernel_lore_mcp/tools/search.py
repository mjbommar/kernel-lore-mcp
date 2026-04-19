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

from typing import Annotated, Literal

from pydantic import Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import invalid_argument, invalid_cursor, query_too_long
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import SearchResponse
from kernel_lore_mcp.timeout import run_with_timeout

_CONCISE_HITS = 10

# Server-side query cap. 2048 accommodates pasted kernel stack traces
# and compiler errors without letting a caller weaponize the BM25
# parser with a megabyte of text. Enforced manually (inside the tool
# body) so the agent sees a structured `query_too_long` error instead
# of a raw pydantic ValidationError.
_QUERY_CAP = 2048


async def lore_search(
    query: Annotated[
        str,
        Field(
            description=(
                "lei-compatible query (subset). Examples: "
                '`dfn:fs/x.c`, `dfb:"smb_check_perm_dacl" list:linux-cifs`, '
                "`fixes:deadbeef`, `ksmbd dacl`. See "
                "docs/mcp/query-routing.md for the full grammar. "
                "Server cap: 2048 characters."
            ),
        ),
    ],
    limit: Annotated[int, Field(ge=1, le=200)] = 25,
    cursor: Annotated[
        str | None,
        Field(
            description=(
                "Reserved for future HMAC-signed pagination. The "
                "server never issues cursors in this version, so any "
                "value supplied here is rejected with `invalid_cursor`. "
                "Omit this field to request the first (and currently "
                "only) page."
            ),
        ),
    ] = None,
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
    # Cursor contract: this server does not issue pagination cursors
    # yet (phase-5d work). Any caller-supplied cursor is by definition
    # forged or stale. Reject explicitly rather than silently ignoring
    # — silent acceptance broke the adversarial probe and violated the
    # HMAC-signed-cursor contract documented in CLAUDE.md.
    if cursor is not None:
        raise invalid_cursor(
            reason="pagination is not available in this server version",
            cursor=cursor,
        )

    # Manual length checks — raise structured LoreErrors so the agent
    # gets consistent `[code] reason` shape instead of pydantic's raw
    # ValidationError. Empty-string rejection is duplicated here so
    # both cases travel the same error pipeline.
    if len(query) == 0:
        raise invalid_argument(
            name="query",
            reason="query must be non-empty",
            value=query,
            example="ksmbd dacl",
        )
    if len(query) > _QUERY_CAP:
        raise query_too_long(name="query", length=len(query), limit=_QUERY_CAP)

    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    result = await run_with_timeout(
        reader.router_search,
        query,
        limit,
        echoed_input={"query": query},
    )
    rows = list(result.get("hits") or [])
    router_defaults = list(result.get("default_applied") or [])
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

    default_applied: list[str] = list(router_defaults)
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
