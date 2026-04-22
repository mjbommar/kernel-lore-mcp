"""Low-level retrieval primitives (Phase 7).

Six tools, each one well-defined query against one tier. Agents
stack these themselves rather than us inventing higher-order
workflows for every new question.

  lore_eq         — exact-equality scan over one structured column
  lore_in_list    — set-membership over one column
  lore_count      — count + distinct-authors + date range
  lore_substr_subject  — case-insensitive substring over subject_raw
  lore_substr_trailers — substring inside one named trailer column
  lore_regex      — DFA-only regex over subject / from / prose / patch

(`lore_diff` lives in `tools/diff.py`; `lore_substr_patch` is
already exposed as `lore_patch_search`.)
"""

from __future__ import annotations

import difflib
from datetime import UTC, datetime
from typing import Annotated

from pydantic import Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.cursor import decode_cursor, mint_cursor, query_hash
from kernel_lore_mcp.errors import LoreError, invalid_argument, unknown_enum
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.kwic import build_snippet
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import (
    CountResponse,
    DiffResponse,
    RowsResponse,
    Snippet,
)
from kernel_lore_mcp.reader_cache import get_reader
from kernel_lore_mcp.time_bounds import TIME_BOUND_DESCRIPTION, resolve_time_bounds
from kernel_lore_mcp.timeout import run_with_timeout

_EQ_FIELDS = {
    "message_id",
    "list",
    "from_addr",
    "in_reply_to",
    # "tid" — intentionally excluded. The tid side-table is computed
    # at ingest time for cover-letter propagation but is NOT joined
    # into metadata rows at read time (metadata writes tid=null at
    # src/metadata.rs:170). eq(field="tid") would always return
    # nothing. When the reader-side join ships, re-add it here.
    "commit_oid",
    "body_sha256",
    "subject_normalized",
    "touched_files",
    "touched_functions",
    "references",
    "subject_tags",
    "signed_off_by",
    "reviewed_by",
    "acked_by",
    "tested_by",
    "co_developed_by",
    "reported_by",
    "suggested_by",
    "helped_by",
    "assisted_by",
    "fixes",
    "link",
    "closes",
    "cc_stable",
}


_TRAILER_NAMES = {
    "fixes",
    "link",
    "closes",
    "cc-stable",
    "cc_stable",
    "signed-off-by",
    "signed_off_by",
    "reviewed-by",
    "reviewed_by",
    "acked-by",
    "acked_by",
    "tested-by",
    "tested_by",
    "co-developed-by",
    "co_developed_by",
    "reported-by",
    "reported_by",
    "suggested-by",
    "suggested_by",
    "helped-by",
    "helped_by",
    "assisted-by",
    "assisted_by",
    "co-authored-by",
}


_REGEX_FIELDS = {"subject", "subject_raw", "from", "from_addr", "body_prose", "prose", "patch"}
_HOSTED_REGEX_FIELDS = {"subject", "subject_raw", "from", "from_addr"}


def _from_ns(ns: int | None) -> datetime | None:
    if ns is None:
        return None
    return datetime.fromtimestamp(ns / 1_000_000_000, tz=UTC)


def _subject_snippet(row: dict, needle: str) -> Snippet | None:
    subject = row.get("subject_raw") or row.get("subject_normalized") or ""
    return build_snippet(subject, needle, case_insensitive=True)


def _trailer_snippet(row: dict, name: str, value_substring: str) -> Snippet | None:
    key = name.lower().replace("-", "_")
    values = row.get(key) or []
    for value in values:
        snippet = build_snippet(str(value), value_substring, case_insensitive=True)
        if snippet is not None:
            return snippet
    return None


def _rows_to_response(rows: list, *, tier: str, reader, snippet_for=None) -> RowsResponse:
    hits = [
        row_to_search_hit(
            r,
            tier_provenance=[tier],
            snippet=snippet_for(r) if snippet_for is not None else None,
        )
        for r in rows
    ]
    return RowsResponse(results=hits, total=len(hits), freshness=build_freshness(reader))


def _enforce_hosted_regex_posture(
    *,
    field: str,
    pattern: str,
    anchor_required: bool,
    list: str | None,
) -> None:
    settings = get_settings()
    if settings.mode != "hosted":
        return
    if list is None:
        raise LoreError(
            "hosted_restriction",
            "hosted `lore_regex` requires `list` so the server never runs a full-corpus regex scan.",
            echoed_input={"field": field, "list": list, "pattern": pattern[:80]},
            valid_example='{"field": "subject", "pattern": "ksmbd", "list": "linux-cifs"}',
        )
    if not anchor_required:
        raise LoreError(
            "hosted_restriction",
            "hosted `lore_regex` requires `anchor_required=true` to keep the candidate filter bounded.",
            echoed_input={"field": field, "list": list, "anchor_required": anchor_required},
            valid_example='{"field": "subject", "pattern": "ksmbd", "list": "linux-cifs", "anchor_required": true}',
        )
    if field not in _HOSTED_REGEX_FIELDS:
        raise LoreError(
            "hosted_restriction",
            "hosted `lore_regex` is limited to metadata fields (`subject` / `from_addr`). Prose and patch regex scans are local-only.",
            echoed_input={"field": field, "list": list},
            valid_example='{"field": "subject", "pattern": "ksmbd", "list": "linux-cifs"}',
        )


async def lore_eq(
    field: Annotated[
        str,
        Field(
            min_length=1,
            description=(
                "Structured column to match. Supported: "
                "message_id, list, from_addr, in_reply_to, commit_oid, "
                "body_sha256, subject_normalized, touched_files, "
                "touched_functions, references, subject_tags, signed_off_by, "
                "reviewed_by, acked_by, tested_by, co_developed_by, "
                "reported_by, fixes, link, closes, cc_stable."
            ),
        ),
    ],
    value: Annotated[str, Field(min_length=1, max_length=512)],
    since: Annotated[
        str | None, Field(description=f"Human-friendly lower bound. {TIME_BOUND_DESCRIPTION}")
    ] = None,
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    until: Annotated[
        str | None,
        Field(description=f"Human-friendly exclusive upper bound. {TIME_BOUND_DESCRIPTION}"),
    ] = None,
    until_unix_ns: Annotated[
        int | None, Field(description="Exclusive upper-bound (ns since epoch).")
    ] = None,
    list: Annotated[str | None, Field(description="Restrict to one mailing list.")] = None,
    limit: Annotated[int, Field(ge=1, le=500)] = 100,
) -> RowsResponse:
    """`WHERE field = value` exact-equality scan over one structured column.

    Cost: cheap — expected p95 50 ms (one-column metadata scan).
    """
    if field not in _EQ_FIELDS:
        raise unknown_enum(field_name="field", bad_value=field, valid=_EQ_FIELDS)

    reader = get_reader()
    resolved_since, resolved_until = resolve_time_bounds(
        since=since,
        since_unix_ns=since_unix_ns,
        until=until,
        until_unix_ns=until_unix_ns,
    )
    rows = await run_with_timeout(
        reader.eq, field, value, resolved_since, resolved_until, list, limit
    )
    return _rows_to_response(rows, tier="metadata", reader=reader)


async def lore_in_list(
    field: Annotated[str, Field(min_length=1, description="Same set as lore_eq.")],
    values: Annotated[list[str], Field(min_length=1, max_length=512)],
    since: Annotated[
        str | None, Field(description=f"Human-friendly lower bound. {TIME_BOUND_DESCRIPTION}")
    ] = None,
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    until: Annotated[
        str | None,
        Field(description=f"Human-friendly exclusive upper bound. {TIME_BOUND_DESCRIPTION}"),
    ] = None,
    until_unix_ns: Annotated[
        int | None, Field(description="Exclusive upper-bound (ns since epoch).")
    ] = None,
    list: Annotated[str | None, Field(description="Restrict to one mailing list.")] = None,
    limit: Annotated[int, Field(ge=1, le=500)] = 100,
) -> RowsResponse:
    """`WHERE field IN (values)` set-membership over one structured column.

    Cost: cheap — expected p95 50 ms.
    """
    if field not in _EQ_FIELDS:
        raise unknown_enum(field_name="field", bad_value=field, valid=_EQ_FIELDS)
    if not values:
        raise invalid_argument(
            name="values",
            reason="must be a non-empty list",
            value=values,
            example='["alice@example.com", "bob@example.com"]',
        )

    reader = get_reader()
    resolved_since, resolved_until = resolve_time_bounds(
        since=since,
        since_unix_ns=since_unix_ns,
        until=until,
        until_unix_ns=until_unix_ns,
    )
    rows = await run_with_timeout(
        reader.in_list,
        field,
        values,
        resolved_since,
        resolved_until,
        list,
        limit,
    )
    return _rows_to_response(rows, tier="metadata", reader=reader)


async def lore_count(
    field: Annotated[str, Field(min_length=1, description="Same set as lore_eq.")],
    value: Annotated[str, Field(min_length=1, max_length=512)],
    since: Annotated[
        str | None, Field(description=f"Human-friendly lower bound. {TIME_BOUND_DESCRIPTION}")
    ] = None,
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    until: Annotated[
        str | None,
        Field(description=f"Human-friendly exclusive upper bound. {TIME_BOUND_DESCRIPTION}"),
    ] = None,
    until_unix_ns: Annotated[
        int | None, Field(description="Exclusive upper-bound (ns since epoch).")
    ] = None,
    list: Annotated[str | None, Field(description="Restrict to one mailing list.")] = None,
) -> CountResponse:
    """Count + distinct-authors + date-range over the same predicate as lore_eq.

    Cheap relative to materializing rows; lets agents budget without
    pulling 10k rows just to know the size.

    Cost: cheap — expected p95 40 ms (aggregate only, no row materialization).
    """
    if field not in _EQ_FIELDS:
        raise unknown_enum(field_name="field", bad_value=field, valid=_EQ_FIELDS)

    reader = get_reader()
    resolved_since, resolved_until = resolve_time_bounds(
        since=since,
        since_unix_ns=since_unix_ns,
        until=until,
        until_unix_ns=until_unix_ns,
    )
    summary = await run_with_timeout(
        reader.count, field, value, resolved_since, resolved_until, list
    )
    return CountResponse(
        count=summary["count"],
        distinct_authors=summary["distinct_authors"],
        earliest_unix_ns=summary["earliest_unix_ns"],
        latest_unix_ns=summary["latest_unix_ns"],
        earliest_utc=_from_ns(summary["earliest_unix_ns"]),
        latest_utc=_from_ns(summary["latest_unix_ns"]),
        freshness=build_freshness(reader),
    )


async def lore_substr_subject(
    needle: Annotated[
        str,
        Field(
            min_length=1,
            max_length=512,
            description="Case-insensitive substring matched against `subject_raw`.",
        ),
    ],
    list: Annotated[str | None, Field(description="Restrict to one mailing list.")] = None,
    since: Annotated[
        str | None, Field(description=f"Human-friendly lower bound. {TIME_BOUND_DESCRIPTION}")
    ] = None,
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    until: Annotated[
        str | None,
        Field(description=f"Human-friendly exclusive upper bound. {TIME_BOUND_DESCRIPTION}"),
    ] = None,
    until_unix_ns: Annotated[
        int | None, Field(description="Exclusive upper-bound (ns since epoch).")
    ] = None,
    limit: Annotated[int, Field(ge=1, le=500)] = 100,
) -> RowsResponse:
    """Case-insensitive byte substring scan over `subject_raw`.

    Cost: cheap — expected p95 80 ms (metadata column scan, no trigram).
    """
    reader = get_reader()
    resolved_since, resolved_until = resolve_time_bounds(
        since=since,
        since_unix_ns=since_unix_ns,
        until=until,
        until_unix_ns=until_unix_ns,
    )
    rows = await run_with_timeout(
        reader.substr_subject, needle, list, resolved_since, resolved_until, limit
    )
    return _rows_to_response(
        rows,
        tier="metadata",
        reader=reader,
        snippet_for=lambda r: _subject_snippet(r, needle),
    )


async def lore_substr_trailers(
    name: Annotated[
        str,
        Field(
            min_length=1,
            description=(
                "Trailer kind (case-insensitive). One of: fixes, link, closes, "
                "cc-stable, signed-off-by, reviewed-by, acked-by, tested-by, "
                "co-developed-by, reported-by."
            ),
        ),
    ],
    value_substring: Annotated[
        str,
        Field(min_length=1, max_length=512, description="Case-insensitive substring."),
    ],
    list: Annotated[str | None, Field(description="Restrict to one mailing list.")] = None,
    since: Annotated[
        str | None, Field(description=f"Human-friendly lower bound. {TIME_BOUND_DESCRIPTION}")
    ] = None,
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    until: Annotated[
        str | None,
        Field(description=f"Human-friendly exclusive upper bound. {TIME_BOUND_DESCRIPTION}"),
    ] = None,
    until_unix_ns: Annotated[
        int | None, Field(description="Exclusive upper-bound (ns since epoch).")
    ] = None,
    limit: Annotated[int, Field(ge=1, le=500)] = 100,
) -> RowsResponse:
    """Substring scan inside one named trailer column.

    Cost: cheap — expected p95 80 ms.
    """
    if name.lower() not in _TRAILER_NAMES:
        raise unknown_enum(
            field_name="name",
            bad_value=name,
            valid=_TRAILER_NAMES,
            code="unknown_trailer_name",
        )

    reader = get_reader()
    resolved_since, resolved_until = resolve_time_bounds(
        since=since,
        since_unix_ns=since_unix_ns,
        until=until,
        until_unix_ns=until_unix_ns,
    )
    rows = await run_with_timeout(
        reader.substr_trailers,
        name,
        value_substring,
        list,
        resolved_since,
        resolved_until,
        limit,
    )
    return _rows_to_response(
        rows,
        tier="metadata",
        reader=reader,
        snippet_for=lambda r: _trailer_snippet(r, name, value_substring),
    )


async def lore_regex(
    field: Annotated[
        str,
        Field(
            min_length=1,
            description=("Field to scan. One of: subject, from_addr, body_prose, patch."),
        ),
    ],
    pattern: Annotated[
        str,
        Field(
            min_length=1,
            max_length=2048,
            description=(
                "DFA-only regex (no backrefs, no lookaround). "
                "Patterns that don't compile to a DFA are rejected."
            ),
        ),
    ],
    anchor_required: Annotated[
        bool,
        Field(
            description=(
                "When true (default), reject leading `.*` patterns to keep "
                "the trigram filter honest. Set false to scan unanchored."
            ),
        ),
    ] = True,
    list: Annotated[str | None, Field(description="Restrict to one mailing list.")] = None,
    since: Annotated[
        str | None, Field(description=f"Human-friendly lower bound. {TIME_BOUND_DESCRIPTION}")
    ] = None,
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    until: Annotated[
        str | None,
        Field(description=f"Human-friendly exclusive upper bound. {TIME_BOUND_DESCRIPTION}"),
    ] = None,
    until_unix_ns: Annotated[
        int | None, Field(description="Exclusive upper-bound (ns since epoch).")
    ] = None,
    limit: Annotated[int, Field(ge=1, le=200)] = 100,
    cursor: Annotated[
        str | None,
        Field(
            description=(
                "Opaque HMAC-signed pagination token. Pass a prior "
                "response's `next_cursor` to resume newest-first "
                "after the last returned hit. Bound to the "
                "(field, pattern, anchor_required, list, since, until) "
                "combination — changing any invalidates the cursor."
            ),
        ),
    ] = None,
) -> RowsResponse:
    """DFA-only regex scan over one of {subject, from_addr, body_prose, patch}.

    Cost: expensive — expected p95 1500 ms on prose/patch; 200 ms on subject/from.
    Prefer a substring or equality primitive first if you know the string literal.
    In hosted mode, only list-scoped metadata regexes are allowed.
    """
    if field not in _REGEX_FIELDS:
        raise unknown_enum(
            field_name="field",
            bad_value=field,
            valid=_REGEX_FIELDS,
            code="unknown_regex_field",
        )
    _enforce_hosted_regex_posture(
        field=field,
        pattern=pattern,
        anchor_required=anchor_required,
        list=list,
    )

    reader = get_reader()
    resolved_since, resolved_until = resolve_time_bounds(
        since=since,
        since_unix_ns=since_unix_ns,
        until=until,
        until_unix_ns=until_unix_ns,
    )

    q_hash = query_hash(
        "lore_regex",
        field,
        pattern,
        int(anchor_required),
        list or "",
        resolved_since or 0,
        resolved_until or 0,
    )
    resume = decode_cursor(cursor, expected_q_hash=q_hash, arg_name="cursor")

    fetch_budget = max(limit * 2 + 1, 32)
    rows = await run_with_timeout(
        reader.regex,
        field,
        pattern,
        anchor_required,
        list,
        resolved_since,
        resolved_until,
        fetch_budget,
    )

    if resume is not None:
        last_date, last_mid = resume
        kept: list[dict] = []
        for r in rows:
            date = float(r.get("date_unix_ns") or 0)
            mid = str(r.get("message_id") or "")
            if date < last_date or (date == last_date and mid > last_mid):
                kept.append(r)
        rows = kept

    total_available = len(rows)
    page = rows[:limit]
    tier = "metadata" if field in {"subject", "from_addr"} else "trigram"
    response = _rows_to_response(page, tier=tier, reader=reader)

    if page and total_available > limit:
        last = page[-1]
        response.next_cursor = mint_cursor(
            q_hash=q_hash,
            last_score=float(last.get("date_unix_ns") or 0),
            last_mid=str(last.get("message_id") or ""),
        )
    return response


async def lore_diff(
    a: Annotated[str, Field(min_length=1, max_length=512, description="First message-id.")],
    b: Annotated[str, Field(min_length=1, max_length=512, description="Second message-id.")],
    mode: Annotated[
        str,
        Field(description='View to diff: "patch", "prose", or "raw".'),
    ] = "patch",
) -> DiffResponse:
    """Generalized message-vs-message unified diff.

    Cost: moderate — expected p95 300 ms (two body fetches + difflib).
    """
    if a == b:
        raise invalid_argument(
            name="a",
            reason="a and b must be different message-ids",
            value=a,
            example='{"a": "m1@x", "b": "m2@x"}',
        )
    if mode not in {"patch", "prose", "raw"}:
        raise unknown_enum(
            field_name="mode",
            bad_value=mode,
            valid={"patch", "prose", "raw"},
            code="unknown_diff_mode",
        )

    reader = get_reader()
    result = await run_with_timeout(reader.diff, a, b, mode)
    diff_text = "".join(
        difflib.unified_diff(
            result["text_a"].splitlines(keepends=True),
            result["text_b"].splitlines(keepends=True),
            fromfile=f"a/{result['a']['message_id']} ({mode})",
            tofile=f"b/{result['b']['message_id']} ({mode})",
            n=3,
        )
    )
    return DiffResponse(
        a=row_to_search_hit(result["a"], tier_provenance=["metadata"]),
        b=row_to_search_hit(result["b"], tier_provenance=["metadata"]),
        mode=mode,
        diff=diff_text,
        freshness=build_freshness(reader),
    )
