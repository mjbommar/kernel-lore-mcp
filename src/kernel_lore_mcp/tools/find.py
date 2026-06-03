"""lore_find — the ?q= you actually want.

One string in, ranked hits out. Routes by query shape:
  - email address       → exact From: match (over.db)
  - commit SHA / CVE    → citation expand (over.db)
  - bare text           → patch-body substring (trigram)

Skips subject/from substring scans by design — neither column has
an inverted index, so any substring scan over 29M rows times out
at the 5s wall-clock cap regardless of `since_days`. When a bare
query likely targets a person's name, the response includes a
`from_substring_not_indexed_pass_email_instead` hint.
"""

from __future__ import annotations

import re
import time
from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import SearchResponse
from kernel_lore_mcp.reader_cache import get_reader
from kernel_lore_mcp.timeout import run_with_timeout

_EMAIL_RE = re.compile(r"^[^\s@]+@[^\s@]+\.[^\s@]+$")
_SHA_RE = re.compile(r"^[0-9a-fA-F]{7,40}$")
_CVE_RE = re.compile(r"^CVE-\d{4}-\d{4,7}$", re.IGNORECASE)
_NAMEISH_RE = re.compile(r"^[A-Za-z][A-Za-z'\-]{2,}$")

_NS_PER_DAY = 86_400 * 1_000_000_000


async def lore_find(
    q: Annotated[
        str,
        Field(
            min_length=2,
            max_length=512,
            description=(
                "Free-text query. Auto-routes: email → exact From: "
                "match; commit SHA (7-40 hex) or CVE → citation lookup; "
                "anything else → patch-body substring (trigram). "
                "Subject/from substring is NOT searched (no index); "
                "pass an exact email if looking for an author."
            ),
        ),
    ],
    limit: Annotated[int, Field(ge=1, le=200)] = 25,
    since_days: Annotated[
        int,
        Field(
            ge=0,
            le=3650,
            description=(
                "Restrict to messages from the last N days. Default 30. "
                "Set to 0 for all-time. Only the From: leg honors this; "
                "trigram patch search is global."
            ),
        ),
    ] = 30,
) -> SearchResponse:
    """Universal search — like lore's `?q=`. One string, ranked hits.

    Cost: cheap — expected p95 500 ms. Single-tier dispatch by query
    shape; no full scans.
    """
    reader = get_reader()
    q = q.strip()
    default_applied: list[str] = []
    tiers_hit: set[str] = set()

    since_unix_ns: int | None = None
    if since_days > 0:
        since_unix_ns = int(time.time() * 1_000_000_000) - since_days * _NS_PER_DAY
        default_applied.append(f"since={since_days}d")

    if _CVE_RE.match(q) or _SHA_RE.match(q):
        rows = await run_with_timeout(
            reader.expand_citation,
            q,
            limit,
            echoed_input={"q": q},
        )
        tiers_hit.add("metadata")
        merged = _tag(rows, "metadata")
    elif _EMAIL_RE.match(q):
        merged = await _email_query(reader, q, since_unix_ns, limit, tiers_hit)
    else:
        merged = await _bare_query(reader, q, limit, tiers_hit, default_applied)

    merged.sort(key=lambda r: r.get("date_unix_ns") or 0, reverse=True)
    page = merged[:limit]

    hits = [
        row_to_search_hit(r, tier_provenance=list(r.get("_tiers") or ["metadata"])) for r in page
    ]

    return SearchResponse(
        results=hits,
        next_cursor=None,
        query_tiers_hit=sorted(tiers_hit),
        default_applied=default_applied,
        freshness=build_freshness(reader),
    )


async def _email_query(
    reader,
    q: str,
    since_unix_ns: int | None,
    limit: int,
    tiers_hit: set[str],
) -> list[dict]:
    budget = max(limit * 2, 50)
    rows = await run_with_timeout(
        reader.eq,
        "from_addr",
        q,
        since_unix_ns,
        None,
        None,
        budget,
        echoed_input={"q": q, "leg": "from_addr"},
    )
    tiers_hit.add("metadata")
    return _tag(rows, "metadata")


async def _bare_query(
    reader,
    q: str,
    limit: int,
    tiers_hit: set[str],
    default_applied: list[str],
) -> list[dict]:
    # Trigram patch-body search. The only indexed substring path we have.
    budget = max(limit * 2, 50)
    rows = await run_with_timeout(
        reader.patch_search,
        q,
        None,
        budget,
        0,
        echoed_input={"q": q, "leg": "patch_search"},
    )
    tiers_hit.add("trigram")

    # If the query looks like a personal name and patch-body returns
    # few hits, the caller probably wanted From:/subject mentions —
    # which we can't serve. Tell them.
    if _NAMEISH_RE.match(q) and len(rows) < limit:
        default_applied.append("from_substring_not_indexed_pass_email_instead")

    return _tag(rows, "trigram")


def _tag(rows: list[dict], tier: str) -> list[dict]:
    seen: dict[str, dict] = {}
    for r in rows:
        mid = r.get("message_id")
        if not mid or mid in seen:
            continue
        r["_tiers"] = [tier]
        seen[mid] = r
    return list(seen.values())
