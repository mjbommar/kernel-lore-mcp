"""`stats://coverage` — what IS indexed, how fresh each tier is.

Exposed as an MCP resource so agents can fetch once per session to
know which lists are present, how many rows each holds, and how far
behind upstream we are. Complements `blind-spots://coverage` (which
enumerates what's NOT in the corpus).

Resource body is human-readable markdown rather than JSON so an LLM
client can cite it back to the user with no additional parsing; the
`lore_corpus_stats` tool returns the same underlying data as a
structured `CorpusStatsResponse` for programmatic callers.
"""

from __future__ import annotations

from datetime import UTC, datetime

COVERAGE_STATS_URI = "stats://coverage"


def _utc_str(ns: int | None) -> str:
    if ns is None:
        return "never"
    return datetime.fromtimestamp(ns / 1_000_000_000, tz=UTC).strftime(
        "%Y-%m-%d %H:%M:%S UTC"
    )


def _fmt_int(n: int) -> str:
    """`1234567` → `1,234,567`. Cheap readability win in a corpus stat."""
    return f"{n:,}"


def render_coverage_stats(stats: dict) -> str:
    """Render the `_core.Reader.corpus_stats()` dict as markdown.

    Kept pure — no Reader / Settings dependency — so it's trivially
    unit-testable. The resource handler in server.py passes the live
    stats dict; tests pass canned fixtures.
    """
    total = stats.get("total_rows", 0)
    generation = stats.get("generation", 0)
    gen_mtime_ns = stats.get("generation_mtime_ns")
    schema_version = stats.get("schema_version", 0)
    tiers: dict[str, int | None] = stats.get("tier_generations", {})
    per_list: list[dict] = stats.get("per_list", [])

    lines: list[str] = []
    lines.append("# kernel-lore-mcp — coverage stats")
    lines.append("")
    lines.append(f"- **Total indexed messages:** {_fmt_int(total)}")
    lines.append(f"- **Lists covered:** {len(per_list)}")
    lines.append(f"- **Corpus generation:** {generation}")
    lines.append(f"- **Last ingest:** {_utc_str(gen_mtime_ns)}")
    lines.append(f"- **Schema version:** {schema_version}")
    lines.append("")

    # Tier drift table. Any tier != corpus generation is a drift
    # signal the operator should see; reader bypasses over.db on
    # `over` drift, so this is load-bearing for understanding
    # "why does this query look stale?"
    lines.append("## Tier generations")
    lines.append("")
    lines.append("| tier | generation | status |")
    lines.append("|---|---|---|")
    for name in ("over", "bm25", "trigram", "tid", "path_vocab"):
        val = tiers.get(name)
        if val is None:
            status = "marker absent (legacy / not yet written)"
            gen_str = "-"
        elif val == generation:
            status = "in sync"
            gen_str = str(val)
        elif val < generation:
            status = f"behind by {generation - val}"
            gen_str = str(val)
        else:
            status = f"ahead by {val - generation}"
            gen_str = str(val)
        lines.append(f"| {name} | {gen_str} | {status} |")
    lines.append("")

    caps: dict[str, bool] = stats.get("capabilities", {})
    if caps:
        lines.append("## Capabilities")
        lines.append("")
        lines.append("| capability | ready |")
        lines.append("|---|---|")
        for name in sorted(caps):
            flag = "yes" if caps[name] else "no"
            lines.append(f"| `{name}` | {flag} |")
        lines.append("")

    if per_list:
        lines.append("## Per-list coverage")
        lines.append("")
        lines.append("| list | rows | earliest | latest |")
        lines.append("|---|---:|---|---|")
        for row in per_list:
            lines.append(
                f"| `{row['list']}` | {_fmt_int(row['rows'])} | "
                f"{_utc_str(row.get('earliest_date_unix_ns'))} | "
                f"{_utc_str(row.get('latest_date_unix_ns'))} |"
            )
        lines.append("")
    else:
        lines.append(
            "_No per-list stats available. This typically means over.db "
            "is absent or stale; see runbook §10A._"
        )
        lines.append("")

    lines.append(
        "See `blind-spots://coverage` for what is NOT indexed. Every "
        "tool response also carries a `freshness` block with `as_of`, "
        "`lag_seconds`, and `generation` so per-call staleness is "
        "always available without re-fetching this resource."
    )
    return "\n".join(lines)


__all__ = ["COVERAGE_STATS_URI", "render_coverage_stats"]
