"""FastMCP server assembly.

Tool registration is **explicit** (not side-effect import). This
avoids the circular-import hazard between `server.py` and
`tools/*.py` and makes the registered surface easy to audit.
"""

from __future__ import annotations

from fastmcp import FastMCP

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.resources.blind_spots import BLIND_SPOTS_URI, blind_spots_text
from kernel_lore_mcp.routes.metrics import metrics_endpoint
from kernel_lore_mcp.routes.status import status_endpoint

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
`blind-spots://coverage` enumerates what is NOT visible (private
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
    from kernel_lore_mcp.tools.explain_patch import lore_explain_patch
    from kernel_lore_mcp.tools.message import lore_message
    from kernel_lore_mcp.tools.nearest import lore_nearest, lore_similar
    from kernel_lore_mcp.tools.patch import lore_patch
    from kernel_lore_mcp.tools.patch_diff import lore_patch_diff
    from kernel_lore_mcp.tools.patch_search import lore_patch_search
    from kernel_lore_mcp.tools.primitives import (
        lore_count,
        lore_diff,
        lore_eq,
        lore_in_list,
        lore_regex,
        lore_substr_subject,
        lore_substr_trailers,
    )
    from kernel_lore_mcp.tools.search import lore_search
    from kernel_lore_mcp.tools.series import lore_series_timeline
    from kernel_lore_mcp.tools.thread import lore_thread

    # Every tool shares the same four-annotation shape. Corpus grows
    # over time as new mail arrives (`openWorldHint=true`); none of
    # our tools mutate state (`readOnlyHint=true`, `destructiveHint=
    # false`); rerunning a tool against the same generation returns
    # the same result (`idempotentHint=true`). Per-tool `title` is
    # what changes — it's shown to users in tool pickers.
    def ann(title: str) -> dict[str, object]:
        return {
            "title": title,
            "readOnlyHint": True,
            "destructiveHint": False,
            "idempotentHint": True,
            "openWorldHint": True,
        }

    # Higher-level / orchestrating tools.
    mcp.tool(lore_search, annotations=ann("Search lore (fused tiers)"))
    mcp.tool(lore_activity, annotations=ann("File / function activity over lore"))
    mcp.tool(lore_message, annotations=ann("Fetch one message (prose + patch split)"))
    mcp.tool(lore_expand_citation, annotations=ann("Expand Message-ID / SHA / CVE"))
    mcp.tool(lore_series_timeline, annotations=ann("Sibling versions of a patch series"))
    mcp.tool(lore_patch_search, annotations=ann("Literal substring search in patch bodies"))
    mcp.tool(lore_thread, annotations=ann("Walk a full conversation thread"))
    mcp.tool(lore_patch, annotations=ann("Raw patch text for one message"))
    mcp.tool(lore_patch_diff, annotations=ann("Diff two patch versions of a series"))
    mcp.tool(lore_explain_patch, annotations=ann("One-call deep view of a patch"))

    # Low-level retrieval primitives. Agents stack these themselves
    # when they want one well-defined query against one tier.
    mcp.tool(lore_eq, annotations=ann("Exact-equality scan on a column"))
    mcp.tool(lore_in_list, annotations=ann("Set-membership scan on a column"))
    mcp.tool(lore_count, annotations=ann("Count + distinct-authors + date range"))
    mcp.tool(lore_substr_subject, annotations=ann("Case-insensitive substring on subject"))
    mcp.tool(lore_substr_trailers, annotations=ann("Substring inside a named trailer"))
    mcp.tool(lore_regex, annotations=ann("DFA-only regex scan"))
    mcp.tool(lore_diff, annotations=ann("Message-vs-message diff (patch / prose / raw)"))

    # Embedding tier (Phase 8). Both tools fail loudly with an
    # actionable ToolError when the index hasn't been built yet.
    mcp.tool(lore_nearest, annotations=ann("Semantic nearest-neighbour on free text"))
    mcp.tool(lore_similar, annotations=ann("Nearest-neighbour on a seed message-id"))

    # Register blind_spots as an MCP resource — fetch once per session.
    @mcp.resource(
        uri=BLIND_SPOTS_URI,
        name="coverage",
        description="What this index does NOT contain (embargoed queues, distro backports, etc).",
        mime_type="text/plain",
    )
    def _blind_spots() -> str:
        return blind_spots_text()

    # Non-MCP HTTP routes. Accessible only when transport=http.
    mcp.custom_route("/status", methods=["GET"])(status_endpoint)
    mcp.custom_route("/metrics", methods=["GET"])(metrics_endpoint)

    # TODO(phase-5+): lore_thread, lore_patch, lore_patch_diff,
    # lore_explain_patch once the router lands.
    _ = settings
    return mcp
