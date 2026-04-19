"""lore_subsystem_churn — where is the bit rot?

Aggregates patch activity for a mailing list or path prefix over a
window and returns the hottest files (by patch count + author
diversity), plus a time-bucketed histogram. Answers "what's
churning in netdev this quarter?" / "which files are seeing
regression fixes?" in one call.

Differs from `lore_activity` (which returns rows) by:
  1. File-ranking aggregation — top-N files with patch counts,
     not just a flat row list.
  2. Author-diversity tracking — a file getting 50 patches from
     2 people looks different from 50 patches from 30 people.
  3. Time-bucket histogram (month / quarter / year) so the agent
     can see "recent spike" vs "long-steady-burn" vs "old-and-cold".

Scope:
  - `scope="list:<name>"`  — aggregate over everything on that list.
  - `scope="path:<prefix>"` — aggregate over patches touching files
    under that prefix (any list).
"""

from __future__ import annotations

from collections import defaultdict
from datetime import UTC, datetime, timedelta
from typing import Annotated, Literal

from pydantic import BaseModel, Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import invalid_argument
from kernel_lore_mcp.freshness import Freshness, build_freshness
from kernel_lore_mcp.timeout import run_with_timeout


class HotFile(BaseModel):
    path: str
    patches: int
    unique_authors: int
    first_seen_unix_ns: int | None = None
    last_seen_unix_ns: int | None = None
    top_authors: list[str] = Field(default_factory=list)


class ChurnBucket(BaseModel):
    label: str
    patches: int
    unique_authors: int


class SubsystemChurnResponse(BaseModel):
    scope: str
    window_days: int
    sampled_patches: int
    total_files_touched: int
    top_files: list[HotFile]
    histogram: list[ChurnBucket]
    bucket: Literal["month", "quarter", "year"]
    caveat: str = Field(
        description=(
            "Merge-rate (what fraction of these patches actually "
            "landed) requires a git-tree sidecar. Pure-lore "
            "aggregation here counts PATCH MAIL, not merged commits."
        )
    )
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


def _bucket_label(date_ns: int, bucket: str) -> str:
    dt = datetime.fromtimestamp(date_ns / 1_000_000_000, tz=UTC)
    if bucket == "year":
        return f"{dt.year:04d}"
    if bucket == "month":
        return f"{dt.year:04d}-{dt.month:02d}"
    q = (dt.month - 1) // 3 + 1
    return f"{dt.year:04d}-Q{q}"


async def lore_subsystem_churn(
    scope: Annotated[
        str,
        Field(
            min_length=5,
            max_length=256,
            description=(
                "Either `list:<name>` (e.g. `list:netdev`) or "
                "`path:<prefix>` (e.g. `path:fs/btrfs/`)."
            ),
        ),
    ],
    window_days: Annotated[
        int,
        Field(
            ge=1,
            le=3650,
            description="Look-back window; default 90 days.",
        ),
    ] = 90,
    bucket: Annotated[
        Literal["month", "quarter", "year"],
        Field(description="Histogram granularity."),
    ] = "month",
    top_n: Annotated[
        int,
        Field(ge=1, le=100, description="How many hottest files to return."),
    ] = 20,
    sample_limit: Annotated[
        int,
        Field(
            ge=100,
            le=50_000,
            description=(
                "Max patches sampled from the window. Large subsystems "
                "(lkml, netdev) can easily hit this cap — check "
                "`sampled_patches` in the response."
            ),
        ),
    ] = 10_000,
) -> SubsystemChurnResponse:
    """Top-N hottest files in a subsystem + time-bucketed histogram.

    Cost: moderate — expected p95 500 ms. Proportional to
    `sample_limit`.
    """
    if ":" not in scope:
        raise invalid_argument(
            name="scope",
            reason="must be `list:<name>` or `path:<prefix>`",
            value=scope,
            example="list:netdev",
        )
    kind, _, value = scope.partition(":")
    if kind not in {"list", "path"}:
        raise invalid_argument(
            name="scope",
            reason="kind must be `list` or `path`",
            value=scope,
            example="list:netdev",
        )
    if not value:
        raise invalid_argument(
            name="scope",
            reason="value after the colon must be non-empty",
            value=scope,
            example="list:netdev",
        )

    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)

    since_ns = int(
        (datetime.now(tz=UTC) - timedelta(days=window_days)).timestamp()
        * 1_000_000_000
    )

    if kind == "list":
        rows = await run_with_timeout(
            reader.eq,
            "list",
            value,
            since_ns,
            None,
            sample_limit,
        )
    else:
        rows = await run_with_timeout(
            reader.activity,
            value,
            None,
            since_ns,
            None,
            sample_limit,
        )

    # Aggregate file → patches + authors + dates.
    files_patches: dict[str, int] = defaultdict(int)
    files_authors: dict[str, set[str]] = defaultdict(set)
    files_first: dict[str, int] = {}
    files_last: dict[str, int] = {}
    bucket_patches: dict[str, int] = defaultdict(int)
    bucket_authors: dict[str, set[str]] = defaultdict(set)

    for r in rows:
        date_ns = r.get("date_unix_ns")
        addr = r.get("from_addr")
        touched = r.get("touched_files") or []
        if kind == "path":
            # path: scope — already filtered by activity(), but any
            # row can touch multiple files; count only files UNDER
            # the prefix.
            touched = [p for p in touched if p.startswith(value)]
        for path in touched:
            files_patches[path] += 1
            if addr:
                files_authors[path].add(addr)
            if date_ns is not None:
                files_first[path] = (
                    min(files_first[path], date_ns) if path in files_first else date_ns
                )
                files_last[path] = (
                    max(files_last[path], date_ns) if path in files_last else date_ns
                )
        if date_ns is not None:
            label = _bucket_label(date_ns, bucket)
            bucket_patches[label] += 1
            if addr:
                bucket_authors[label].add(addr)

    # Rank files by patches desc, break ties by author diversity.
    ranked = sorted(
        files_patches.items(),
        key=lambda p: (-p[1], -len(files_authors.get(p[0], set())), p[0]),
    )[:top_n]

    top_files = []
    for path, patches in ranked:
        authors = sorted(files_authors.get(path, set()))
        top_files.append(
            HotFile(
                path=path,
                patches=patches,
                unique_authors=len(authors),
                first_seen_unix_ns=files_first.get(path),
                last_seen_unix_ns=files_last.get(path),
                top_authors=authors[:5],
            )
        )

    histogram = [
        ChurnBucket(
            label=label,
            patches=bucket_patches[label],
            unique_authors=len(bucket_authors[label]),
        )
        for label in sorted(bucket_patches.keys())
    ]

    return SubsystemChurnResponse(
        scope=scope,
        window_days=window_days,
        sampled_patches=len(rows),
        total_files_touched=len(files_patches),
        top_files=top_files,
        histogram=histogram,
        bucket=bucket,
        caveat=(
            "lore-only — these are patch-mail counts, not merged "
            "commit counts. Merge rate requires a linux.git sidecar "
            "(backlog #40)."
        ),
        freshness=build_freshness(reader),
    )
