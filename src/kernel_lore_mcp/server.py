"""FastMCP server assembly.

Tool registration is **explicit** (not side-effect import). This
avoids the circular-import hazard between `server.py` and
`tools/*.py` and makes the registered surface easy to audit.
"""

from __future__ import annotations

from fastmcp import FastMCP

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.prompts import register_prompts
from kernel_lore_mcp.resources.blind_spots import BLIND_SPOTS_URI, blind_spots_text
from kernel_lore_mcp.resources.templates import register_templated_resources
from kernel_lore_mcp.routes.metrics import metrics_endpoint
from kernel_lore_mcp.routes.status import status_endpoint

INSTRUCTIONS = """\
Search and retrieve messages from the Linux kernel mailing list archives
(lore.kernel.org). All tools are live and backed by real indices.

Tool families:
  Search — lore_search (fused BM25 + trigram + metadata via RRF),
    lore_patch_search (literal or fuzzy substring in patch bodies),
    lore_regex (DFA-only regex over subject/from/prose/patch),
    lore_path_mentions (Aho-Corasick file-path reverse index).
  Lookup — lore_message, lore_expand_citation, lore_thread, lore_patch,
    lore_patch_diff, lore_explain_patch, lore_series_timeline.
  Primitives — lore_eq, lore_in_list, lore_count, lore_substr_subject,
    lore_substr_trailers, lore_diff.
  Activity — lore_activity (file/function touches over time).
  Semantic — lore_nearest (free-text → ANN), lore_similar (seed mid → ANN).
  Sampling — lore_summarize_thread, lore_classify_patch,
    lore_explain_review_status (LLM via ctx.sample, extractive fallback).

Every tool's description includes a cost class (cheap/moderate/expensive)
and expected p95 latency. Use `response_format="concise"` on high-volume
tools to cap tokens.

Coverage is lore public archives only. The MCP resource
`blind-spots://coverage` enumerates what is NOT visible. Fetch it once
per session; do not re-fetch per call.

Freshness: every response carries a populated `freshness` block with
`as_of`, `lag_seconds`, and `generation`. End-to-end p50 ~5 min,
p95 ~11 min from vger to our index.
"""


def build_server(settings: Settings | None = None) -> FastMCP:
    """Construct the FastMCP app with all v0.5 tools registered."""
    from kernel_lore_mcp.config import set_settings

    settings = settings or Settings()
    set_settings(settings)
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
    from kernel_lore_mcp.tools.path_mentions import lore_path_mentions
    from kernel_lore_mcp.tools.primitives import (
        lore_count,
        lore_diff,
        lore_eq,
        lore_in_list,
        lore_regex,
        lore_substr_subject,
        lore_substr_trailers,
    )
    from kernel_lore_mcp.tools.sampling_tools import (
        lore_classify_patch,
        lore_explain_review_status,
        lore_summarize_thread,
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

    # Phase 13a-file — Aho-Corasick path-mention reverse index.
    mcp.tool(lore_path_mentions, annotations=ann("Find messages mentioning a file path"))

    # Phase 12 — sampling-backed tools with graceful extractive
    # fallback. `backend` on every response tells the agent which
    # path fired (sampled / extractive) so downstream confidence
    # stays honest.
    mcp.tool(lore_summarize_thread, annotations=ann("Summarize a thread (LLM or extractive)"))
    mcp.tool(lore_classify_patch, annotations=ann("Classify a patch into a fixed label set"))
    mcp.tool(
        lore_explain_review_status,
        annotations=ann("Explain open reviewer concerns + trailers seen"),
    )

    # Register blind_spots as an MCP resource — fetch once per session.
    @mcp.resource(
        uri=BLIND_SPOTS_URI,
        name="coverage",
        description="What this index does NOT contain (embargoed queues, distro backports, etc).",
        mime_type="text/plain",
    )
    def _blind_spots() -> str:
        return blind_spots_text()

    # Phase 10 — RFC-6570 templated resources. `lore://message/{mid}`,
    # `lore://thread/{tid}`, `lore://patch/{mid}` wrap existing reader
    # paths; `lore://maintainer/{path}` and `lore://patchwork/{msg_id}`
    # return a stub body that names the phase that ships real data.
    register_templated_resources(mcp)

    # Phase 11 — server-provided prompts (Claude Code slash commands).
    # 5 prompts encoding the canonical kernel-research workflows.
    # Every argument has a default so the slash command is invocable
    # with zero user input (anthropics/claude-code#30733).
    register_prompts(mcp)

    # Non-MCP HTTP routes. Accessible only when transport=http.
    mcp.custom_route("/status", methods=["GET"])(status_endpoint)
    mcp.custom_route("/metrics", methods=["GET"])(metrics_endpoint)

    # TODO(phase-5+): lore_thread, lore_patch, lore_patch_diff,
    # lore_explain_patch once the router lands.
    _ = settings
    return mcp
