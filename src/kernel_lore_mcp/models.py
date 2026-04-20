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
    """Freshness envelope attached to discovery responses.

    `as_of` + `lag_seconds` are populated from the generation-file
    mtime on every request; `generation` mirrors the monotonic counter
    so reruns of the same query can detect drift. `last_ingest_utc` is
    an alias for `as_of` kept for wire-compat with earlier clients.
    """

    as_of: datetime | None = Field(
        default=None,
        description="Server-side timestamp the responding index was last committed.",
    )
    lag_seconds: int | None = Field(
        default=None,
        ge=0,
        description="Seconds between the index commit and now — an upper bound on ingest lag.",
    )
    generation: int | None = Field(
        default=None,
        ge=0,
        description="Monotonic ingest-generation counter. Bumps at every committed ingest.",
    )
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
    date: datetime | None = Field(
        default=None,
        description=(
            "Message date (UTC). None when the RFC822 Date header was "
            "missing and no commit-date fallback was available."
        ),
    )
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

    `next_cursor` is populated by tools that paginate (currently
    `lore_regex` and the low-level `substr_*` primitives with
    pagination-wired variants); unpaginated tools always return
    `None` here.
    """

    results: list[SearchHit]
    total: int
    next_cursor: str | None = None
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


class SubsystemBucket(BaseModel):
    """One subsystem (mailing list) an author has participated in."""

    list: str
    patches: int
    oldest_unix_ns: int | None = None
    newest_unix_ns: int | None = None
    oldest_utc: datetime | None = None
    newest_utc: datetime | None = None


class OwnTrailerStats(BaseModel):
    """Trailers visible on messages this author sent."""

    signed_off_by_present: int = 0
    fixes_issued: int = 0


class ReceivedTrailerStats(BaseModel):
    """Trailers OTHERS added to this author's patches in reply chains
    that made it into the indexed series version. Counts are
    patch-granularity: N-of-my-patches got at least one Reviewed-by,
    not total Reviewed-by entries across all reply chains.
    """

    reviewed_by: int = 0
    acked_by: int = 0
    tested_by: int = 0
    co_developed_by: int = 0
    reported_by: int = 0
    cc_stable: int = 0


class AuthorProfileResponse(BaseModel):
    """Profile for one `from_addr`, aggregated from the most recent N
    messages they authored. See `lore_author_profile`.

    Scope note: all counts are for messages THIS person AUTHORED. For
    "how many patches has this person REVIEWED" (i.e. appeared as
    Reviewed-by on someone else's patches), a reverse-trailer index
    is needed — tracked as future work.
    """

    addr_queried: str
    sampled: int = Field(
        description="How many rows were aggregated. Capped by the limit parameter."
    )
    authored_count: int = Field(
        default=0,
        description="Subset of `sampled` where the address was the From: (authored).",
    )
    mention_count: int = Field(
        default=0,
        description=(
            "Subset of `sampled` that came from the expanded scope "
            "(appearing in a trailer on someone else's patch). Always "
            "zero when `include_mentions=False`."
        ),
    )
    limit_hit: bool = Field(
        description=(
            "True when `sampled == limit`; the caller may be seeing a "
            "recent-only slice of a prolific author's history."
        )
    )
    oldest_unix_ns: int | None = None
    newest_unix_ns: int | None = None
    oldest_utc: datetime | None = None
    newest_utc: datetime | None = None
    patches_with_content: int
    cover_letters: int
    unique_subjects: int
    with_fixes_trailer: int
    own_trailers: OwnTrailerStats
    received_trailers: ReceivedTrailerStats
    subsystems: list[SubsystemBucket]
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class DeclaredMaintainerEntry(BaseModel):
    """One MAINTAINERS section that claims this path."""

    name: str
    status: str | None = None
    depth: int = Field(
        description=(
            "Pattern-specificity score — number of `/` in the F: glob "
            "that matched. Deeper = more specific; ties broken by "
            "declaration order."
        )
    )
    lists: list[str] = Field(default_factory=list)
    maintainers: list[str] = Field(default_factory=list)
    reviewers: list[str] = Field(default_factory=list)


class ObservedAddr(BaseModel):
    """One person who has actually been active on patches touching
    the queried path, by trailer kind."""

    email: str
    reviewed_by: int = 0
    acked_by: int = 0
    tested_by: int = 0
    signed_off_by: int = 0
    last_seen_unix_ns: int | None = None
    last_seen_utc: datetime | None = None


class MaintainerProfileResponse(BaseModel):
    """Declared (MAINTAINERS) vs. observed (lore trailers) view of
    one kernel path. See `lore_maintainer_profile`.
    """

    path_queried: str
    maintainers_available: bool = Field(
        description=(
            "False when the server has no MAINTAINERS snapshot loaded. "
            "In that case `declared` is empty; observed activity is "
            "still useful on its own."
        )
    )
    sampled_patches: int = Field(
        description=(
            "How many patches touching this path were aggregated from "
            "the observation window."
        )
    )
    declared: list[DeclaredMaintainerEntry]
    observed: list[ObservedAddr] = Field(
        description="Top-N observed reviewers/ackers/testers/signers."
    )
    stale_declared: list[str] = Field(
        description=(
            "Emails declared in MAINTAINERS (M: or R:) that had ZERO "
            "observed activity in the window — candidates for the "
            "subsystem maintainers to prune or refresh."
        )
    )
    active_unlisted: list[ObservedAddr] = Field(
        description=(
            "Observed addresses NOT in MAINTAINERS for this path. "
            "Ranked by reviews + acks. Heavy hitters here often "
            "should be promoted to R: or M:."
        )
    )
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


class SummarizeThreadResponse(BaseModel):
    """`lore_summarize_thread` — short prose summary of a conversation.

    `backend` tells the agent whether the summary came from the
    client's LLM (via `ctx.sample()`) or from a deterministic
    extractive fallback. Both paths are honest; the fallback is not
    a degradation, it is a different algorithm that uses no tokens.
    """

    root_message_id: str
    summary: str
    backend: str = Field(
        description='Either "sampled" (client LLM via ctx.sample) or "extractive" (deterministic fallback).',
    )
    message_count: int
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class ClassifyPatchResponse(BaseModel):
    """`lore_classify_patch` — {bugfix|feature|cleanup|doc|test|merge|revert|backport|security|unknown}."""

    message_id: str
    label: str
    confidence: float | None = Field(
        default=None,
        ge=0.0,
        le=1.0,
        description='Populated on the extractive backend via rule weights; None for "sampled".',
    )
    rationale: str
    backend: str
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


class ExplainReviewStatusResponse(BaseModel):
    """`lore_explain_review_status` — open reviewer concerns + trailers seen.

    `open_concerns` is a list of short sentences extracted or
    generated from replies. `trailers_seen` is the union of
    structured trailers (reviewed_by/acked_by/tested_by) across the
    whole thread — the agent can use it to tell "reviewed but
    not acked" states.
    """

    root_message_id: str
    open_concerns: list[str] = Field(default_factory=list)
    trailers_seen: dict[str, list[str]] = Field(default_factory=dict)
    backend: str
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"
