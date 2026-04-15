"""Row-dict → pydantic model mapping.

`_core.Reader` returns plain dicts keyed by column name. This module
is the single place that reshapes them into `SearchHit` /
`ActivityRow` / etc. so the rest of the codebase imports models, not
raw column-name strings.
"""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Any

from kernel_lore_mcp.models import (
    ActivityRow,
    PatchStats,
    SearchHit,
    SeriesTimelineEntry,
    Snippet,
)

LORE_URL_PREFIX = "https://lore.kernel.org"


def _date_from_ns(ns: int | None) -> datetime | None:
    if ns is None:
        return None
    return datetime.fromtimestamp(ns / 1_000_000_000, tz=UTC)


def _series_index_str(row: dict[str, Any]) -> str | None:
    idx, total = row.get("series_index"), row.get("series_total")
    if idx is None or total is None:
        return None
    return f"{idx}/{total}"


def cite_key(row: dict[str, Any]) -> str:
    """Stable short handle for citation, e.g. `linux-cifs/2026-04/m1-x`.

    Chosen so an agent can re-identify a hit from a human-legible string
    if it loses the message-id. Not a primary key — always pair with
    `message_id` in the response.
    """
    ymd = "unknown"
    if (d := _date_from_ns(row.get("date_unix_ns"))) is not None:
        ymd = d.strftime("%Y-%m")
    mid_slug = row["message_id"].replace("@", "-").replace("/", "-")
    return f"{row['list']}/{ymd}/{mid_slug}"


def lore_url(row: dict[str, Any]) -> str:
    return f"{LORE_URL_PREFIX}/{row['list']}/{row['message_id']}/"


def _patch_stats(row: dict[str, Any]) -> PatchStats | None:
    if not row.get("has_patch"):
        return None
    fc = row.get("files_changed")
    ins = row.get("insertions")
    dels = row.get("deletions")
    if fc is None or ins is None or dels is None:
        return None
    return PatchStats(files_changed=fc, insertions=ins, deletions=dels)


def row_to_search_hit(
    row: dict[str, Any],
    *,
    tier_provenance: list[str],
    is_exact_match: bool = True,
    snippet: Snippet | None = None,
) -> SearchHit:
    """The canonical row→hit mapping. `tier_provenance` says which tier
    produced the hit; for v0.5 metadata-only queries it's
    `["metadata"]`. v1+ may set `["metadata", "bm25"]` etc. when
    merged results arrive from multiple tiers.

    `snippet` is passed through unchanged when the caller has a real
    needle + source text to build KWIC context from; otherwise stays
    `None` rather than fabricating an offset.
    """
    subject = row.get("subject_normalized") or row.get("subject_raw") or ""
    return SearchHit(
        message_id=row["message_id"],
        cite_key=cite_key(row),
        list=row["list"],
        cross_posted_to=[],
        from_addr=row.get("from_addr"),
        from_name=row.get("from_name"),
        subject=subject,
        subject_tags=list(row.get("subject_tags") or []),
        date=_date_from_ns(row.get("date_unix_ns")) or datetime.fromtimestamp(0, tz=UTC),
        has_patch=bool(row.get("has_patch")),
        is_cover_letter=bool(row.get("is_cover_letter")),
        series_version=row.get("series_version") or None,
        series_index=_series_index_str(row),
        patch_stats=_patch_stats(row),
        snippet=snippet,
        score=None,
        tier_provenance=tier_provenance,
        is_exact_match=is_exact_match,
        lore_url=lore_url(row),
    )


def row_to_activity_row(row: dict[str, Any]) -> ActivityRow:
    subject = row.get("subject_normalized") or row.get("subject_raw") or ""
    return ActivityRow(
        message_id=row["message_id"],
        cite_key=cite_key(row),
        list=row["list"],
        from_addr=row.get("from_addr"),
        from_name=row.get("from_name"),
        subject=subject,
        subject_tags=list(row.get("subject_tags") or []),
        date=_date_from_ns(row.get("date_unix_ns")),
        has_patch=bool(row.get("has_patch")),
        is_cover_letter=bool(row.get("is_cover_letter")),
        series_version=row.get("series_version") or None,
        series_index=_series_index_str(row),
        patch_stats=_patch_stats(row),
        reviewed_by=list(row.get("reviewed_by") or []),
        acked_by=list(row.get("acked_by") or []),
        tested_by=list(row.get("tested_by") or []),
        signed_off_by=list(row.get("signed_off_by") or []),
        fixes=list(row.get("fixes") or []),
        cc_stable=list(row.get("cc_stable") or []),
        lore_url=lore_url(row),
    )


def row_to_timeline_entry(row: dict[str, Any]) -> SeriesTimelineEntry:
    subject = row.get("subject_normalized") or row.get("subject_raw") or ""
    return SeriesTimelineEntry(
        message_id=row["message_id"],
        cite_key=cite_key(row),
        subject=subject,
        series_version=row.get("series_version") or None,
        series_index=_series_index_str(row),
        date=_date_from_ns(row.get("date_unix_ns")),
        reviewed_by=list(row.get("reviewed_by") or []),
        acked_by=list(row.get("acked_by") or []),
        lore_url=lore_url(row),
    )
