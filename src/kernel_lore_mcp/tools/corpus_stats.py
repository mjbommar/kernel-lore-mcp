"""lore_corpus_stats — "what IS in here, and how fresh" at a glance.

Complements the two freshness surfaces we already ship:

  * `freshness` block on every tool response — per-call staleness.
  * `blind-spots://coverage` MCP resource — what is NOT indexed.

This tool (and its sibling `stats://coverage` resource) answer the
third question: which mailing lists DO we hold, how many rows each,
how fresh each tier is. Agents call it once at the start of a
workflow to pick the right search scope.

Cost is one indexed SQL `GROUP BY list` against over.db (~3 s on the
17.7M-row corpus; ~50 ms on small corpora). Results cached in-
process for `CACHE_TTL_SECONDS` so an agent fetching `stats://
coverage` AND then calling the tool pays the query once. The cache
is invalidated on generation change — a fresh ingest always bypasses
the stale snapshot.

Deliberately does not compute `COUNT(DISTINCT from_addr)` per list —
that doubles the cost and callers who care can issue a separate
`eq from_addr` query.
"""

from __future__ import annotations

import threading
import time
from datetime import UTC, datetime
from typing import Annotated, Any

from pydantic import BaseModel, Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.freshness import Freshness, build_freshness
from kernel_lore_mcp.timeout import run_with_timeout


class PerListStats(BaseModel):
    list: str = Field(description="Mailing list name, e.g. `linux-cifs`.")
    rows: int = Field(description="Indexed message count on this list.")
    earliest_date_unix_ns: int | None = None
    latest_date_unix_ns: int | None = None
    earliest_utc: datetime | None = None
    latest_utc: datetime | None = None


class TierGeneration(BaseModel):
    tier: str = Field(
        description="`over` | `bm25` | `trigram` | `tid` — the four "
        "per-tier generation markers ingest maintains."
    )
    generation: int | None = Field(
        default=None,
        description=(
            "Tier's generation counter. `None` means the marker file "
            "is absent — legacy pre-marker deployment, or the tier "
            "has not yet been written this data_dir."
        ),
    )
    status: str = Field(
        description="Human-readable drift status relative to the "
        "corpus generation: `in sync`, `behind by N`, `ahead by N`, "
        "or `marker absent`."
    )


class CorpusStatsResponse(BaseModel):
    total_rows: int = Field(
        description="Sum of all per-list row counts. "
        "Equals COUNT(*) FROM over."
    )
    lists_covered: int = Field(
        description="Number of distinct mailing lists in the corpus."
    )
    generation: int = Field(
        description="Corpus generation counter. Bumps once per ingest."
    )
    last_ingest_utc: datetime | None = Field(
        default=None,
        description="UTC timestamp of the most recent generation bump. "
        "`None` on a fresh data_dir.",
    )
    schema_version: int = Field(
        description="over.db schema version. Rebuild required on mismatch."
    )
    tiers: list[TierGeneration]
    lists: list[PerListStats]
    capabilities: dict[str, bool] = Field(
        default_factory=dict,
        description=(
            "Per-tier readiness booleans — `over_db_ready`, "
            "`bm25_ready`, `trigram_ready`, `tid_ready`, "
            "`path_vocab_ready`, `embedding_ready`, `maintainers_ready`, "
            "`git_sidecar_ready`, `metadata_ready`. Lets callers "
            "distinguish 'no results' from 'feature not provisioned'."
        ),
    )
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


CACHE_TTL_SECONDS = 30

# Module-level cache. Keyed on (data_dir, corpus_generation) so any
# ingest commit invalidates the snapshot automatically — we never
# serve stats for generation N after generation N+1 has landed.
_cache_lock = threading.Lock()
_cache: dict[tuple[str, int], tuple[float, dict[str, Any]]] = {}


def _cached_corpus_stats(reader, data_dir: str, generation: int) -> dict[str, Any]:
    """Return cached Rust-side corpus_stats() dict, refreshing when
    the TTL has expired or the corpus generation advanced. Thread-
    safe; the inner query is idempotent so a cache-miss race is
    correct (both callers would compute the same value)."""
    key = (data_dir, generation)
    now = time.monotonic()
    with _cache_lock:
        if key in _cache:
            fetched_at, snap = _cache[key]
            if now - fetched_at < CACHE_TTL_SECONDS:
                return snap
    # Miss or stale — compute outside the lock so a slow query
    # doesn't serialize other callers.
    snap = reader.corpus_stats()
    with _cache_lock:
        _cache[key] = (now, snap)
        # Drop old generation entries for this data_dir so the cache
        # doesn't grow unboundedly across a long-running process.
        stale = [k for k in _cache if k[0] == data_dir and k[1] != generation]
        for k in stale:
            _cache.pop(k, None)
    return snap


def _utc(ns: int | None) -> datetime | None:
    if ns is None:
        return None
    return datetime.fromtimestamp(ns / 1_000_000_000, tz=UTC)


def _status_for(gen_tier: int | None, gen_corpus: int) -> str:
    if gen_tier is None:
        return "marker absent"
    if gen_tier == gen_corpus:
        return "in sync"
    if gen_tier < gen_corpus:
        return f"behind by {gen_corpus - gen_tier}"
    return f"ahead by {gen_tier - gen_corpus}"


async def lore_corpus_stats(
    # pydantic Field() on the only argument keeps the MCP tool schema
    # self-describing even though this tool takes no user input.
    _: Annotated[
        bool,
        Field(
            default=True,
            description="Placeholder (no arguments). The tool always "
            "returns the full corpus summary.",
        ),
    ] = True,
) -> CorpusStatsResponse:
    """List every mailing list we've indexed, with row counts, date
    windows, and per-tier freshness markers.

    Cost: moderate — expected p95 3500 ms on full lore scale
    (17.7M rows, 341 lists). ~50 ms on small corpora. Result is
    cached in-process for 30 s and invalidated on generation
    change, so a burst of calls pays the GROUP BY once.
    """
    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    generation = 0
    try:
        generation = reader.generation()
    except Exception:
        # Fresh data_dir with no ingest yet — the cache key stays
        # (data_dir, 0); we'll recompute once on first real ingest.
        pass
    stats = await run_with_timeout(
        _cached_corpus_stats, reader, str(settings.data_dir), generation
    )

    generation = int(stats.get("generation", 0))
    tier_dict: dict[str, int | None] = stats.get("tier_generations", {})
    tiers = [
        TierGeneration(
            tier=name,
            generation=tier_dict.get(name),
            status=_status_for(tier_dict.get(name), generation),
        )
        for name in ("over", "bm25", "trigram", "tid")
    ]

    per_list_raw = stats.get("per_list", [])
    lists = [
        PerListStats(
            list=r["list"],
            rows=r["rows"],
            earliest_date_unix_ns=r.get("earliest_date_unix_ns"),
            latest_date_unix_ns=r.get("latest_date_unix_ns"),
            earliest_utc=_utc(r.get("earliest_date_unix_ns")),
            latest_utc=_utc(r.get("latest_date_unix_ns")),
        )
        for r in per_list_raw
    ]

    from kernel_lore_mcp.routes.status import capabilities as _capabilities

    return CorpusStatsResponse(
        total_rows=int(stats.get("total_rows", 0)),
        lists_covered=len(lists),
        generation=generation,
        last_ingest_utc=_utc(stats.get("generation_mtime_ns")),
        schema_version=int(stats.get("schema_version", 0)),
        tiers=tiers,
        lists=lists,
        capabilities=_capabilities(settings.data_dir),
        freshness=build_freshness(reader),
    )
