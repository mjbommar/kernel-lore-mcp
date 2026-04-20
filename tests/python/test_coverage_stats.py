"""Unit tests for the coverage-stats markdown renderer and the
in-process cache that backs both the tool and the resource."""

from __future__ import annotations

import pytest

from kernel_lore_mcp.resources.coverage_stats import render_coverage_stats
from kernel_lore_mcp.tools import corpus_stats as stats_mod


def _fixture_stats(generation: int = 5) -> dict:
    return {
        "total_rows": 1234,
        "generation": generation,
        "generation_mtime_ns": 1_700_000_000 * 1_000_000_000,
        "schema_version": 1,
        "tier_generations": {
            "over": generation,
            "bm25": generation - 1,
            "trigram": generation,
            "tid": None,
        },
        "per_list": [
            {
                "list": "linux-cifs",
                "rows": 1000,
                "earliest_date_unix_ns": 1_600_000_000_000_000_000,
                "latest_date_unix_ns": 1_700_000_000_000_000_000,
            },
            {
                "list": "netdev",
                "rows": 234,
                "earliest_date_unix_ns": None,
                "latest_date_unix_ns": None,
            },
        ],
    }


def test_renderer_surfaces_headline_totals_and_drift() -> None:
    md = render_coverage_stats(_fixture_stats())
    assert "Total indexed messages:** 1,234" in md
    assert "Lists covered:** 2" in md
    # Drift detection for bm25 behind by 1.
    assert "behind by 1" in md
    # Tier marker absent signaled distinctly.
    assert "marker absent" in md
    # Per-list date windows rendered in UTC.
    assert "linux-cifs" in md
    # Cross-reference to blind_spots resource.
    assert "blind-spots://coverage" in md


def test_renderer_handles_empty_corpus_gracefully() -> None:
    empty = {
        "total_rows": 0,
        "generation": 0,
        "generation_mtime_ns": None,
        "schema_version": 1,
        "tier_generations": {
            "over": None,
            "bm25": None,
            "trigram": None,
            "tid": None,
        },
        "per_list": [],
    }
    md = render_coverage_stats(empty)
    assert "Total indexed messages:** 0" in md
    assert "Lists covered:** 0" in md
    assert "Last ingest:** never" in md
    assert "over.db" in md  # empty-corpus hint references it


class _FakeReader:
    """Counts corpus_stats() calls so we can assert cache behavior."""

    def __init__(self) -> None:
        self.calls = 0
        self._snap = _fixture_stats()

    def corpus_stats(self) -> dict:
        self.calls += 1
        return self._snap


@pytest.fixture(autouse=True)
def _clear_cache() -> None:
    stats_mod._cache.clear()


def test_cache_returns_same_snapshot_within_ttl() -> None:
    r = _FakeReader()
    snap1 = stats_mod._cached_corpus_stats(r, "/data", generation=7)
    snap2 = stats_mod._cached_corpus_stats(r, "/data", generation=7)
    assert r.calls == 1
    assert snap1 is snap2


def test_cache_invalidates_on_generation_change() -> None:
    r = _FakeReader()
    stats_mod._cached_corpus_stats(r, "/data", generation=7)
    stats_mod._cached_corpus_stats(r, "/data", generation=8)
    # One query per distinct generation.
    assert r.calls == 2
    # Stale entry pruned.
    keys = list(stats_mod._cache.keys())
    assert keys == [("/data", 8)]


def test_cache_respects_ttl_expiry(monkeypatch: pytest.MonkeyPatch) -> None:
    r = _FakeReader()
    # Freeze time, then advance past TTL.
    t = [1000.0]
    monkeypatch.setattr(stats_mod.time, "monotonic", lambda: t[0])
    stats_mod._cached_corpus_stats(r, "/data", generation=1)
    assert r.calls == 1
    t[0] += stats_mod.CACHE_TTL_SECONDS + 1
    stats_mod._cached_corpus_stats(r, "/data", generation=1)
    assert r.calls == 2
