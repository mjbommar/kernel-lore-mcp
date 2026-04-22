"""lore_activity — file/function activity over lore.

Returns one row per matching message. Tid-based grouping + trailer
rollup lands in Phase 2.5 once the tid computation pass is wired.
"""

from __future__ import annotations

from typing import Annotated, Literal

from pydantic import Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.cursor import decode_cursor, mint_cursor, query_hash
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_activity_row
from kernel_lore_mcp.models import ActivityResponse
from kernel_lore_mcp.time_bounds import TIME_BOUND_DESCRIPTION, resolve_time_bounds
from kernel_lore_mcp.timeout import run_with_timeout

_CONCISE_ROWS = 20


async def lore_activity(
    file: Annotated[
        str | None,
        Field(description="Exact path, e.g. `fs/smb/server/smbacl.c`."),
    ] = None,
    function: Annotated[
        str | None,
        Field(description="Exact identifier, e.g. `smb_check_perm_dacl`."),
    ] = None,
    since: Annotated[
        str | None,
        Field(description=f"Human-friendly lower bound. {TIME_BOUND_DESCRIPTION}"),
    ] = None,
    since_unix_ns: Annotated[
        int | None,
        Field(description="Lower bound on message date (nanoseconds since epoch UTC)."),
    ] = None,
    until: Annotated[
        str | None,
        Field(description=f"Human-friendly exclusive upper bound. {TIME_BOUND_DESCRIPTION}"),
    ] = None,
    until_unix_ns: Annotated[
        int | None,
        Field(description="Exclusive upper bound on message date (nanoseconds since epoch UTC)."),
    ] = None,
    list: Annotated[
        str | None,
        Field(description="Restrict to one mailing list (e.g. `linux-cifs`)."),
    ] = None,
    limit: Annotated[int, Field(ge=1, le=500)] = 100,
    cursor: Annotated[
        str | None,
        Field(
            description=(
                "Opaque HMAC-signed pagination token. Pass a "
                "`next_cursor` from a prior response to resume "
                "newest-first after the last returned row. Bound "
                "to the (file, function, since, list) combination "
                "— changing any invalidates the cursor."
            ),
        ),
    ] = None,
    response_format: Annotated[
        Literal["concise", "detailed"],
        Field(
            description=(
                "'concise' (default) caps the rows at 20 for a fast overview; "
                "'detailed' returns up to `limit`."
            ),
        ),
    ] = "concise",
) -> ActivityResponse:
    """Return recent activity touching the given file and/or function.

    Cost: cheap — expected p95 50 ms (metadata-tier column scan).
    """
    if not file and not function:
        from kernel_lore_mcp.errors import invalid_argument

        raise invalid_argument(
            name="file|function",
            reason="at least one of `file` or `function` is required",
            value={"file": file, "function": function},
            example='{"file": "fs/smb/server/smbacl.c"} or {"function": "smb_check_perm_dacl"}',
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

    # Cursor scope: (file, function, since, list) define the sorted
    # result set. `limit` / `response_format` can change between
    # pages without invalidating the cursor.
    q_hash = query_hash(
        "lore_activity",
        file or "",
        function or "",
        resolved_since or 0,
        resolved_until or 0,
        list or "",
    )
    resume = decode_cursor(cursor, expected_q_hash=q_hash, arg_name="cursor")

    # Oversample 2× for the cursor-skip headroom + one-row
    # look-ahead to detect "more pages exist."
    fetch_budget = max(limit * 2 + 1, 32)
    rows = await run_with_timeout(
        reader.activity,
        file,
        function,
        resolved_since,
        resolved_until,
        list,
        fetch_budget,
    )

    # Skip past the resume point (newest-first by date_unix_ns).
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
    effective_limit = _CONCISE_ROWS if response_format == "concise" else limit
    page = rows[:effective_limit]

    activity_rows = [row_to_activity_row(r) for r in page]
    default_applied: list[str] = []
    if response_format == "concise" and total_available > _CONCISE_ROWS:
        default_applied.append(f"response_format=concise (showing top {_CONCISE_ROWS})")

    next_cursor: str | None = None
    if page and total_available > effective_limit:
        last = page[-1]
        next_cursor = mint_cursor(
            q_hash=q_hash,
            last_score=float(last.get("date_unix_ns") or 0),
            last_mid=str(last.get("message_id") or ""),
        )

    return ActivityResponse(
        rows=activity_rows,
        total=total_available,
        default_applied=default_applied,
        next_cursor=next_cursor,
        freshness=build_freshness(reader),
    )
