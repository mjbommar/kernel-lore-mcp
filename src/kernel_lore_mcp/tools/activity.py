"""lore_activity — file/function activity over lore.

Returns one row per matching message. Tid-based grouping + trailer
rollup lands in Phase 2.5 once the tid computation pass is wired.
"""

from __future__ import annotations

import asyncio
from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.mapping import row_to_activity_row
from kernel_lore_mcp.models import ActivityResponse, Freshness


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
) -> ActivityResponse:
    """Return recent activity touching the given file and/or function."""
    if not file and not function:
        # FastMCP maps Exception to a tool error; keep the message
        # actionable per docs/standards/python/design/errors.md.
        from fastmcp.exceptions import ToolError

        raise ToolError("lore_activity requires at least one of `file` or `function`.")

    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    rows = await asyncio.to_thread(
        reader.activity,
        file,
        function,
        since_unix_ns,
        list,
        limit,
    )
    activity_rows = [row_to_activity_row(r) for r in rows]
    return ActivityResponse(
        rows=activity_rows,
        total=len(activity_rows),
        default_applied=[],
        freshness=Freshness(),
    )
