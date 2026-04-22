"""lore_author_profile — aggregate view of one from_addr's activity.

Answers: who is this person? Which subsystems do they work in? How
many patches have they sent? How many have been reviewed / acked /
tested? What's their fix-rate? When were they last active?

Sampled-window semantics: the over.db `from_addr` index is fast, but
for a prolific author (gregkh@linuxfoundation.org has ~500 k messages)
we don't want to aggregate over the whole history on every call. The
tool defaults to the most recent 10 000 messages, newest-first. The
response's `limit_hit` flag tells the caller whether they're seeing
a truncated window.

Intentional scope: this reports on messages this address AUTHORED.
"How many patches has this person REVIEWED" (appearing as another
author's Reviewed-by) is a separate query shape; `over.db` has no
reverse-trailer index today. Tracked as future work in the backlog.
"""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import invalid_argument
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.models import (
    AuthorProfileResponse,
    OwnTrailerStats,
    ReceivedTrailerStats,
    SubsystemBucket,
)
from kernel_lore_mcp.time_bounds import TIME_BOUND_DESCRIPTION, resolve_time_bounds
from kernel_lore_mcp.timeout import run_with_timeout


def _utc_of(ns: int | None) -> datetime | None:
    if ns is None:
        return None
    return datetime.fromtimestamp(ns / 1_000_000_000, tz=UTC)


async def lore_author_profile(
    addr: Annotated[
        str,
        Field(
            min_length=3,
            max_length=254,
            description=(
                "Author email address (case-insensitive; the over.db "
                "index is lowercased at ingest). Example: "
                "`gregkh@linuxfoundation.org`."
            ),
        ),
    ],
    list_filter: Annotated[
        str | None,
        Field(
            description=(
                "Optional: restrict to one mailing list slug. Useful "
                "for subsystem-specific profiles."
            ),
        ),
    ] = None,
    since: Annotated[
        str | None,
        Field(description=f"Human-friendly lower bound. {TIME_BOUND_DESCRIPTION}"),
    ] = None,
    since_unix_ns: Annotated[
        int | None,
        Field(description="Lower-bound on message date (ns since epoch)."),
    ] = None,
    until: Annotated[
        str | None,
        Field(description=f"Human-friendly exclusive upper bound. {TIME_BOUND_DESCRIPTION}"),
    ] = None,
    until_unix_ns: Annotated[
        int | None,
        Field(description="Exclusive upper-bound on message date (ns since epoch)."),
    ] = None,
    limit: Annotated[
        int,
        Field(
            ge=1,
            le=50_000,
            description=(
                "Sample ceiling — how many most-recent messages to "
                "aggregate. Default 10 000 is enough for most authors; "
                "crank higher for truly prolific addresses (watch the "
                "`limit_hit` flag in the response)."
            ),
        ),
    ] = 10_000,
    include_mentions: Annotated[
        bool,
        Field(
            description=(
                "When True, also aggregate messages where the address "
                "appears in any trailer (signed_off_by / reviewed_by / "
                "acked_by / tested_by / co_developed_by / reported_by / "
                "suggested_by / helped_by / assisted_by) on someone "
                "else's patch. Matches what a full-text lore search "
                "would show. Costs one extra Parquet scan — slower, "
                "bounded by `mention_limit`. REQUIRES a narrowing "
                "filter (`list_filter` or `since_unix_ns`) on large "
                "deployments — an unfiltered full-corpus mention scan "
                "is the shape anonymous users shouldn't be able to "
                "trigger on demand."
            ),
        ),
    ] = False,
    mention_limit: Annotated[
        int,
        Field(
            ge=100,
            le=500,
            description=(
                "Upper cap on mention-scope rows. Ignored when "
                "`include_mentions=False`. Ceiling is deliberately "
                "tight (500) so a single call can't saturate a "
                "worker under anonymous multi-tenant load; raise "
                "`limit` for prolific authors instead."
            ),
        ),
    ] = 500,
) -> AuthorProfileResponse:
    """Aggregate profile for messages authored by `addr`.

    Cost: cheap — expected p95 100 ms for typical addresses (one
    indexed over.db scan + in-memory aggregation). Scales with
    `limit` × row decode cost, so 50 000 on a prolific author is
    closer to 500 ms.
    """
    if "@" not in addr:
        raise invalid_argument(
            name="addr",
            reason="must be an email address",
            value=addr,
            example="gregkh@linuxfoundation.org",
        )
    resolved_since, resolved_until = resolve_time_bounds(
        since=since,
        since_unix_ns=since_unix_ns,
        until=until,
        until_unix_ns=until_unix_ns,
    )
    if include_mentions and list_filter is None and resolved_since is None:
        raise invalid_argument(
            name="include_mentions",
            reason=(
                "unbounded mention scan is not allowed on this server; "
                "pass either `list_filter` (e.g. list_filter='linux-cifs') "
                "or `since` / `since_unix_ns` (a date lower bound) to narrow"
            ),
            value={
                "include_mentions": True,
                "list_filter": None,
                "since": since,
                "since_unix_ns": since_unix_ns,
            },
            example=('{"include_mentions": true, "list_filter": "linux-cifs"}'),
        )

    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    raw = await run_with_timeout(
        reader.author_profile,
        addr,
        list_filter,
        resolved_since,
        resolved_until,
        limit,
        include_mentions,
        mention_limit,
        echoed_input={"addr": addr},
    )

    subsystems = [
        SubsystemBucket(
            list=s["list"],
            patches=s["patches"],
            oldest_unix_ns=s.get("oldest_unix_ns"),
            newest_unix_ns=s.get("newest_unix_ns"),
            oldest_utc=_utc_of(s.get("oldest_unix_ns")),
            newest_utc=_utc_of(s.get("newest_unix_ns")),
        )
        for s in raw.get("subsystems") or []
    ]

    return AuthorProfileResponse(
        addr_queried=raw["addr_queried"],
        sampled=raw["sampled"],
        authored_count=raw.get("authored_count", raw["sampled"]),
        mention_count=raw.get("mention_count", 0),
        limit_hit=raw["limit_hit"],
        oldest_unix_ns=raw.get("oldest_unix_ns"),
        newest_unix_ns=raw.get("newest_unix_ns"),
        oldest_utc=_utc_of(raw.get("oldest_unix_ns")),
        newest_utc=_utc_of(raw.get("newest_unix_ns")),
        patches_with_content=raw["patches_with_content"],
        cover_letters=raw["cover_letters"],
        unique_subjects=raw["unique_subjects"],
        with_fixes_trailer=raw["with_fixes_trailer"],
        own_trailers=OwnTrailerStats(
            signed_off_by_present=raw["own_trailers"]["signed_off_by_present"],
            fixes_issued=raw["own_trailers"]["fixes_issued"],
        ),
        received_trailers=ReceivedTrailerStats(
            reviewed_by=raw["received_trailers"]["reviewed_by"],
            acked_by=raw["received_trailers"]["acked_by"],
            tested_by=raw["received_trailers"]["tested_by"],
            co_developed_by=raw["received_trailers"]["co_developed_by"],
            reported_by=raw["received_trailers"]["reported_by"],
            cc_stable=raw["received_trailers"]["cc_stable"],
        ),
        subsystems=subsystems,
        freshness=build_freshness(reader),
    )
