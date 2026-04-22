"""lore_fix_status — bug-centric fix correlation across threads."""

from __future__ import annotations

from collections import OrderedDict, defaultdict
from datetime import UTC, datetime
from typing import Annotated, Literal
from urllib.parse import parse_qs, unquote, urlsplit

from pydantic import BaseModel, Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import invalid_argument, not_found
from kernel_lore_mcp.freshness import Freshness, build_freshness
from kernel_lore_mcp.timeout import run_with_timeout

_SIDE_WINDOW_NS = 90 * 24 * 3600 * 1_000_000_000


def _utc(ns: int | None) -> datetime | None:
    if ns is None:
        return None
    return datetime.fromtimestamp(ns / 1_000_000_000, tz=UTC)


def _strip_angles(value: str) -> str:
    return value.strip().removeprefix("<").removesuffix(">")


def _extract_syzbot_hash_from_email(value: str | None) -> str | None:
    if not value:
        return None
    addr = value.strip().lower()
    local = addr.split("@", 1)[0]
    if not local.startswith("syzbot+"):
        return None
    maybe_hash = local.removeprefix("syzbot+")
    if 8 <= len(maybe_hash) <= 40 and all(c in "0123456789abcdef" for c in maybe_hash):
        return maybe_hash
    return None


def _extract_syzbot_hash_from_url(value: str) -> str | None:
    try:
        parsed = urlsplit(value.strip())
    except ValueError:
        return None
    params = parse_qs(parsed.query)
    for key in ("extid", "id"):
        vals = params.get(key) or []
        for raw in vals:
            candidate = raw.strip().lower()
            if 8 <= len(candidate) <= 40 and all(c in "0123456789abcdef" for c in candidate):
                return candidate
    return None


def _extract_lore_mid_from_url(value: str) -> str | None:
    try:
        parsed = urlsplit(value.strip())
    except ValueError:
        return None
    if not parsed.netloc.endswith("lore.kernel.org"):
        return None
    for segment in parsed.path.split("/"):
        if not segment:
            continue
        decoded = _strip_angles(unquote(segment))
        if "@" in decoded:
            return decoded
    return None


def _row_syzbot_hashes(row: dict) -> set[str]:
    out: set[str] = set()
    if hash_from_from := _extract_syzbot_hash_from_email(row.get("from_addr")):
        out.add(hash_from_from)
    for raw in row.get("reported_by") or []:
        if hash_from_reported := _extract_syzbot_hash_from_email(raw):
            out.add(hash_from_reported)
    for raw in (row.get("link") or []) + (row.get("closes") or []):
        if hash_from_url := _extract_syzbot_hash_from_url(raw):
            out.add(hash_from_url)
    return out


def _row_lore_links(row: dict) -> set[str]:
    out: set[str] = set()
    for raw in (row.get("link") or []) + (row.get("closes") or []):
        if mid := _extract_lore_mid_from_url(raw):
            out.add(mid)
    return out


class FixEvidence(BaseModel):
    trailer_kind: Literal["reported_by", "link", "closes", "thread_command"]
    match_kind: Literal["email", "syzbot_hash", "lore_mid", "dup_command"]
    matched_value: str
    message_id: str
    subject: str | None = None
    from_addr: str | None = None


class FixMergedEvidence(BaseModel):
    repo: str
    sha: str
    subject: str
    author_date_unix_ns: int
    author_date_utc: datetime | None = None


class FixCandidate(BaseModel):
    message_id: str
    subject: str | None = None
    from_addr: str | None = None
    date_unix_ns: int | None = None
    date_utc: datetime | None = None
    has_patch: bool
    is_cover_letter: bool
    matched_by: list[FixEvidence]
    merged_in: list[FixMergedEvidence] = Field(default_factory=list)


class FixStatusResponse(BaseModel):
    seed_message_id: str | None = None
    syzbot_hash_queried: str | None = None
    state: Literal["merged", "pending_patch", "no_fix_found", "duplicate_suspected", "unknown"]
    confidence: Literal["high", "medium", "low"]
    fix_candidates: list[FixCandidate]
    evidence: list[FixEvidence]
    backend: Literal["sidecar_authoritative", "lore_correlated"]
    caveat: str
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


async def lore_fix_status(
    message_id: Annotated[
        str | None,
        Field(
            description=(
                "Any seed message-id tied to the bug: the original syzbot report, "
                "a fix patch, or a reply linking the report."
            )
        ),
    ] = None,
    syzbot_hash: Annotated[
        str | None,
        Field(
            description=(
                "Optional syzbot extid/hash. When omitted, the tool tries to extract it "
                "from the seed message's from_addr / Reported-by / Link / Closes trailers."
            )
        ),
    ] = None,
    function_name: Annotated[
        str | None,
        Field(
            description=(
                "Optional corroborating function name. Reserved for future tree-aware checks; "
                "currently advisory only."
            )
        ),
    ] = None,
    vulnerable_file: Annotated[
        str | None,
        Field(
            description=(
                "Optional corroborating path. Reserved for future tree-aware checks; currently "
                "advisory only."
            )
        ),
    ] = None,
    candidate_limit: Annotated[
        int,
        Field(
            ge=1,
            le=200,
            description="Upper bound on related fix messages to inspect before deduplication.",
        ),
    ] = 50,
) -> FixStatusResponse:
    """Correlate a bug report to candidate fixes across separate threads.

    Cost: moderate — expected p95 300 ms without sidecar, ~700 ms with
    sidecar lookups for a handful of candidate patches.
    """
    del function_name, vulnerable_file

    if message_id is None and syzbot_hash is None:
        raise invalid_argument(
            name="message_id|syzbot_hash",
            reason="at least one of `message_id` or `syzbot_hash` is required",
            value={"message_id": message_id, "syzbot_hash": syzbot_hash},
            example='{"message_id": "<report@lore>"} or {"syzbot_hash": "ac3c79181f6aecc5120c"}',
        )

    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    seed: dict | None = None
    if message_id is not None:
        seed = await run_with_timeout(reader.fetch_message, message_id)
        if seed is None and syzbot_hash is None:
            raise not_found(what="fix-status seed", message_id=message_id)

    seed_mid = _strip_angles(seed["message_id"]) if seed is not None else None
    syzbot_hash_q = syzbot_hash.strip().lower() if syzbot_hash else None
    if seed is not None and syzbot_hash_q is None:
        hashes = sorted(_row_syzbot_hashes(seed))
        if hashes:
            syzbot_hash_q = hashes[0]

    sidecar_repos = await run_with_timeout(_core.git_sidecar_repos, settings.data_dir)
    candidate_rows: dict[str, dict] = OrderedDict()
    evidence_by_mid: dict[str, list[FixEvidence]] = defaultdict(list)

    async def _collect(kind: str, match_kind: str, value: str) -> None:
        rows = await run_with_timeout(
            reader.trailer_ref_lookup,
            kind,
            match_kind,
            value,
            None,
            None,
            None,
            candidate_limit,
        )
        for row in rows:
            mid = row["message_id"]
            candidate_rows.setdefault(mid, row)
            evidence_by_mid[mid].append(
                FixEvidence(
                    trailer_kind=kind,  # type: ignore[arg-type]
                    match_kind=match_kind,  # type: ignore[arg-type]
                    matched_value=value,
                    message_id=mid,
                    subject=row.get("subject_raw"),
                    from_addr=row.get("from_addr"),
                )
            )

    if syzbot_hash_q is not None:
        await _collect(
            "reported_by",
            "email",
            f"syzbot+{syzbot_hash_q}@syzkaller.appspotmail.com",
        )
        await _collect("reported_by", "syzbot_hash", syzbot_hash_q)
        await _collect("link", "syzbot_hash", syzbot_hash_q)
        await _collect("closes", "syzbot_hash", syzbot_hash_q)

    if seed_mid is not None:
        await _collect("link", "lore_mid", seed_mid)

    if seed is not None and seed_mid is not None:
        candidate_rows.pop(seed_mid, None)
        evidence_by_mid.pop(seed_mid, None)

    duplicate_evidence: list[FixEvidence] = []
    if seed is not None:
        thread_rows = await run_with_timeout(reader.thread, seed["message_id"], 50)
        for row in thread_rows:
            body = await run_with_timeout(reader.fetch_body, row["message_id"])
            if body is None:
                continue
            if b"#syz dup:" in body.lower():
                duplicate_evidence.append(
                    FixEvidence(
                        trailer_kind="thread_command",
                        match_kind="dup_command",
                        matched_value="#syz dup",
                        message_id=row["message_id"],
                        subject=row.get("subject_raw"),
                        from_addr=row.get("from_addr"),
                    )
                )
                break

    fix_candidates: list[FixCandidate] = []
    any_merged = False
    for mid, row in candidate_rows.items():
        subject = row.get("subject_normalized") or row.get("subject_raw") or ""
        from_addr = (row.get("from_addr") or "").lower()
        date_ns = int(row.get("date_unix_ns") or 0)
        merged_in: list[FixMergedEvidence] = []
        if sidecar_repos and subject and from_addr and date_ns > 0:
            hits = await run_with_timeout(
                _core.git_sidecar_find_by_subject_author,
                settings.data_dir,
                subject,
                from_addr,
                _SIDE_WINDOW_NS,
                date_ns,
            )
            dedup: dict[tuple[str, str], FixMergedEvidence] = {}
            for hit in hits:
                key = (hit["repo"], hit["sha"])
                dedup[key] = FixMergedEvidence(
                    repo=hit["repo"],
                    sha=hit["sha"],
                    subject=hit["subject"],
                    author_date_unix_ns=hit["author_date_ns"],
                    author_date_utc=_utc(hit["author_date_ns"]),
                )
            merged_in = list(dedup.values())
        any_merged = any_merged or bool(merged_in)
        fix_candidates.append(
            FixCandidate(
                message_id=mid,
                subject=row.get("subject_raw"),
                from_addr=row.get("from_addr"),
                date_unix_ns=row.get("date_unix_ns"),
                date_utc=_utc(row.get("date_unix_ns")),
                has_patch=bool(row.get("has_patch")),
                is_cover_letter=bool(row.get("is_cover_letter")),
                matched_by=evidence_by_mid[mid],
                merged_in=merged_in,
            )
        )

    fix_candidates.sort(
        key=lambda c: (
            0 if c.merged_in else 1,
            0 if c.has_patch else 1,
            -(c.date_unix_ns or 0),
        )
    )
    flat_evidence = [ev for evs in evidence_by_mid.values() for ev in evs]

    if any_merged:
        state: Literal[
            "merged", "pending_patch", "no_fix_found", "duplicate_suspected", "unknown"
        ] = "merged"
        confidence: Literal["high", "medium", "low"] = "high"
    elif any(c.has_patch or c.is_cover_letter for c in fix_candidates):
        state = "pending_patch"
        confidence = "medium"
    elif duplicate_evidence:
        state = "duplicate_suspected"
        confidence = "medium"
    elif syzbot_hash_q is not None or seed_mid is not None:
        state = "no_fix_found"
        confidence = "low"
    else:
        state = "unknown"
        confidence = "low"

    backend: Literal["sidecar_authoritative", "lore_correlated"] = (
        "sidecar_authoritative" if any_merged else "lore_correlated"
    )
    if any_merged:
        caveat = "merged verdict is authoritative via the git sidecar; correlation into candidate lore threads used structured trailer links."
    elif sidecar_repos:
        caveat = "candidate fixes were correlated from lore trailers; no sidecar merge match was found for the candidate patch subjects/authors."
    else:
        caveat = "git sidecar not built on this server; pending/no-fix states are lore-correlated only and may miss off-list or already-landed fixes."

    return FixStatusResponse(
        seed_message_id=seed_mid,
        syzbot_hash_queried=syzbot_hash_q,
        state=state,
        confidence=confidence,
        fix_candidates=fix_candidates,
        evidence=flat_evidence + duplicate_evidence,
        backend=backend,
        caveat=caveat,
        freshness=build_freshness(reader),
    )


__all__ = ["lore_fix_status"]
