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
from kernel_lore_mcp.cursor import decode_cursor, mint_cursor, query_hash
from kernel_lore_mcp.errors import invalid_argument, query_too_long
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
                "Opaque HMAC-signed pagination token. Pass a "
                "`next_cursor` from a prior response to resume after "
                "the last returned hit. Omit on the first call. "
                "Cursors are bound to the query string that produced "
                "them — changing `query` on the next call invalidates "
                "the cursor and returns `invalid_argument`."
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
    # Manual length checks — raise structured LoreErrors so the agent
    # gets consistent `[code] reason` shape instead of pydantic's raw
    # ValidationError.
    if len(query) == 0:
        raise invalid_argument(
            name="query",
            reason="query must be non-empty",
            value=query,
            example="ksmbd dacl",
        )
    if len(query) > _QUERY_CAP:
        raise query_too_long(name="query", length=len(query), limit=_QUERY_CAP)

    # Cursor scope: just the query string. `limit` / `response_format`
    # can legitimately change between pages without invalidating the
    # resumption point; `query` cannot.
    q_hash = query_hash("lore_search", query)
    resume = decode_cursor(cursor, expected_q_hash=q_hash, arg_name="cursor")

    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    # Oversample to see if there's a next page AND to absorb
    # cursor-skipped rows that arrive ahead of the resume point. 2x
    # + 1 is the smallest oversample that handles the common case
    # (cursor points at row `limit` of the prior response) in one
    # round-trip; runaway pagination through many identical-score
    # rows still makes forward progress via the mid tiebreak.
    fetch_budget = max(limit * 2 + 1, _CONCISE_HITS + 1)
    result = await run_with_timeout(
        reader.router_search,
        query,
        fetch_budget,
        echoed_input={"query": query},
    )
    rows = list(result.get("hits") or [])
    router_defaults = list(result.get("default_applied") or [])

    # Apply the cursor: skip rows whose (score, mid) tuple comes at or
    # before the last-seen position under the router's native ordering
    # (score DESC, mid ASC as tiebreaker).
    if resume is not None:
        last_score, last_mid = resume
        kept: list[dict] = []
        for row in rows:
            score = float(row.get("_score") or 0.0)
            mid = str(row.get("message_id") or "")
            if score < last_score or (score == last_score and mid > last_mid):
                kept.append(row)
        rows = kept

    total_available = len(rows)
    effective_limit = _CONCISE_HITS if response_format == "concise" else limit
    page = rows[:effective_limit]

    hits = []
    tiers_seen: set[str] = set()
    for row in page:
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

    # Emit next_cursor only when there's definitely at least one more
    # hit beyond this page. `fetch_budget` is the ceiling the router
    # honored; if we hit it AND still returned a full page, callers
    # might still paginate — mint a cursor from the last page row.
    next_cursor: str | None = None
    if page and total_available > effective_limit:
        last = page[-1]
        next_cursor = mint_cursor(
            q_hash=q_hash,
            last_score=float(last.get("_score") or 0.0),
            last_mid=str(last.get("message_id") or ""),
        )

    default_applied: list[str] = list(router_defaults)
    if response_format == "concise" and total_available > _CONCISE_HITS:
        default_applied.append(f"response_format=concise (showing top {_CONCISE_HITS})")
    return SearchResponse(
        results=hits,
        next_cursor=next_cursor,
        query_tiers_hit=sorted(tiers_seen),
        default_applied=default_applied,
        truncated_by_candidate_cap=(
            response_format == "concise" and total_available > _CONCISE_HITS
        ),
        freshness=build_freshness(reader),
    )
