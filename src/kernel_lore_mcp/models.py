"""Pydantic response models.

FastMCP 3.x serializes BaseModel return types as structured content
with `outputSchema` auto-derived, which is what LLM clients expect.
Returning a bare dict collapses to a `TextContent` block with
stringified JSON — avoid.
"""

from __future__ import annotations

from datetime import datetime

from pydantic import BaseModel, Field


class Freshness(BaseModel):
    """Freshness envelope attached to discovery responses."""

    last_ingest_utc: datetime | None = None
    oldest_list_last_updated: datetime | None = None
    stale_lists: list[str] = Field(
        default_factory=list,
        description="Lists that haven't updated in > ingest_cadence * 3.",
    )


class PatchStats(BaseModel):
    files_changed: int
    insertions: int
    deletions: int


class Snippet(BaseModel):
    """A body snippet with verifiable provenance (offset+length+sha256)."""

    offset: int = Field(ge=0)
    length: int = Field(ge=0)
    sha256: str
    text: str


class SearchHit(BaseModel):
    """A single result row. Fields shaped for LLM consumption."""

    message_id: str
    cite_key: str = Field(
        description='Stable short handle, e.g. "linux-cifs/2026-04/ksmbd-alloc-user-v3".',
    )
    list: str
    cross_posted_to: list[str] = Field(default_factory=list)
    from_addr: str | None
    from_name: str | None
    subject: str
    subject_tags: list[str] = Field(default_factory=list)
    date: datetime
    has_patch: bool
    is_cover_letter: bool = False
    series_version: int | None = None
    series_index: str | None = Field(
        default=None,
        description='"N/M" if part of a numbered series.',
    )
    patch_stats: PatchStats | None = None
    snippet: Snippet | None = None
    score: float | None = Field(
        default=None,
        description="BM25 score if the hit came through BM25; null otherwise.",
    )
    tier_provenance: list[str] = Field(
        description='Which tier(s) produced the hit: "metadata", "trigram", "bm25".',
    )
    is_exact_match: bool = Field(
        default=False,
        description="True for trigram post-confirmation and metadata-exact hits.",
    )
    lore_url: str


class SearchResponse(BaseModel):
    """Top-level envelope for lore_search."""

    results: list[SearchHit]
    next_cursor: str | None = None
    query_tiers_hit: list[str]
    default_applied: list[str] = Field(
        default_factory=list,
        description='Silently-applied defaults such as "rt:5y"; surfaces so LLMs know.',
    )
    candidate_set_warning: str | None = Field(
        default=None,
        description="Populated when the candidate set exceeds the warn threshold.",
    )
    truncated_by_candidate_cap: bool = False
    freshness: Freshness
    blind_spots_ref: str = Field(
        default="blind_spots://coverage",
        description="MCP resource pointer. Fetch once, not per response.",
    )
