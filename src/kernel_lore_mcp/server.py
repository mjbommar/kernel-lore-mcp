"""FastMCP server assembly.

Tool registration is **explicit** (not side-effect import). This
avoids the circular-import hazard between `server.py` and
`tools/*.py` and makes the registered surface easy to audit.
"""

from __future__ import annotations

from fastmcp import FastMCP

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.observability import MetricsMiddleware
from kernel_lore_mcp.prompts import register_prompts
from kernel_lore_mcp.resources.blind_spots import BLIND_SPOTS_URI, blind_spots_text
from kernel_lore_mcp.resources.coverage_stats import (
    COVERAGE_STATS_URI,
    render_coverage_stats,
)
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
    mcp.add_middleware(MetricsMiddleware())

    # Explicit tool registration.
    from kernel_lore_mcp.tools.activity import lore_activity
    from kernel_lore_mcp.tools.author_footprint import lore_author_footprint
    from kernel_lore_mcp.tools.author_profile import lore_author_profile
    from kernel_lore_mcp.tools.corpus_stats import lore_corpus_stats
    from kernel_lore_mcp.tools.expand_citation import lore_expand_citation
    from kernel_lore_mcp.tools.explain_patch import lore_explain_patch
    from kernel_lore_mcp.tools.file_timeline import lore_file_timeline
    from kernel_lore_mcp.tools.maintainer_profile import lore_maintainer_profile
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
    from kernel_lore_mcp.tools.stable_backport import lore_stable_backport_status
    from kernel_lore_mcp.tools.subsystem_churn import lore_subsystem_churn
    from kernel_lore_mcp.tools.thread import lore_thread
    from kernel_lore_mcp.tools.thread_state import lore_thread_state

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

    # Cost-class concurrency wrapper. Each tool's Cost: docstring line
    # determines its class (cheap/moderate/expensive); the wrapper
    # enforces a per-class in-flight cap and rejects with a structured
    # `rate_limited` error when the class is saturated. See
    # kernel_lore_mcp.cost_class for the limits and rationale.
    from kernel_lore_mcp.cost_class import cost_limited

    def _reg(fn, title: str) -> None:
        mcp.tool(cost_limited(fn), annotations=ann(title))

    # Higher-level / orchestrating tools.
    _reg(lore_search, "Search lore (fused tiers)")
    _reg(lore_activity, "File / function activity over lore")
    _reg(lore_message, "Fetch one message (prose + patch split)")
    _reg(lore_expand_citation, "Expand Message-ID / SHA / CVE")
    _reg(lore_series_timeline, "Sibling versions of a patch series")
    _reg(lore_patch_search, "Literal substring search in patch bodies")
    _reg(lore_thread, "Walk a full conversation thread")
    _reg(lore_patch, "Raw patch text for one message")
    _reg(lore_patch_diff, "Diff two patch versions of a series")
    _reg(lore_explain_patch, "One-call deep view of a patch")

    # Low-level retrieval primitives. Agents stack these themselves
    # when they want one well-defined query against one tier.
    _reg(lore_eq, "Exact-equality scan on a column")
    _reg(lore_in_list, "Set-membership scan on a column")
    _reg(lore_count, "Count + distinct-authors + date range")
    _reg(lore_substr_subject, "Case-insensitive substring on subject")
    _reg(lore_substr_trailers, "Substring inside a named trailer")
    _reg(lore_regex, "DFA-only regex scan")
    _reg(lore_diff, "Message-vs-message diff (patch / prose / raw)")
    _reg(lore_author_profile, "Aggregate profile for one from_addr")
    _reg(lore_corpus_stats, "Coverage stats: per-list rows + tier freshness")
    _reg(
        lore_author_footprint,
        "Every lore message that mentions an address",
    )
    _reg(
        lore_maintainer_profile,
        "Declared vs. observed ownership for a kernel path",
    )
    _reg(
        lore_stable_backport_status,
        "Was this mainline SHA picked up by -stable?",
    )
    _reg(
        lore_file_timeline,
        "Chronological patch activity on one file (with histogram)",
    )
    _reg(
        lore_thread_state,
        "Classify a thread (rfc/superseded/nacked/...)",
    )
    _reg(
        lore_subsystem_churn,
        "Hot files in a list/subsystem (top-N + histogram)",
    )

    # Embedding tier (Phase 8). Both tools fail loudly with an
    # actionable ToolError when the index hasn't been built yet.
    _reg(lore_nearest, "Semantic nearest-neighbour on free text")
    _reg(lore_similar, "Nearest-neighbour on a seed message-id")

    # Phase 13a-file — Aho-Corasick path-mention reverse index.
    _reg(lore_path_mentions, "Find messages mentioning a file path")

    # Phase 12 — sampling-backed tools with graceful extractive
    # fallback. `backend` on every response tells the agent which
    # path fired (sampled / extractive) so downstream confidence
    # stays honest.
    _reg(lore_summarize_thread, "Summarize a thread (LLM or extractive)")
    _reg(lore_classify_patch, "Classify a patch into a fixed label set")
    _reg(
        lore_explain_review_status,
        "Explain open reviewer concerns + trailers seen",
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

    # Companion to blind_spots: `stats://coverage` answers "what IS
    # in here" — per-list row counts, date windows, tier freshness.
    # Markdown body so an LLM can cite it directly; the
    # `lore_corpus_stats` tool returns the same data as a structured
    # pydantic response for programmatic callers.
    @mcp.resource(
        uri=COVERAGE_STATS_URI,
        name="coverage_stats",
        description="Which lists ARE indexed, per-list row counts, tier freshness.",
        mime_type="text/markdown",
    )
    def _coverage_stats() -> str:
        # Share the cache with lore_corpus_stats so an agent that
        # reads the resource and then calls the tool pays the GROUP
        # BY once.
        from kernel_lore_mcp import _core
        from kernel_lore_mcp.config import get_settings
        from kernel_lore_mcp.routes.status import capabilities
        from kernel_lore_mcp.tools.corpus_stats import _cached_corpus_stats

        settings = get_settings()
        reader = _core.Reader(settings.data_dir)
        try:
            generation = reader.generation()
        except Exception:
            generation = 0
        snap = dict(_cached_corpus_stats(reader, str(settings.data_dir), generation))
        # Capabilities are a deployment-state question, not a corpus
        # query — compute them at resource-assembly time so the
        # markdown surfaces them even on fresh data_dirs where the
        # Rust corpus_stats returns an empty envelope.
        snap["capabilities"] = capabilities(settings.data_dir)
        return render_coverage_stats(snap)

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

    # Boot-time warmup: page in the BM25 mmap and an over.db connection
    # so the FIRST real request doesn't pay the ~1.3 s cold-cache tail
    # we measured pre-fix. Any error here is swallowed — a deployment
    # that hasn't built BM25 yet should still boot the server. Best-
    # effort; logs emit at debug, not warn, so a missing tier doesn't
    # look like a production incident.
    _warmup_tiers(settings)

    # TODO(phase-5+): lore_thread, lore_patch, lore_patch_diff,
    # lore_explain_patch once the router lands.
    return mcp


def _warmup_tiers(settings: Settings) -> None:
    """Fire one throwaway query against each tier that keeps large
    read-only state cached in the shared process-local Reader.
    """
    import logging

    log = logging.getLogger(__name__)
    try:
        from kernel_lore_mcp.reader_cache import get_reader

        reader = get_reader()
        # BM25: cheapest valid query that touches segment readers.
        try:
            reader.prose_search("the", 1)
        except Exception as exc:
            log.debug("bm25 warmup skipped: %s", exc)
        # Trigram / store / over.db indexes get touched lazily on first
        # lookup; one cheap mid-shape router query exercises them.
        try:
            reader.router_search("list:lkml", 1)
        except Exception as exc:
            log.debug("router warmup skipped: %s", exc)
        # Trigram segment cache: `patch_search` opens ~530 segments
        # cross-list on lore scale; first call costs ~9 s page-in
        # even with the per-process cache (#66). Fire a cheap probe
        # here so the OS page cache holds the fst/postings/docs
        # files before the first real request. Needle is picked to
        # hit enough trigrams that the full walk runs but stop at
        # one result so we don't spend wall-clock on the confirm.
        try:
            reader.patch_search("__function__", None, 1)
        except Exception as exc:
            log.debug("trigram warmup skipped: %s", exc)
    except Exception as exc:
        log.debug("warmup skipped entirely: %s", exc)
