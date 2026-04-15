"""Phase 12 — sampling-backed tools with extractive fallback.

Three tools, each graceful when the client doesn't implement
sampling:

  lore_summarize_thread(message_id, max_sentences=5)
      LLM summary of a conversation; falls back to top-K
      sentence ranking on the thread prose.

  lore_classify_patch(message_id)
      Classify into {bugfix|feature|cleanup|doc|test|merge|
      revert|backport|security|unknown}; falls back to a rule
      scanner over subject_tags + trailers + patch_stats.

  lore_explain_review_status(message_id)
      Summarize open reviewer concerns + enumerate trailers
      seen; falls back to an extractive trailer scan.

All three ship `backend: "sampled"|"extractive"` on every response
so agents know which algorithm produced the text.
"""

from __future__ import annotations

import re
from typing import Annotated

from fastmcp import Context
from pydantic import Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import not_found
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.models import (
    ClassifyPatchResponse,
    ExplainReviewStatusResponse,
    SummarizeThreadResponse,
)
from kernel_lore_mcp.sampling import client_supports_sampling, sample_text
from kernel_lore_mcp.timeout import run_with_timeout
from kernel_lore_mcp.tools.message import _split_prose_patch

_SENTENCE_SPLIT_RE = re.compile(r"(?<=[.!?])\s+(?=[A-Z0-9])")
_SUMMARY_SYSTEM = (
    "You summarize Linux kernel mailing-list threads for a kernel "
    "developer or security researcher. Be terse; preserve maintainer "
    "names, subsystem filenames, and decision states. No speculation."
)
_CLASSIFY_SYSTEM = (
    "You classify a single Linux kernel patch into exactly one of: "
    "bugfix, feature, cleanup, doc, test, merge, revert, backport, "
    "security. Reply with one word only — the label."
)
_REVIEW_SYSTEM = (
    "You extract open reviewer concerns from a kernel mailing-list "
    "thread. Return at most 5 short bullet-point concerns, one line "
    "each, no prose. If nothing is open, say so."
)

_CLASSIFY_LABELS = {
    "bugfix",
    "feature",
    "cleanup",
    "doc",
    "test",
    "merge",
    "revert",
    "backport",
    "security",
}
_UNKNOWN_LABEL = "unknown"


def _decode(body: bytes) -> str:
    try:
        return body.decode("utf-8")
    except UnicodeDecodeError:
        return body.decode("latin-1", errors="replace")


def _score_sentence(sentence: str) -> float:
    """Lightweight relevance ranker for extractive summarization.

    Simple bag-of-signals: prefer sentences that mention kernel
    identifiers (snake_case), filenames, trailer-like structure, or
    named entities. Negative weight for reply-fluff ("thanks", "yes",
    etc.). Keeps the fallback deterministic.
    """
    s = sentence.strip()
    if len(s) < 10 or len(s) > 400:
        return 0.0
    score = 1.0
    if re.search(r"\b[a-z][a-z0-9_]*_[a-z0-9_]+\(?", s):
        score += 2.0
    if re.search(r"\b[A-Z][a-z]+(-[A-Z][a-z]+)+:", s):
        score += 1.5
    if re.search(r"\b(?:fixes|reviewed|acked|tested|signed-off)-?by:", s, re.IGNORECASE):
        score += 0.5
    if re.search(r"\bfs/|\bdrivers/|\bmm/|\bnet/|\bkernel/|\.c\b|\.h\b", s):
        score += 1.5
    lower = s.lower()
    if lower.startswith(("thanks", "thank you", "yes", "no", "+1", "nit", "lgtm")):
        score -= 1.5
    if "wrote:" in lower or lower.startswith(">"):
        score -= 2.0
    if re.search(r"\b(cve-\d{4}-\d{4,})\b", lower):
        score += 2.0
    return score


def _extractive_summary(text: str, max_sentences: int) -> str:
    # Drop quoted-reply lines before scoring.
    lines = [ln for ln in text.splitlines() if not ln.lstrip().startswith(">")]
    flat = " ".join(lines)
    sentences = [s.strip() for s in _SENTENCE_SPLIT_RE.split(flat) if s.strip()]
    ranked = sorted(
        enumerate(sentences),
        key=lambda ix: (-_score_sentence(ix[1]), ix[0]),
    )
    picked = sorted(ranked[:max_sentences], key=lambda ix: ix[0])
    return " ".join(s for _, s in picked)


def _rule_classify(row: dict, patch: str | None) -> tuple[str, float, str]:
    """Deterministic patch classifier."""
    tags = {t.lower() for t in row.get("subject_tags") or []}
    subj = (row.get("subject_raw") or row.get("subject_normalized") or "").lower()
    fixes = row.get("fixes") or []
    cc_stable = row.get("cc_stable") or []

    # Priority order matters — revert > security > bugfix > backport > ...
    # A `Fixes:` trailer is a stronger signal than `Cc: stable`; the
    # latter only hints that the fix may warrant backporting. A true
    # backport message lands in linux-stable / -next under someone
    # else's signoff chain, so we only classify `backport` when
    # there's NO `Fixes:` but there IS a stable signal (or the list
    # is explicitly one of the stable/next mailing lists).
    if "revert" in tags or subj.startswith("revert "):
        return "revert", 0.9, "`Revert` detected in subject / tags."
    if "security" in tags or re.search(r"\bcve-\d{4}-\d{4,}\b", subj):
        return "security", 0.85, "Security tag or CVE in subject."
    if fixes:
        return "bugfix", 0.8, f"Fixes: trailer present ({len(fixes)} culprit(s))."
    if cc_stable or "stable" in tags:
        return "backport", 0.7, f"Cc-stable detected ({len(cc_stable)} entries), no Fixes: trailer."
    if "doc" in tags or subj.startswith(("documentation:", "doc:", "docs:")):
        return "doc", 0.7, "Docs prefix / tag."
    if "test" in tags or subj.startswith(("selftests:", "test:", "kunit:")):
        return "test", 0.7, "Test-prefix subject."
    if "cleanup" in tags or " cleanup" in subj or " refactor" in subj:
        return "cleanup", 0.6, "Cleanup-prefix or tag."
    if "merge" in tags or subj.startswith("merge "):
        return "merge", 0.6, "Merge subject."
    if "rfc" in tags:
        return "feature", 0.55, "RFC tag indicates new functionality."
    if patch and re.search(r"\n\+\+\+ b/[^\n]+\n@@\s*-0,0\s+\+1", patch):
        return "feature", 0.55, "Adds a new file (new-file hunk detected)."
    return _UNKNOWN_LABEL, 0.0, "No strong signals from subject / trailers / patch shape."


_CONCERN_PATTERN = re.compile(
    r"^(?:[-*]\s+|\d+[.)]\s+)?(?:"
    r"(?:nit:|nits:|concern:|issue:|bug:|wrong:|broken:|questionable:"
    r"|shouldn.?t|wouldn.?t|doesn.?t|can.?t|won.?t|should\s+not|would\s+not)"
    r").*",
    re.IGNORECASE,
)


def _extract_concerns(thread_rows: list[dict], bodies: list[str]) -> list[str]:
    concerns: list[str] = []
    for body in bodies:
        for raw in body.splitlines():
            line = raw.strip()
            if not line or line.startswith(">"):
                continue
            if _CONCERN_PATTERN.match(line):
                concerns.append(line[:200])
    # Dedup + cap
    seen: set[str] = set()
    out: list[str] = []
    for c in concerns:
        key = c.lower()
        if key in seen:
            continue
        seen.add(key)
        out.append(c)
        if len(out) >= 5:
            break
    return out


def _aggregate_trailers(thread_rows: list[dict]) -> dict[str, list[str]]:
    out: dict[str, list[str]] = {}
    for name in ("reviewed_by", "acked_by", "tested_by", "signed_off_by"):
        seen: list[str] = []
        for r in thread_rows:
            for v in r.get(name) or []:
                if v not in seen:
                    seen.append(v)
        if seen:
            out[name] = seen
    return out


async def lore_summarize_thread(
    message_id: Annotated[str, Field(min_length=1, max_length=512)],
    max_sentences: Annotated[int, Field(ge=1, le=20)] = 5,
    ctx: Context | None = None,
) -> SummarizeThreadResponse:
    """Summarize a conversation thread in prose.

    Uses `ctx.sample()` when the client advertises sampling; falls
    back to a deterministic extractive top-K sentence ranker.

    Cost: expensive — expected p95 2000 ms on the sampled path (one
    round-trip to the client's LLM) / 120 ms on the extractive path.
    """
    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    rows = await run_with_timeout(reader.thread, message_id, 500)
    if not rows:
        raise not_found(what="thread seed", message_id=message_id)

    bodies: list[str] = []
    for r in rows:
        body = await run_with_timeout(reader.fetch_body, r["message_id"])
        if body is not None:
            bodies.append(_decode(body))
    joined = "\n\n".join(bodies)

    backend = "extractive"
    summary = _extractive_summary(joined, max_sentences)
    if ctx is not None and client_supports_sampling(ctx):
        try:
            prompt = (
                f"Summarize the following kernel mailing-list thread in at most "
                f"{max_sentences} sentences. Preserve filenames, identifiers, and "
                f"decision states. Omit pleasantries.\n\n{joined[:30_000]}"
            )
            summary = await sample_text(
                ctx,
                prompt,
                system_prompt=_SUMMARY_SYSTEM,
                max_tokens=512,
            )
            backend = "sampled"
        except (ValueError, RuntimeError) as exc:
            await ctx.warning(f"sampling failed, falling back to extractive: {exc}")

    return SummarizeThreadResponse(
        root_message_id=rows[0]["message_id"],
        summary=summary,
        backend=backend,
        message_count=len(rows),
        freshness=build_freshness(reader),
    )


async def lore_classify_patch(
    message_id: Annotated[str, Field(min_length=1, max_length=512)],
    ctx: Context | None = None,
) -> ClassifyPatchResponse:
    """Classify a patch into a fixed label set.

    Uses `ctx.sample()` when the client advertises sampling; falls
    back to a deterministic rule scanner over subject tags, trailers,
    and patch shape.

    Cost: moderate — expected p95 800 ms sampled / 60 ms extractive.
    """
    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    row = await run_with_timeout(reader.fetch_message, message_id)
    if row is None:
        raise not_found(what="message", message_id=message_id)

    body = await run_with_timeout(reader.fetch_body, message_id)
    prose: str | None = None
    patch: str | None = None
    if body is not None:
        prose, patch = _split_prose_patch(_decode(body))

    rule_label, rule_conf, rule_reason = _rule_classify(row, patch)
    backend = "extractive"
    label = rule_label
    confidence: float | None = rule_conf
    rationale = rule_reason

    if ctx is not None and client_supports_sampling(ctx):
        subject = row.get("subject_raw") or row.get("subject_normalized") or ""
        trailers = {
            name: row.get(name) or []
            for name in (
                "signed_off_by",
                "reviewed_by",
                "acked_by",
                "fixes",
                "cc_stable",
            )
        }
        patch_head = (patch or "")[:4000]
        prompt = (
            "Classify this patch. Reply with exactly one label.\n\n"
            f"Subject: {subject}\n"
            f"Trailers: {trailers}\n"
            f"Prose (first 1 KB): {(prose or '')[:1024]}\n"
            f"Patch head (first 4 KB): {patch_head}"
        )
        try:
            raw = await sample_text(
                ctx,
                prompt,
                system_prompt=_CLASSIFY_SYSTEM,
                max_tokens=16,
            )
            candidate = raw.strip().lower().split()[0].strip(" .,;:!?") if raw.strip() else ""
            if candidate in _CLASSIFY_LABELS:
                label = candidate
                confidence = None
                rationale = f"Classified by client LLM; rule fallback suggested {rule_label!r}."
                backend = "sampled"
            else:
                await ctx.warning(
                    f"sampled label {raw!r} not in the accepted set; keeping extractive"
                )
        except (ValueError, RuntimeError) as exc:
            await ctx.warning(f"sampling failed, falling back to rule classifier: {exc}")

    return ClassifyPatchResponse(
        message_id=message_id,
        label=label,
        confidence=confidence,
        rationale=rationale,
        backend=backend,
        freshness=build_freshness(reader),
    )


async def lore_explain_review_status(
    message_id: Annotated[str, Field(min_length=1, max_length=512)],
    ctx: Context | None = None,
) -> ExplainReviewStatusResponse:
    """Summarize open reviewer concerns + trailers seen in a thread.

    Uses `ctx.sample()` when available; falls back to a line-pattern
    extractor for concern-shaped sentences.

    Cost: moderate — expected p95 1500 ms sampled / 100 ms extractive.
    """
    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    rows = await run_with_timeout(reader.thread, message_id, 500)
    if not rows:
        raise not_found(what="thread seed", message_id=message_id)

    bodies: list[str] = []
    for r in rows:
        body = await run_with_timeout(reader.fetch_body, r["message_id"])
        if body is not None:
            bodies.append(_decode(body))

    concerns = _extract_concerns(rows, bodies)
    trailers = _aggregate_trailers(rows)
    backend = "extractive"

    if ctx is not None and client_supports_sampling(ctx):
        joined = "\n\n".join(bodies)[:30_000]
        prompt = (
            "From this kernel mailing-list thread, extract at most 5 short "
            "reviewer concerns that are still open. One bullet per line. If "
            "nothing is open, reply with 'no open concerns'.\n\n"
            f"{joined}"
        )
        try:
            raw = await sample_text(
                ctx,
                prompt,
                system_prompt=_REVIEW_SYSTEM,
                max_tokens=400,
            )
            if raw.strip():
                parsed = [line.lstrip("-* ").strip() for line in raw.splitlines() if line.strip()]
                if parsed and parsed[0].lower() != "no open concerns":
                    concerns = parsed[:5]
                elif parsed:
                    concerns = []
                backend = "sampled"
        except (ValueError, RuntimeError) as exc:
            await ctx.warning(f"sampling failed, falling back to extractive: {exc}")

    return ExplainReviewStatusResponse(
        root_message_id=rows[0]["message_id"],
        open_concerns=concerns,
        trailers_seen=trailers,
        backend=backend,
        freshness=build_freshness(reader),
    )


__all__ = [
    "lore_classify_patch",
    "lore_explain_review_status",
    "lore_summarize_thread",
]
