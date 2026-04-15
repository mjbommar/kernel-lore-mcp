"""Phase 10 — RFC-6570 templated resources.

Every resource here maps a stable URI shape to body text (or a
stub) so agents can pin a reference without burning a tool call.
FastMCP 3.x derives the RFC-6570 template automatically from the
URI string + function signature.

URIs we register:

  lore://message/{mid}         — raw mbox body (MIME text/plain)
  lore://thread/{mid}          — concatenated bodies for a thread
                                 (MIME text/plain)
  lore://patch/{mid}           — patch payload only (MIME text/x-diff)
  lore://maintainer/{path}     — MAINTAINERS block for a file path
                                 (MIME text/plain; stub until Phase 18A)
  lore://patchwork/{msg_id}    — patchwork state blob for a message
                                 (MIME application/json; stub until
                                 Phase 19A)

Error shape: we raise `ResourceError` on "not found" rather than
`NotFoundError` because FastMCP's default `mask_error_details=True`
rewrites NotFoundError messages into a generic wrapper that hides
the message-id from the caller. See `docs/research/` task #47.
"""

from __future__ import annotations

import asyncio
import json
from typing import TYPE_CHECKING

from fastmcp.exceptions import ResourceError
from fastmcp.resources import ResourceContent, ResourceResult

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.tools.message import _split_prose_patch

if TYPE_CHECKING:
    from fastmcp import FastMCP


def _decode(body: bytes) -> str:
    try:
        return body.decode("utf-8")
    except UnicodeDecodeError:
        return body.decode("latin-1", errors="replace")


async def _fetch_message_body(mid: str) -> str:
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    body = await asyncio.to_thread(reader.fetch_body, mid)
    if body is None:
        raise ResourceError(f"lore://message/{mid} — message-id not found in indexed corpus")
    return _decode(body)


async def _fetch_thread_text(mid: str) -> str:
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    rows = await asyncio.to_thread(reader.thread, mid, 500)
    if not rows:
        raise ResourceError(f"lore://thread/{mid} — no messages under that thread seed")

    chunks: list[str] = []
    for row in rows:
        body = await asyncio.to_thread(reader.fetch_body, row["message_id"])
        if body is None:
            continue
        chunks.append(f"=== {row['message_id']} ===\n{_decode(body)}")
    return "\n\n".join(chunks)


async def _fetch_patch_text(mid: str) -> str:
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    row = await asyncio.to_thread(reader.fetch_message, mid)
    if row is None:
        raise ResourceError(f"lore://patch/{mid} — message-id not found")
    if not row.get("has_patch"):
        raise ResourceError(f"lore://patch/{mid} — message carries no diff payload")
    body = await asyncio.to_thread(reader.fetch_body, mid)
    if body is None:
        raise ResourceError(
            f"lore://patch/{mid} — metadata row present but body missing from store"
        )
    _, patch = _split_prose_patch(_decode(body))
    if patch is None:
        raise ResourceError(
            f"lore://patch/{mid} — has_patch=true at ingest but diff payload could not be re-extracted"
        )
    return patch


_MAINTAINER_STUB = (
    "Not yet implemented. The MAINTAINERS parser lands in Phase 18A of "
    "docs/plans/2026-04-14-best-in-class-kernel-mcp.md. Once shipped, "
    "this resource returns the matching M:/R:/L:/S:/T:/F:/K:/N:/X: block "
    "for the requested source-tree path, plus the last 90 days of "
    "Reviewed-by traffic from the corresponding list.\n"
)

_PATCHWORK_STUB_JSON = json.dumps(
    {
        "status": "not_yet_implemented",
        "phase": "19A",
        "plan_reference": "docs/plans/2026-04-14-best-in-class-kernel-mcp.md",
        "description": (
            "patchwork.kernel.org state lookup is scheduled for Phase 19A. "
            "Once shipped, this resource returns "
            "{state, delegate, series_id, checks[]} for the given Message-ID."
        ),
    },
    indent=2,
)


def register_templated_resources(mcp: FastMCP) -> None:
    """Wire the 5 Phase-10 templated resources onto `mcp`.

    Kept in a single function so `build_server()` stays scannable
    and so tests can exercise the registration in isolation.
    """

    @mcp.resource(
        "lore://message/{mid}",
        name="lore_message",
        description=(
            "Raw mbox body for one Message-ID. Use this when you already "
            "have an mid and want the text without an extra tool round-trip."
        ),
        mime_type="text/plain",
    )
    async def _message(mid: str) -> str:
        return await _fetch_message_body(mid)

    @mcp.resource(
        "lore://thread/{mid}",
        name="lore_thread_text",
        description=(
            "Concatenated raw bodies of every message in a thread, seeded "
            "by any message-id within it (not a thread-id — the walker "
            "discovers the full thread from any message). For structured "
            "thread metadata, use the `lore_thread` tool instead."
        ),
        mime_type="text/plain",
    )
    async def _thread(mid: str) -> str:
        return await _fetch_thread_text(mid)

    @mcp.resource(
        "lore://patch/{mid}",
        name="lore_patch_text",
        description="Patch payload (diff --git onward) for a message carrying a patch.",
        mime_type="text/x-diff",
    )
    async def _patch(mid: str) -> ResourceResult:
        # Return ResourceResult explicitly so the template-side
        # conversion path preserves mime_type (FastMCP 3.2.4
        # drops it for raw str/bytes returns from templates).
        text = await _fetch_patch_text(mid)
        return ResourceResult([ResourceContent(text, mime_type="text/x-diff")])

    @mcp.resource(
        "lore://maintainer/{path}",
        name="lore_maintainer_block",
        description=(
            "MAINTAINERS block for a kernel source-tree path "
            "(e.g. lore://maintainer/fs/smb/server). Stub until Phase 18A."
        ),
        mime_type="text/plain",
    )
    async def _maintainer(path: str) -> str:
        return _MAINTAINER_STUB

    @mcp.resource(
        "lore://patchwork/{msg_id}",
        name="lore_patchwork_state",
        description=(
            "Patchwork state blob for a Message-ID — "
            "{state, delegate, series_id, checks[]}. Stub until Phase 19A."
        ),
        mime_type="application/json",
    )
    async def _patchwork(msg_id: str) -> ResourceResult:
        _ = msg_id  # unused for the stub, validated by FastMCP URI binding
        return ResourceResult([ResourceContent(_PATCHWORK_STUB_JSON, mime_type="application/json")])


__all__ = ["register_templated_resources"]
