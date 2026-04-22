"""lore_file_timeline — chronological activity on one kernel path.

Differs from `lore_activity` in three ways:

1. Oldest-first ordering (archaeology use case) — or newest-first
   when the caller wants recent changes.
2. Explicit window parameters (`since_unix_ns` / `until_unix_ns`)
   rather than "just recent".
3. A per-time-bucket histogram, so agents can see the shape of
   activity — a file that's churning NOW vs one that was hot five
   years ago but silent recently.

Use cases:
  - "When was this function first added?" → `order=asc, limit=5`.
  - "What's the patch-rate on this file?" → inspect `histogram`.
  - "Was this file rewritten recently?" → spike detection in recent
    buckets.
"""

from __future__ import annotations

from collections import defaultdict
from datetime import UTC, datetime
from typing import Annotated, Literal

from pydantic import BaseModel, Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import invalid_argument
from kernel_lore_mcp.freshness import Freshness, build_freshness
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import SearchHit
from kernel_lore_mcp.time_bounds import TIME_BOUND_DESCRIPTION, resolve_time_bounds
from kernel_lore_mcp.timeout import run_with_timeout


class TimelineBucket(BaseModel):
    """One histogram bucket (year or quarter, by granularity)."""

    label: str = Field(description="`2024-Q3` / `2024` / `2024-11` depending on bucket width.")
    patches: int
    unique_authors: int


class FileTimelineResponse(BaseModel):
    path_queried: str
    total_matching: int
    oldest_unix_ns: int | None = None
    newest_unix_ns: int | None = None
    oldest_utc: datetime | None = None
    newest_utc: datetime | None = None
    order: Literal["asc", "desc"]
    events: list[SearchHit]
    histogram: list[TimelineBucket]
    bucket: Literal["year", "quarter", "month"]
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


def _bucket_label(date_ns: int, bucket: str) -> str:
    dt = datetime.fromtimestamp(date_ns / 1_000_000_000, tz=UTC)
    if bucket == "year":
        return f"{dt.year:04d}"
    if bucket == "month":
        return f"{dt.year:04d}-{dt.month:02d}"
    # quarter
    q = (dt.month - 1) // 3 + 1
    return f"{dt.year:04d}-Q{q}"


async def lore_file_timeline(
    path: Annotated[
        str,
        Field(
            min_length=1,
            max_length=512,
            description="Kernel tree path, e.g. `fs/smb/server/smbacl.c`.",
        ),
    ],
    since: Annotated[
        str | None,
        Field(description=f"Human-friendly lower bound. {TIME_BOUND_DESCRIPTION}"),
    ] = None,
    since_unix_ns: Annotated[
        int | None,
        Field(description="Window lower bound (ns since epoch); None = beginning of corpus."),
    ] = None,
    until: Annotated[
        str | None,
        Field(description=f"Human-friendly exclusive upper bound. {TIME_BOUND_DESCRIPTION}"),
    ] = None,
    until_unix_ns: Annotated[
        int | None,
        Field(description="Window upper bound (ns since epoch); None = end of corpus."),
    ] = None,
    order: Annotated[
        Literal["asc", "desc"],
        Field(
            description=(
                "'asc' (oldest-first — archaeology) or 'desc' (newest-first — "
                "what changed recently). Default 'asc'."
            ),
        ),
    ] = "asc",
    bucket: Annotated[
        Literal["year", "quarter", "month"],
        Field(description="Histogram granularity."),
    ] = "quarter",
    limit: Annotated[
        int,
        Field(
            ge=1,
            le=2_000,
            description=(
                "Max events returned. Histogram always reflects the FULL "
                "sampled window, not just returned events."
            ),
        ),
    ] = 100,
    activity_sample: Annotated[
        int,
        Field(
            ge=100,
            le=50_000,
            description=(
                "Upper cap on messages sampled for the histogram. Most "
                "paths comfortably fit the default."
            ),
        ),
    ] = 5_000,
) -> FileTimelineResponse:
    """Chronological activity on one file with a per-bucket histogram.

    Cost: cheap — expected p95 200 ms. One activity scan + in-memory
    sort + bucketing.
    """
    if path.strip() != path or not path:
        raise invalid_argument(
            name="path",
            reason="must be a non-empty, non-whitespace-padded kernel tree path",
            value=path,
            example="fs/smb/server/smbacl.c",
        )
    if since_unix_ns is not None and until_unix_ns is not None and since_unix_ns >= until_unix_ns:
        raise invalid_argument(
            name="window",
            reason="since_unix_ns must be less than until_unix_ns",
            value={"since": since_unix_ns, "until": until_unix_ns},
            example="since=0, until=2_000_000_000_000_000_000",
        )

    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    resolved_since, resolved_until = resolve_time_bounds(
        since=since,
        since_unix_ns=since_unix_ns,
        until=until,
        until_unix_ns=until_unix_ns,
    )
    rows = await run_with_timeout(
        reader.activity,
        path,
        None,  # function
        resolved_since,
        resolved_until,
        None,  # list
        activity_sample,
    )

    # Sort per `order` on date_unix_ns; rows with no date sink.
    rows_with_date = [r for r in rows if r.get("date_unix_ns") is not None]
    rows_no_date = [r for r in rows if r.get("date_unix_ns") is None]
    rows_with_date.sort(key=lambda r: r["date_unix_ns"], reverse=(order == "desc"))
    ordered = rows_with_date + rows_no_date

    total_matching = len(ordered)
    oldest_ns = min((r["date_unix_ns"] for r in rows_with_date), default=None)
    newest_ns = max((r["date_unix_ns"] for r in rows_with_date), default=None)

    # Histogram over the FULL sampled window (not just truncated events).
    bucket_patches: dict[str, int] = defaultdict(int)
    bucket_authors: dict[str, set[str]] = defaultdict(set)
    for r in rows_with_date:
        label = _bucket_label(r["date_unix_ns"], bucket)
        bucket_patches[label] += 1
        addr = r.get("from_addr")
        if addr:
            bucket_authors[label].add(addr)
    histogram = [
        TimelineBucket(
            label=label,
            patches=bucket_patches[label],
            unique_authors=len(bucket_authors[label]),
        )
        for label in sorted(bucket_patches.keys())
    ]

    # Truncate events AFTER histogram (so histogram reflects full window).
    events_rows = ordered[:limit]
    events = [row_to_search_hit(r, tier_provenance=["metadata"]) for r in events_rows]

    def _utc_of(ns: int | None) -> datetime | None:
        if ns is None:
            return None
        return datetime.fromtimestamp(ns / 1_000_000_000, tz=UTC)

    return FileTimelineResponse(
        path_queried=path,
        total_matching=total_matching,
        oldest_unix_ns=oldest_ns,
        newest_unix_ns=newest_ns,
        oldest_utc=_utc_of(oldest_ns),
        newest_utc=_utc_of(newest_ns),
        order=order,
        events=events,
        histogram=histogram,
        bucket=bucket,
        freshness=build_freshness(reader),
    )
