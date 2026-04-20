"""lore_thread_state — is this thread alive? nacked? superseded? merged?

Returns one of:
  {merged, rfc, superseded, nacked, under_review, abandoned, unknown}

`merged` is the *authoritative* answer and only fires when the git
sidecar (`kernel-lore-build-git-sidecar`) has been built against at
least one upstream tree and the seed's normalized subject + author
emailmatch a commit within a date window. When the sidecar is
absent, `merged` is never emitted — the rest of the ladder returns
a lore-only verdict and the caveat documents the limitation. This
replaces the "we don't return `merged`" stub that shipped with v0.1.x.

Confidence tier on every verdict (`high` / `medium` / `low`) —
overconfidence on a noisy classifier misleads agents into acting
on dead threads. `low` is honest when the evidence is weak; the
caller can downgrade to `unknown` if they want strict gating.

Ladder (priority order):
  1. rfc          — subject_tags includes `RFC`. High confidence.
  2. superseded   — a later [PATCH vN+1 ...] with same normalized
                    subject / from_addr exists. High confidence.
  3. nacked       — NACK/NAK regex match in a reply body from a
                    non-author. Medium confidence (depends on quote
                    context — the word can appear in discussion).
  4. under_review — at least one Reviewed-by/Acked-by trailer on
                    the seed or its siblings, no supersede, activity
                    within 180 days. Medium.
  5. abandoned    — no non-author reply in >180 days, not RFC.
                    Low confidence — some patches sit then land.
  6. unknown      — default; emitted rather than fabricating a
                    verdict when evidence is ambiguous.
"""

from __future__ import annotations

import re
from datetime import UTC, datetime, timedelta
from typing import Annotated, Literal

from pydantic import BaseModel, Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import not_found
from kernel_lore_mcp.freshness import Freshness, build_freshness
from kernel_lore_mcp.timeout import run_with_timeout

_NACK_WORD = re.compile(r"(?:^|\s)(?:NACK|NAK)(?:\s|[:.!]|$)", re.IGNORECASE)
_REVIEW_WINDOW = timedelta(days=180)


class ThreadStateEvidence(BaseModel):
    message_id: str | None = None
    subject: str | None = None
    from_addr: str | None = None
    detail: str


class MergedEvidence(BaseModel):
    repo: str
    sha: str
    subject: str
    author_date_unix_ns: int


class ThreadStateResponse(BaseModel):
    message_id: str
    state: Literal[
        "merged",
        "rfc",
        "superseded",
        "nacked",
        "under_review",
        "abandoned",
        "unknown",
    ]
    confidence: Literal["high", "medium", "low"]
    evidence: list[ThreadStateEvidence]
    merged_in: list[MergedEvidence] = Field(
        default_factory=list,
        description=(
            "Authoritative git-sidecar matches for this thread's "
            "seed. When non-empty, the verdict is `merged` regardless "
            "of lore-side signals. Empty otherwise (and always empty "
            "on deployments without the git sidecar)."
        ),
    )
    superseded_by_message_id: str | None = None
    latest_activity_unix_ns: int | None = None
    latest_activity_utc: datetime | None = None
    backend: Literal["sidecar_authoritative", "lore_heuristic"] = Field(
        description=(
            "`sidecar_authoritative` when the git sidecar contributed "
            "the `merged` verdict; `lore_heuristic` otherwise."
        ),
    )
    caveat: str = Field(
        description=(
            "Honest note about what this classifier CAN and CAN'T "
            "detect. `merged` requires the git sidecar — when absent, "
            "the caveat documents that."
        )
    )
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


def _decode(body: bytes) -> str:
    try:
        return body.decode("utf-8")
    except UnicodeDecodeError:
        return body.decode("latin-1", errors="replace")


async def lore_thread_state(
    message_id: Annotated[
        str,
        Field(
            min_length=1,
            max_length=512,
            description="Any message-id in the thread (seed / cover-letter / patch).",
        ),
    ],
    nack_scan_limit: Annotated[
        int,
        Field(
            ge=5,
            le=200,
            description=(
                "How many non-author replies to scan for NACK regex. "
                "Default 20 covers most real threads without paying "
                "the fetch_body cost for long ones."
            ),
        ),
    ] = 20,
) -> ThreadStateResponse:
    """Classify a thread's state from lore signals only.

    Cost: moderate — expected p95 300 ms. Dominated by the NACK body
    scan (one fetch_body per non-author reply, capped by
    `nack_scan_limit`).
    """
    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)

    seed = await run_with_timeout(reader.fetch_message, message_id)
    if seed is None:
        raise not_found(what="thread seed", message_id=message_id)

    evidence: list[ThreadStateEvidence] = []

    # ---- git-sidecar `merged` check (authoritative) --------------
    # Runs before any lore-side heuristic: a patch that landed
    # upstream can't meaningfully be "abandoned" or "nacked" in
    # retrospect; the terminal state is `merged`. Only the exact
    # seed is looked up — if the seed is v1 and v2 was merged, the
    # superseded-by check downstream catches that case.
    merged_hits: list[MergedEvidence] = []
    sidecar_repos: list[str] = []
    try:
        repos = await run_with_timeout(
            _core.git_sidecar_repos, settings.data_dir
        )
        sidecar_repos = [r["repo"] for r in repos]
    except Exception:  # noqa: BLE001
        sidecar_repos = []

    if sidecar_repos:
        subject = (
            seed.get("subject_normalized") or seed.get("subject_raw") or ""
        )
        from_addr = (seed.get("from_addr") or "").lower()
        seed_date_ns = int(seed.get("date_unix_ns") or 0)
        if subject and from_addr and seed_date_ns > 0:
            # ±90 days around the seed's send date — wide enough to
            # absorb multi-version respins, tight enough to avoid
            # matching unrelated commits with the same author that
            # happen to share a short subject prefix.
            window_ns = 90 * 24 * 3600 * 10**9
            try:
                hits = await run_with_timeout(
                    _core.git_sidecar_find_by_subject_author,
                    settings.data_dir,
                    subject,
                    from_addr,
                    window_ns,
                    seed_date_ns,
                )
                merged_hits = [
                    MergedEvidence(
                        repo=h["repo"],
                        sha=h["sha"],
                        subject=h["subject"],
                        author_date_unix_ns=h["author_date_ns"],
                    )
                    for h in hits
                ]
            except Exception:  # noqa: BLE001
                merged_hits = []

    def _caveat(state: str) -> str:
        if merged_hits:
            return (
                f"authoritative via git sidecar — matched in "
                f"{', '.join(sorted({h.repo for h in merged_hits}))}"
            )
        if sidecar_repos:
            return (
                "sidecar searched but no merge match for seed "
                "(subject + author within ±90 days); falling back to "
                "lore-side signals"
            )
        return (
            "git sidecar not built on this server — `merged` cannot be "
            "detected; set KLMCP_GIT_MIRRORS and run "
            "`kernel-lore-build-git-sidecar` for authoritative answers"
        )

    backend: Literal["sidecar_authoritative", "lore_heuristic"] = (
        "sidecar_authoritative" if merged_hits else "lore_heuristic"
    )

    if merged_hits:
        evidence.append(
            ThreadStateEvidence(
                message_id=seed.get("message_id"),
                subject=seed.get("subject_raw"),
                from_addr=seed.get("from_addr"),
                detail=(
                    f"git sidecar: seed subject + author matched "
                    f"{len(merged_hits)} commit(s) "
                    f"in {', '.join(sorted({h.repo for h in merged_hits}))}"
                ),
            )
        )
        return ThreadStateResponse(
            message_id=seed["message_id"],
            state="merged",
            confidence="high",
            evidence=evidence,
            merged_in=merged_hits,
            latest_activity_unix_ns=merged_hits[0].author_date_unix_ns,
            latest_activity_utc=_utc(merged_hits[0].author_date_unix_ns),
            backend=backend,
            caveat=_caveat("merged"),
            freshness=build_freshness(reader),
        )

    caveat = _caveat("lore")

    # 1. RFC — metadata-only, deterministic.
    tags = {t.lower() for t in (seed.get("subject_tags") or [])}
    if "rfc" in tags:
        evidence.append(
            ThreadStateEvidence(
                message_id=seed.get("message_id"),
                subject=seed.get("subject_raw"),
                detail="subject carries [RFC] tag",
            )
        )
        return ThreadStateResponse(
            message_id=seed["message_id"],
            state="rfc",
            confidence="high",
            evidence=evidence,
            latest_activity_unix_ns=seed.get("date_unix_ns"),
            latest_activity_utc=_utc(seed.get("date_unix_ns")),
            backend=backend,
            caveat=caveat,
            freshness=build_freshness(reader),
        )

    # 2. Superseded — series_timeline + series_version comparison.
    siblings = await run_with_timeout(reader.series_timeline, seed["message_id"])
    seed_version = int(seed.get("series_version") or 0)
    max_sibling_version = seed_version
    newer_sibling: dict | None = None
    for sib in siblings:
        v = int(sib.get("series_version") or 0)
        if v > max_sibling_version and sib.get("message_id") != seed.get("message_id"):
            max_sibling_version = v
            newer_sibling = sib
    if newer_sibling is not None:
        evidence.append(
            ThreadStateEvidence(
                message_id=newer_sibling.get("message_id"),
                subject=newer_sibling.get("subject_raw"),
                from_addr=newer_sibling.get("from_addr"),
                detail=(
                    f"later series version exists "
                    f"(v{seed_version} → v{max_sibling_version})"
                ),
            )
        )
        return ThreadStateResponse(
            message_id=seed["message_id"],
            state="superseded",
            confidence="high",
            superseded_by_message_id=newer_sibling.get("message_id"),
            evidence=evidence,
            latest_activity_unix_ns=newer_sibling.get("date_unix_ns"),
            latest_activity_utc=_utc(newer_sibling.get("date_unix_ns")),
            backend=backend,
            caveat=caveat,
            freshness=build_freshness(reader),
        )

    # 3. Nacked — scan non-author reply bodies.
    thread_rows = await run_with_timeout(reader.thread, seed["message_id"], 200)
    non_author_replies = [
        r
        for r in thread_rows
        if r.get("from_addr") != seed.get("from_addr")
        and r.get("message_id") != seed.get("message_id")
    ]
    latest_activity_ns = max(
        (r.get("date_unix_ns") or 0 for r in thread_rows),
        default=seed.get("date_unix_ns") or 0,
    )

    for reply in non_author_replies[:nack_scan_limit]:
        body = await run_with_timeout(reader.fetch_body, reply["message_id"])
        if body is None:
            continue
        text = _decode(body)
        # Skip quoted blocks: naive but robust enough — we look for
        # NACK in lines that don't start with `>`.
        for line in text.splitlines():
            if line.lstrip().startswith(">"):
                continue
            if _NACK_WORD.search(line):
                evidence.append(
                    ThreadStateEvidence(
                        message_id=reply.get("message_id"),
                        subject=reply.get("subject_raw"),
                        from_addr=reply.get("from_addr"),
                        detail=f"NACK-shaped line in reply body: {line.strip()[:120]!r}",
                    )
                )
                return ThreadStateResponse(
                    message_id=seed["message_id"],
                    state="nacked",
                    confidence="medium",
                    evidence=evidence,
                    latest_activity_unix_ns=latest_activity_ns or None,
                    latest_activity_utc=_utc(latest_activity_ns or None),
                    caveat=caveat,
                    freshness=build_freshness(reader),
                )

    # 4. Under review — trailer signal + recent activity.
    has_review_trailer = any(
        r.get("reviewed_by") or r.get("acked_by") for r in thread_rows
    )
    now_ns = int(datetime.now(tz=UTC).timestamp() * 1_000_000_000)
    recent = bool(latest_activity_ns) and (
        now_ns - latest_activity_ns
        < int(_REVIEW_WINDOW.total_seconds() * 1_000_000_000)
    )
    if has_review_trailer and recent:
        evidence.append(
            ThreadStateEvidence(
                message_id=seed.get("message_id"),
                subject=seed.get("subject_raw"),
                detail="Reviewed-by/Acked-by trailer present + thread activity within 180 days",
            )
        )
        return ThreadStateResponse(
            message_id=seed["message_id"],
            state="under_review",
            confidence="medium",
            evidence=evidence,
            latest_activity_unix_ns=latest_activity_ns or None,
            latest_activity_utc=_utc(latest_activity_ns or None),
            backend=backend,
            caveat=caveat,
            freshness=build_freshness(reader),
        )

    # 5. Abandoned — long silence, no trailer signal.
    if latest_activity_ns and not recent and not has_review_trailer:
        days_silent = (now_ns - latest_activity_ns) / (86_400 * 1_000_000_000)
        evidence.append(
            ThreadStateEvidence(
                message_id=seed.get("message_id"),
                subject=seed.get("subject_raw"),
                detail=(
                    f"no trailer activity and >{int(days_silent)} days "
                    f"since last thread message"
                ),
            )
        )
        return ThreadStateResponse(
            message_id=seed["message_id"],
            state="abandoned",
            confidence="low",
            evidence=evidence,
            latest_activity_unix_ns=latest_activity_ns or None,
            latest_activity_utc=_utc(latest_activity_ns or None),
            backend=backend,
            caveat=caveat,
            freshness=build_freshness(reader),
        )

    # 6. Unknown — evidence insufficient.
    return ThreadStateResponse(
        message_id=seed["message_id"],
        state="unknown",
        confidence="low",
        evidence=evidence,
        latest_activity_unix_ns=latest_activity_ns or None,
        latest_activity_utc=_utc(latest_activity_ns or None),
        caveat=caveat,
        freshness=build_freshness(reader),
    )


def _utc(ns: int | None) -> datetime | None:
    if ns is None or ns == 0:
        return None
    return datetime.fromtimestamp(ns / 1_000_000_000, tz=UTC)
