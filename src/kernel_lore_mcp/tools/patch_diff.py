"""lore_patch_diff — diff two patch versions of the same series.

The "what changed between v2 and v3" workflow. Both inputs are
message-ids; the response carries both hits + a unified diff of the
patch payloads.
"""

from __future__ import annotations

import difflib
from typing import Annotated, Literal

from pydantic import Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import LoreError, invalid_argument, not_found
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import PatchDiffResponse
from kernel_lore_mcp.timeout import run_with_timeout
from kernel_lore_mcp.tools.message import _split_prose_patch

_CONCISE_DIFF_LINES = 120


async def _fetch_patch(reader, mid: str) -> tuple[dict, str]:
    row = await run_with_timeout(reader.fetch_message, mid)
    if row is None:
        raise not_found(what="message", message_id=mid)
    body = await run_with_timeout(reader.fetch_body, mid)
    if body is None:
        raise LoreError(
            "store_inconsistent",
            "metadata row present but compressed body missing.",
            echoed_input={"message_id": mid},
            retry_after_seconds=30,
        )
    try:
        text = body.decode("utf-8")
    except UnicodeDecodeError:
        text = body.decode("latin-1", errors="replace")
    _, patch = _split_prose_patch(text)
    if patch is None:
        raise LoreError(
            "not_a_patch",
            f"message_id {mid!r} carries no `diff --git` payload.",
            echoed_input={"message_id": mid},
        )
    return row, patch


async def lore_patch_diff(
    a: Annotated[str, Field(min_length=1, max_length=512, description="Older message-id.")],
    b: Annotated[str, Field(min_length=1, max_length=512, description="Newer message-id.")],
    response_format: Annotated[
        Literal["concise", "detailed"],
        Field(
            description=(
                f"'concise' (default) truncates the diff to {_CONCISE_DIFF_LINES} "
                "lines for a fast agent-budget-friendly overview; 'detailed' "
                "returns the full unified diff."
            ),
        ),
    ] = "concise",
) -> PatchDiffResponse:
    """Unified diff between two patch versions of the same series.

    Cost: moderate — expected p95 300 ms (two decompresses + difflib).
    """
    if a == b:
        raise invalid_argument(
            name="a",
            reason="a and b must be different message-ids",
            value=a,
            example='{"a": "m1@x", "b": "m2@x"}',
        )

    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    row_a, patch_a = await _fetch_patch(reader, a)
    row_b, patch_b = await _fetch_patch(reader, b)

    diff_lines = list(
        difflib.unified_diff(
            patch_a.splitlines(keepends=True),
            patch_b.splitlines(keepends=True),
            fromfile=f"a/{row_a['message_id']}",
            tofile=f"b/{row_b['message_id']}",
            n=3,
        )
    )
    total_lines = len(diff_lines)
    if response_format == "concise" and total_lines > _CONCISE_DIFF_LINES:
        head = "".join(diff_lines[:_CONCISE_DIFF_LINES])
        diff = (
            head + f"\n... {total_lines - _CONCISE_DIFF_LINES} more lines; "
            "rerun with response_format='detailed' for the full diff.\n"
        )
    else:
        diff = "".join(diff_lines)

    return PatchDiffResponse(
        a=row_to_search_hit(row_a, tier_provenance=["metadata"]),
        b=row_to_search_hit(row_b, tier_provenance=["metadata"]),
        diff=diff,
        freshness=build_freshness(reader),
    )
