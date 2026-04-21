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

from kernel_lore_mcp.cursor import decode_cursor, mint_cursor, query_hash
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.kwic import build_snippet_from_body
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import SearchResponse
from kernel_lore_mcp.reader_cache import get_reader
from kernel_lore_mcp.timeout import run_with_timeout


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
    cursor: Annotated[
        str | None,
        Field(
            description=(
                "Opaque HMAC-signed pagination token. Pass a "
                "`next_cursor` from a prior response to resume after "
                "the last returned hit (newest-first by message "
                "date). Bound to the `needle` + `list` + `fuzzy_edits` "
                "combination — changing any invalidates the cursor."
            ),
        ),
    ] = None,
) -> SearchResponse:
    """Substring search over patch bodies (exact or fuzzy).

    Cost: moderate — expected p95 300 ms exact, ~400 ms fuzzy_edits=1.
    SIMD-accelerated Levenshtein confirmation via triple_accel.
    """
    reader = get_reader()

    # Cursor scope: needle + list + fuzzy_edits uniquely define the
    # sorted result set. `limit` is NOT part of the hash so callers
    # can change page size between calls without re-issuing the
    # query.
    q_hash = query_hash("lore_patch_search", needle, list or "", fuzzy_edits)
    resume = decode_cursor(cursor, expected_q_hash=q_hash, arg_name="cursor")

    # Oversample 2x so we have headroom for the cursor skip PLUS a
    # one-row look-ahead to detect "more pages exist." Trigram-
    # confirm is the expensive step; the cap protects against
    # runaway confirm cost (MAX_PATCH_CANDIDATES still applies).
    fetch_budget = max(limit * 2 + 1, 16)
    rows = await run_with_timeout(
        reader.patch_search,
        needle,
        list,
        fetch_budget,
        fuzzy_edits,
        echoed_input={"needle": needle, "fuzzy_edits": fuzzy_edits},
    )

    # Rows come back newest-first by date_unix_ns. Skip past the
    # resume point: strictly older date OR same-date + mid > last.
    if resume is not None:
        last_date, last_mid = resume
        kept: list[dict] = []
        for r in rows:
            date = float(r.get("date_unix_ns") or 0)
            mid = str(r.get("message_id") or "")
            if date < last_date or (date == last_date and mid > last_mid):
                kept.append(r)
        rows = kept

    total_available = len(rows)
    page = rows[:limit]

    hits = []
    for r in page:
        body = await asyncio.to_thread(reader.fetch_body, r["message_id"])
        snippet = build_snippet_from_body(body, needle, r.get("body_sha256"))
        hits.append(row_to_search_hit(r, tier_provenance=["trigram"], snippet=snippet))

    # Emit cursor from the last returned row when more hits remain
    # beyond this page.
    next_cursor: str | None = None
    if page and total_available > limit:
        last = page[-1]
        next_cursor = mint_cursor(
            q_hash=q_hash,
            last_score=float(last.get("date_unix_ns") or 0),
            last_mid=str(last.get("message_id") or ""),
        )

    default_applied: list[str] = []
    if fuzzy_edits > 0:
        default_applied.append(f"fuzzy_edits={fuzzy_edits}")

    return SearchResponse(
        results=hits,
        next_cursor=next_cursor,
        query_tiers_hit=["trigram"] if hits else [],
        default_applied=default_applied,
        freshness=build_freshness(reader),
    )
