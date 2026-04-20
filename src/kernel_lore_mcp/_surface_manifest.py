"""Canonical list of the MCP surface elements that MUST exist on a
healthy deployment.

Single source of truth consumed by:
  * `scripts/klmcp-doctor.sh` (runtime sanity probe),
  * `tests/python/test_surface_manifest.py` (CI drift-detector),
  * any future `kernel-lore-mcp surface` CLI that prints the ship
    surface for ops review.

The sets intentionally under-specify the full registered surface:
we list only the tools / resource templates / prompts the MCP
contract considers mandatory. Any actually-registered extras
beyond this set are fine — think of these sets as "at minimum,
these must be present."

When a new tool / template / prompt lands, add its name here AND
make sure `src/kernel_lore_mcp/server.py` registers it. The
paired pytest (`test_surface_manifest.py`) asserts subset
containment and will fail CI if either side drifts.
"""

from __future__ import annotations


REQUIRED_TOOLS: frozenset[str] = frozenset(
    {
        "lore_search",
        "lore_eq",
        "lore_patch_search",
        "lore_summarize_thread",
        "lore_classify_patch",
        "lore_explain_review_status",
        # Coverage-transparency (v0.2.0).
        "lore_corpus_stats",
    }
)

REQUIRED_RESOURCE_TEMPLATES: frozenset[str] = frozenset(
    {
        # URIs as registered in src/kernel_lore_mcp/resources/templates.py.
        "lore://message/{mid}",
        "lore://thread/{mid}",
        "lore://patch/{mid}",
        "lore://maintainer/{path}",
        "lore://patchwork/{msg_id}",
    }
)

REQUIRED_PROMPTS: frozenset[str] = frozenset(
    {
        "klmcp_pre_disclosure_novelty_check",
        "klmcp_cve_chain_expand",
        "klmcp_series_version_diff",
        "klmcp_recent_reviewers_for",
        "klmcp_cross_subsystem_pattern_transfer",
    }
)

REQUIRED_STATIC_RESOURCES: frozenset[str] = frozenset(
    {
        "blind-spots://coverage",
        "stats://coverage",
    }
)


__all__ = [
    "REQUIRED_TOOLS",
    "REQUIRED_RESOURCE_TEMPLATES",
    "REQUIRED_PROMPTS",
    "REQUIRED_STATIC_RESOURCES",
]
