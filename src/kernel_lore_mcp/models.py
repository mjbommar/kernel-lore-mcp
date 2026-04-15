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
        default="blind-spots://coverage",
        description="MCP resource pointer. Fetch once, not per response.",
    )


class ActivityRow(BaseModel):
    """One row in `lore_activity`; one per message in v0.5.

    Grouping by tid lands in a later phase once the tid computation
    pass is wired; for now each matching message is its own row with
    enough metadata that an agent can group client-side.
    """

    message_id: str
    cite_key: str
    list: str
    from_addr: str | None
    from_name: str | None
    subject: str
    subject_tags: list[str] = Field(default_factory=list)
    date: datetime | None
    has_patch: bool
    is_cover_letter: bool = False
    series_version: int | None = None
    series_index: str | None = None
    patch_stats: PatchStats | None = None
    reviewed_by: list[str] = Field(default_factory=list)
    acked_by: list[str] = Field(default_factory=list)
    tested_by: list[str] = Field(default_factory=list)
    signed_off_by: list[str] = Field(default_factory=list)
    fixes: list[str] = Field(default_factory=list)
    cc_stable: list[str] = Field(default_factory=list)
    lore_url: str


class ActivityResponse(BaseModel):
    rows: list[ActivityRow]
    total: int = Field(description="Row count after filtering (not capped by limit).")
    default_applied: list[str] = Field(default_factory=list)
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class MessageResponse(BaseModel):
    """`lore_message` / `lore_explain_patch`."""

    hit: SearchHit
    prose: str | None = None
    patch: str | None = None
    body_sha256: str
    body_length: int
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class SeriesTimelineEntry(BaseModel):
    message_id: str
    cite_key: str
    subject: str
    series_version: int | None
    series_index: str | None
    date: datetime | None
    reviewed_by: list[str] = Field(default_factory=list)
    acked_by: list[str] = Field(default_factory=list)
    lore_url: str


class SeriesTimelineResponse(BaseModel):
    entries: list[SeriesTimelineEntry]
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class ExpandCitationResponse(BaseModel):
    results: list[SearchHit]
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class ThreadMessage(BaseModel):
    """One message in a conversation, with prose and patch separated."""

    hit: SearchHit
    prose: str | None = None
    patch: str | None = None


class ThreadResponse(BaseModel):
    root_message_id: str
    messages: list[ThreadMessage]
    truncated: bool = False
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class PatchResponse(BaseModel):
    """Raw patch text for one message."""

    hit: SearchHit
    patch: str
    body_sha256: str
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class PatchDiffResponse(BaseModel):
    """Diff-of-diffs between two patch versions of the same series."""

    a: SearchHit
    b: SearchHit
    diff: str
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class RowsResponse(BaseModel):
    """Plain envelope around a list of `SearchHit` rows + the
    standard freshness/blind-spots fields. Used by the low-level
    primitives (eq / in_list / substr_* / regex) — none of which
    rank, so no fused score and `tier_provenance` is fixed per
    primitive.
    """

    results: list[SearchHit]
    total: int
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class CountResponse(BaseModel):
    """Aggregate counts from `lore_count`."""

    count: int
    distinct_authors: int
    earliest_unix_ns: int | None = None
    latest_unix_ns: int | None = None
    earliest_utc: datetime | None = None
    latest_utc: datetime | None = None
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class DiffResponse(BaseModel):
    """Generalized message-vs-message diff."""

    a: SearchHit
    b: SearchHit
    mode: str = Field(description='One of: "patch", "prose", "raw".')
    diff: str
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class NearestHit(BaseModel):
    """One hit from the embedding tier. `score` is cosine similarity
    in [-1, 1]; higher = more similar. `tier_provenance` is fixed to
    ["embedding"]; `is_exact_match` is always False (semantic, not
    structural).
    """

    message_id: str
    cite_key: str
    score: float
    list: str
    from_addr: str | None
    subject: str
    date: datetime | None
    has_patch: bool
    lore_url: str


class NearestResponse(BaseModel):
    results: list[NearestHit]
    model: str = Field(description="Embedder model name; matches the indexed model.")
    dim: int
    tier_provenance: list[str] = Field(default_factory=lambda: ["embedding"])
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class ExplainPatchResponse(BaseModel):
    """One-call view: prose + patch + series timeline + downstream replies."""

    hit: SearchHit
    prose: str | None
    patch: str | None
    series: list[SeriesTimelineEntry] = Field(default_factory=list)
    downstream: list[SearchHit] = Field(
        default_factory=list,
        description="Direct replies (in_reply_to == this mid).",
    )
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"
