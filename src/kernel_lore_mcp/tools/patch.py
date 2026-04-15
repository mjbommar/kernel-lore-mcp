"""lore_patch — fetch raw patch text for one message-id.

For browsing prose+patch together, use lore_message; for cross-version
diffing use lore_patch_diff.
"""

from __future__ import annotations

import asyncio
from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.errors import LoreError, not_found
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import PatchResponse
from kernel_lore_mcp.tools.message import _split_prose_patch


async def lore_patch(
    message_id: Annotated[str, Field(min_length=1, max_length=512)],
) -> PatchResponse:
    """Return the raw patch payload for one message-id.

    Cost: cheap — expected p95 80 ms (point-lookup + body decompress).
    """
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    row = await asyncio.to_thread(reader.fetch_message, message_id)
    if row is None:
        raise not_found(what="message", message_id=message_id)
    if not row.get("has_patch"):
        raise LoreError(
            "not_a_patch",
            f"message_id {message_id!r} carries no `diff --git` payload.",
            echoed_input={"message_id": message_id},
            valid_example="use lore_message for prose-only messages; lore_patch only accepts patches.",
        )

    body = await asyncio.to_thread(reader.fetch_body, message_id)
    if body is None:
        raise LoreError(
            "store_inconsistent",
            "metadata row present but compressed body missing — index and store are out of sync.",
            echoed_input={"message_id": message_id},
            retry_after_seconds=30,
        )
    try:
        body_text = body.decode("utf-8")
    except UnicodeDecodeError:
        body_text = body.decode("latin-1", errors="replace")
    _, patch = _split_prose_patch(body_text)
    if patch is None:
        raise LoreError(
            "patch_payload_lost",
            "ingest flagged has_patch=true, but the diff payload could not be re-extracted.",
            echoed_input={"message_id": message_id},
            retry_after_seconds=60,
        )

    return PatchResponse(
        hit=row_to_search_hit(row, tier_provenance=["metadata"]),
        patch=patch,
        body_sha256=row["body_sha256"],
        freshness=build_freshness(reader),
    )
