"""lore_maintainer_profile — declared (MAINTAINERS) vs. observed activity.

Answers the single most common lore-adjacent question: "who should I
CC on a patch to THIS file?" — plus the corollaries "are the listed
maintainers still actually active?" and "is there a prolific reviewer
who isn't in MAINTAINERS yet?".

Data shape:
  - Declared = sections of the kernel's MAINTAINERS file whose F:/N:
    pattern matches the queried path (minus any X: exclusions). Sorted
    by depth (most specific first), just like get_maintainer.pl.
  - Observed = addresses that appear in Reviewed-by / Acked-by /
    Tested-by / Signed-off-by trailers on patches touching the path
    within a recent window.
  - Stale-declared = in MAINTAINERS, but silent in the window.
  - Active-unlisted = loud in the window, but not in MAINTAINERS.

The server needs a MAINTAINERS snapshot to answer the declared half.
Set `KLMCP_MAINTAINERS_FILE` or drop the file at `<data_dir>/MAINTAINERS`.
Without it, `maintainers_available` is False and only observed activity
is reported.
"""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import invalid_argument
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.models import (
    DeclaredMaintainerEntry,
    MaintainerProfileResponse,
    ObservedAddr,
)
from kernel_lore_mcp.timeout import run_with_timeout


def _utc_of(ns: int | None) -> datetime | None:
    if ns is None:
        return None
    return datetime.fromtimestamp(ns / 1_000_000_000, tz=UTC)


async def lore_maintainer_profile(
    path: Annotated[
        str,
        Field(
            min_length=1,
            max_length=512,
            description=(
                "Kernel tree path, e.g. `fs/smb/server/smbacl.c` or "
                "`net/core/sock.c`. Matched against MAINTAINERS F:/N: "
                "patterns with the same algorithm as get_maintainer.pl."
            ),
        ),
    ],
    window_days: Annotated[
        int,
        Field(
            ge=1,
            le=3650,
            description=(
                "How far back to look for observed activity. Default "
                "180 days balances recency with enough signal for "
                "low-traffic subsystems."
            ),
        ),
    ] = 180,
    activity_limit: Annotated[
        int,
        Field(
            ge=100,
            le=50_000,
            description=(
                "Cap on messages sampled when aggregating observed "
                "trailer activity. Most paths fit well under the default."
            ),
        ),
    ] = 5_000,
) -> MaintainerProfileResponse:
    """Declared vs. observed ownership of a kernel path.

    Cost: cheap — expected p95 150 ms (one activity scan + in-memory
    trailer aggregation). Proportional to `activity_limit` and the
    density of patches touching the path.
    """
    if not path or path.strip() != path:
        raise invalid_argument(
            name="path",
            reason="must be a non-empty, non-whitespace-padded kernel tree path",
            value=path,
            example="fs/smb/server/smbacl.c",
        )

    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    raw = await run_with_timeout(
        reader.maintainer_profile,
        path,
        window_days,
        activity_limit,
        echoed_input={"path": path},
    )

    declared = [
        DeclaredMaintainerEntry(
            name=e["name"],
            status=e.get("status"),
            depth=e["depth"],
            lists=list(e.get("lists") or []),
            maintainers=list(e.get("maintainers") or []),
            reviewers=list(e.get("reviewers") or []),
        )
        for e in (raw.get("declared") or [])
    ]

    def _obs(o: dict) -> ObservedAddr:
        return ObservedAddr(
            email=o["email"],
            reviewed_by=o.get("reviewed_by", 0),
            acked_by=o.get("acked_by", 0),
            tested_by=o.get("tested_by", 0),
            signed_off_by=o.get("signed_off_by", 0),
            last_seen_unix_ns=o.get("last_seen_unix_ns"),
            last_seen_utc=_utc_of(o.get("last_seen_unix_ns")),
        )

    return MaintainerProfileResponse(
        path_queried=raw["path_queried"],
        maintainers_available=raw["maintainers_available"],
        sampled_patches=raw["sampled_patches"],
        declared=declared,
        observed=[_obs(o) for o in (raw.get("observed") or [])],
        stale_declared=list(raw.get("stale_declared") or []),
        active_unlisted=[_obs(o) for o in (raw.get("active_unlisted") or [])],
        freshness=build_freshness(reader),
    )
