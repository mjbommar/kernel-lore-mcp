"""lore_message — fetch one message by id + its prose/patch split."""

from __future__ import annotations

from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.errors import LoreError, not_found
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import MessageResponse
from kernel_lore_mcp.timeout import run_with_timeout


def _split_prose_patch(body: str) -> tuple[str | None, str | None]:
    marker = "\ndiff --git "
    idx = body.find(marker)
    if idx < 0:
        return (body or None), None
    return (body[: idx + 1] or None), (body[idx + 1 :] or None)


async def lore_message(
    message_id: Annotated[str, Field(min_length=1, max_length=512)],
) -> MessageResponse:
    """Fetch a single message + its prose/patch split + raw body bytes.

    Cost: cheap — expected p95 50 ms (metadata point-lookup + one body fetch).
    """
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    row = await run_with_timeout(reader.fetch_message, message_id)
    if row is None:
        raise not_found(what="message", message_id=message_id)

    body = await run_with_timeout(reader.fetch_body, message_id)
    if body is None:
        raise LoreError(
            "store_inconsistent",
            "metadata row present but compressed body missing — index and store are out of sync.",
            echoed_input={"message_id": message_id},
            retry_after_seconds=30,
        )

    # mail-parser on the Rust side decodes to UTF-8 for us; best-effort
    # decode here for the body text. ASCII is the common case.
    try:
        body_text = body.decode("utf-8")
    except UnicodeDecodeError:
        body_text = body.decode("latin-1", errors="replace")

    prose, patch = _split_prose_patch(body_text)

    return MessageResponse(
        hit=row_to_search_hit(row, tier_provenance=["metadata"]),
        prose=prose,
        patch=patch,
        body_sha256=row["body_sha256"],
        body_length=row["body_length"],
        freshness=build_freshness(reader),
    )
