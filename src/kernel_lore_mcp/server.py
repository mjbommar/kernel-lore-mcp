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
(lore.kernel.org). The tools available in v0.5 answer structured
metadata queries; prose BM25 and patch/code trigram search land in
follow-up phases.

Tools:
  lore_activity(file|function, since?, list?, limit?)
    Find recent messages touching a file or function.
  lore_message(message_id)
    Fetch one message + its prose/patch split + raw body bytes.
  lore_expand_citation(token)
    Resolve a Message-ID, a git commit SHA, or a CVE ID.
  lore_series_timeline(message_id)
    Return sibling versions (v1/v2/v3/...) of the same patch series.
  lore_search(query, limit?, cursor?)
    Free-text search — returns an empty SearchResponse in v0.5;
    wired to real indices in Phase 3/4.

Coverage is lore public archives only. The MCP resource
`blind_spots://coverage` enumerates what is NOT visible (private
security@kernel.org queue, distro vendor backports, syzbot pre-public,
research-shop pipelines, CVE in-flight embargoes). Fetch it once per
session; do not re-fetch per call.

Freshness: lore runs 1-5 minutes behind vger. Every response carries
a `freshness` block.
"""


def build_server(settings: Settings | None = None) -> FastMCP:
    """Construct the FastMCP app with all v0.5 tools registered."""
    settings = settings or Settings()
    mcp: FastMCP = FastMCP(name="kernel-lore", instructions=INSTRUCTIONS)

    # Explicit tool registration.
    from kernel_lore_mcp.tools.activity import lore_activity
    from kernel_lore_mcp.tools.expand_citation import lore_expand_citation
    from kernel_lore_mcp.tools.message import lore_message
    from kernel_lore_mcp.tools.search import lore_search
    from kernel_lore_mcp.tools.series import lore_series_timeline

    read_only = {"readOnlyHint": True, "idempotentHint": True}

    mcp.tool(lore_search, annotations=read_only)
    mcp.tool(lore_activity, annotations=read_only)
    mcp.tool(lore_message, annotations=read_only)
    mcp.tool(lore_expand_citation, annotations=read_only)
    mcp.tool(lore_series_timeline, annotations=read_only)

    # TODO(phase-3/4): lore_patch, lore_thread, lore_patch_diff,
    # lore_explain_patch once the trigram + BM25 tiers land.
    # TODO(phase-2): register blind_spots resource + /status route.
    _ = settings
    return mcp
