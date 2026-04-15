"""lore_activity — file/function activity over lore.

Returns one row per matching message. Tid-based grouping + trailer
rollup lands in Phase 2.5 once the tid computation pass is wired.
"""

from __future__ import annotations

from typing import Annotated, Literal

from pydantic import Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_activity_row
from kernel_lore_mcp.models import ActivityResponse
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
    since_unix_ns: Annotated[
        int | None,
        Field(description="Lower bound on message date (nanoseconds since epoch UTC)."),
    ] = None,
    list: Annotated[
        str | None,
        Field(description="Restrict to one mailing list (e.g. `linux-cifs`)."),
    ] = None,
    limit: Annotated[int, Field(ge=1, le=500)] = 100,
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
    rows = await run_with_timeout(
        reader.activity,
        file,
        function,
        since_unix_ns,
        list,
        limit,
    )
    activity_rows = [row_to_activity_row(r) for r in rows]
    total = len(activity_rows)
    default_applied: list[str] = []
    if response_format == "concise" and total > _CONCISE_ROWS:
        activity_rows = activity_rows[:_CONCISE_ROWS]
        default_applied.append(f"response_format=concise (showing top {_CONCISE_ROWS})")
    return ActivityResponse(
        rows=activity_rows,
        total=total,
        default_applied=default_applied,
        freshness=build_freshness(reader),
    )
