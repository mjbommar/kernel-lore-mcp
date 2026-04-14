"""FastMCP server assembly.

Tool registration is **explicit** (not side-effect import). This
avoids the circular-import hazard between `server.py` and
`tools/*.py` and makes the registered surface easy to audit.
"""

from __future__ import annotations

from fastmcp import FastMCP

from kernel_lore_mcp.config import Settings

INSTRUCTIONS = """\
Search and retrieve messages from the Linux kernel mailing list archives
(lore.kernel.org). Discovery via lore_search; pull a full thread via
lore_thread; fetch patch text via lore_patch; find recent activity on a
file/function via lore_activity; fetch any single message via
lore_message.

Coverage is lore public archives only. The MCP resource
`blind_spots://coverage` enumerates what is NOT visible (private
security@kernel.org queue, distro vendor backports, syzbot pre-public,
research-shop pipelines, CVE in-flight embargoes). Fetch it once per
session; do not re-fetch per call.

Freshness: lore runs 1-5 minutes behind vger. Every discovery response
carries a `freshness` block; also see /status.
"""


def build_server(settings: Settings | None = None) -> FastMCP:
    """Construct the FastMCP app with all v1 tools registered."""
    settings = settings or Settings()
    mcp: FastMCP = FastMCP(name="kernel-lore", instructions=INSTRUCTIONS)

    # Explicit tool registration. Import-then-register, not
    # import-for-side-effect. See TODO.md phase 2.
    from kernel_lore_mcp.tools.search import lore_search

    mcp.tool(
        lore_search,
        annotations={"readOnlyHint": True, "idempotentHint": True},
    )

    # TODO(task #17 ff): register lore_thread, lore_patch,
    # lore_activity, lore_message, lore_series_versions,
    # lore_patch_diff once those tool modules land.

    # TODO: register blind_spots resource and /status + /metrics routes.
    _ = settings
    return mcp
