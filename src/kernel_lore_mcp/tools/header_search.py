"""lore_header_search — indexed search across header / trailer fields.

Talks directly to the indexed `over_trailer_email`, `over_trailer_ref`,
and `over` (from_addr) tables in over.db. Sub-second on the full
corpus because every supported field has a btree index.

Supported `field` values:
  Email-bearing trailers (from over_trailer_email):
    signed_off_by, reviewed_by, acked_by, tested_by,
    co_developed_by, reported_by
  Ref-bearing trailers (from over_trailer_ref):
    fixes, link, closes, reported_by_ref
  RFC822 header (from over):
    from

`match=prefix` (default) uses anchored LIKE and seeks the index in
sub-ms. `match=contains` falls back to an index-only scan over the
keyed prefix — slower for high-volume kinds like `signed_off_by`
(~15M rows) but still bounded by `since_days`.

NOT YET indexed (would need ingest changes):
  Assisted-by:, Suggested-by:, To:, Cc:
"""

from __future__ import annotations

import asyncio
import sqlite3
import time
from pathlib import Path
from typing import Annotated, Literal

from pydantic import Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import SearchResponse
from kernel_lore_mcp.reader_cache import get_reader

_EMAIL_KINDS = {
    "signed_off_by",
    "reviewed_by",
    "acked_by",
    "tested_by",
    "co_developed_by",
    "reported_by",
    "suggested_by",
    "helped_by",
    "assisted_by",
    # Long-tail trailers indexed via the generic ddd.trailers walker
    # added in over.rs::extra_trailer_email_kinds. Populations observed
    # in the 29.5 M-row corpus: cc 1.8 M (patch-body Cc: lines —
    # includes but is not limited to Cc:stable@…), reported_and_tested_by
    # 23.7 K, originally_by 3.4 K, inspired_by 1.1 K. Any other email-
    # bearing trailer kind written by `parse.rs::extract_trailers` lands
    # in `over_trailer_email` automatically and is queryable with the
    # exact `kind` string from the trailers map (e.g. `nacked_by`).
    "cc",
    "originally_by",
    "inspired_by",
    "reported_and_tested_by",
    # RFC822 envelope addresses — distinct from body-trailer `to:` /
    # `cc:` matches. Populated by `backfill_envelope_addresses` and
    # by every new `ingest_shard` call. Use these when you want to
    # know "was X addressed at SMTP time on this message?" — the
    # body-trailer kinds miss anyone who was on the original list
    # post but never quoted in the patch text.
    "to_env",
    "cc_env",
}
_REF_KINDS = {"fixes", "link", "closes", "reported_by_ref"}
_FIELD_FROM = "from"

_NS_PER_DAY = 86_400 * 1_000_000_000


async def lore_header_search(
    field: Annotated[
        Literal[
            "signed_off_by",
            "reviewed_by",
            "acked_by",
            "tested_by",
            "co_developed_by",
            "reported_by",
            "suggested_by",
            "helped_by",
            "assisted_by",
            "cc",
            "originally_by",
            "inspired_by",
            "reported_and_tested_by",
            "to_env",
            "cc_env",
            "reported_by_ref",
            "fixes",
            "link",
            "closes",
            "from",
        ],
        Field(
            description=(
                "Header or trailer to search. Email-bearing trailers "
                "(signed_off_by, reviewed_by, acked_by, tested_by, "
                "co_developed_by, reported_by, suggested_by, helped_by, "
                "assisted_by, cc, originally_by, inspired_by, "
                "reported_and_tested_by — assisted_by also matches "
                "Co-authored-by:) match against the address; envelope "
                "recipients (to_env, cc_env) match RFC822 To: / Cc: "
                "headers (i.e. who the mail was sent to at SMTP time, "
                "distinct from body-trailer `to:` / `cc:` lines); "
                "ref-bearing "
                "trailers (fixes, link, closes, reported_by_ref) match "
                "against the ref value (SHA prefix, URL substring, "
                "syzbot hash, etc.); `from` matches the message From: "
                "address."
            ),
        ),
    ],
    value: Annotated[
        str,
        Field(
            min_length=2,
            max_length=256,
            description="Substring or prefix of the trailer value to search for.",
        ),
    ],
    match: Annotated[
        Literal["prefix", "contains"],
        Field(
            description=(
                "`prefix` (default) seeks the btree index — fastest. "
                "`contains` scans the keyed prefix range — slower but "
                "matches mid-string occurrences (e.g. surname inside an "
                "email address)."
            ),
        ),
    ] = "prefix",
    list: Annotated[
        str | None,
        Field(description="Restrict to one mailing list (e.g. `linux-rdma`)."),
    ] = None,
    since_days: Annotated[
        int,
        Field(
            ge=0,
            le=3650,
            description=(
                "Restrict to messages from the last N days. Default 30. Set to 0 for all-time."
            ),
        ),
    ] = 30,
    limit: Annotated[int, Field(ge=1, le=200)] = 50,
) -> SearchResponse:
    """Indexed search on header / trailer fields. Sub-second by design.

    Cost: cheap — expected p95 1500 ms. Every supported field has a
    btree index (`over_trailer_email`, `over_trailer_ref`,
    `over_from_date`); prefix matches index-seek in sub-ms, contains
    matches do a keyed-prefix scan.
    """
    reader = get_reader()
    data_dir: Path = get_settings().data_dir
    over_path = data_dir / "over.db"

    since_unix_ns: int | None = None
    default_applied: list[str] = []
    if since_days > 0:
        since_unix_ns = int(time.time() * 1_000_000_000) - since_days * _NS_PER_DAY
        default_applied.append(f"since={since_days}d")

    pattern = value + "%" if match == "prefix" else f"%{value}%"
    fetch_budget = max(limit * 2, 50)

    rows = await asyncio.to_thread(
        _query_index,
        over_path,
        field,
        pattern,
        since_unix_ns,
        list,
        fetch_budget,
    )

    page = rows[:limit]
    tiers_hit = ["metadata"] if page else []
    hits = [row_to_search_hit(r, tier_provenance=["metadata"]) for r in page]

    return SearchResponse(
        results=hits,
        next_cursor=None,
        query_tiers_hit=tiers_hit,
        default_applied=default_applied,
        freshness=build_freshness(reader),
    )


def _query_index(
    over_path: Path,
    field: str,
    pattern: str,
    since_unix_ns: int | None,
    list_filter: str | None,
    limit: int,
) -> list[dict]:
    """Run the keyed index query + hydrate from the `over` table.

    Two passes: (1) index seek/scan returns (message_id, list,
    date_unix_ns) tuples; (2) one SELECT joins them back to `over`
    for from_addr + subject_normalized.
    """
    uri = f"file:{over_path}?mode=ro"
    db = sqlite3.connect(uri, uri=True, timeout=5.0)
    db.row_factory = sqlite3.Row
    try:
        # Build the keyed-index query for the requested field. Each
        # branch uses an existing btree index — verified via PRAGMA
        # index_list at design time.
        params: list = []
        if field in _EMAIL_KINDS:
            sql = (
                "SELECT message_id, list, date_unix_ns FROM over_trailer_email "
                "WHERE kind = ? AND email LIKE ?"
            )
            params.extend([field, pattern])
        elif field == "reported_by_ref":
            sql = (
                "SELECT message_id, list, date_unix_ns FROM over_trailer_ref "
                "WHERE kind = 'reported_by' AND ref_value LIKE ?"
            )
            params.append(pattern)
        elif field in _REF_KINDS:
            sql = (
                "SELECT message_id, list, date_unix_ns FROM over_trailer_ref "
                "WHERE kind = ? AND ref_value LIKE ?"
            )
            params.extend([field, pattern])
        elif field == _FIELD_FROM:
            sql = "SELECT message_id, list, date_unix_ns FROM over WHERE from_addr LIKE ?"
            params.append(pattern)
        else:
            return []

        if since_unix_ns is not None:
            sql += " AND date_unix_ns >= ?"
            params.append(since_unix_ns)
        if list_filter is not None:
            sql += " AND list = ?"
            params.append(list_filter)

        sql += " ORDER BY date_unix_ns DESC LIMIT ?"
        params.append(limit)

        index_rows = db.execute(sql, params).fetchall()
        if not index_rows:
            return []

        # Hydrate from the `over` table in one round-trip, keyed on
        # the (message_id, list) primary key. `placeholders` is a
        # string of literal `?` characters, not user data — every
        # actual mid is bound via parameter substitution below.
        mids = [r["message_id"] for r in index_rows]
        placeholders = ",".join("?" * len(mids))
        hyd_sql = (
            "SELECT message_id, list, from_addr, subject_normalized, "  # noqa: S608
            "date_unix_ns, has_patch, is_cover_letter, series_version, "
            "series_index, in_reply_to "
            f"FROM over WHERE message_id IN ({placeholders})"
        )
        hyd_rows = {
            (r["message_id"], r["list"]): dict(r) for r in db.execute(hyd_sql, mids).fetchall()
        }

        out: list[dict] = []
        for ir in index_rows:
            key = (ir["message_id"], ir["list"])
            full = hyd_rows.get(key)
            if full is not None:
                out.append(full)
        return out
    finally:
        db.close()
