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

import asyncio
import difflib
from datetime import UTC, datetime
from typing import Annotated

from fastmcp.exceptions import ToolError
from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import (
    CountResponse,
    DiffResponse,
    Freshness,
    RowsResponse,
)

_EQ_FIELDS = {
    "message_id",
    "list",
    "from_addr",
    "in_reply_to",
    "tid",
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
}


_REGEX_FIELDS = {"subject", "subject_raw", "from", "from_addr", "body_prose", "prose", "patch"}


def _from_ns(ns: int | None) -> datetime | None:
    if ns is None:
        return None
    return datetime.fromtimestamp(ns / 1_000_000_000, tz=UTC)


def _rows_to_response(rows: list, *, tier: str) -> RowsResponse:
    hits = [row_to_search_hit(r, tier_provenance=[tier]) for r in rows]
    return RowsResponse(results=hits, total=len(hits), freshness=Freshness())


async def lore_eq(
    field: Annotated[
        str,
        Field(
            min_length=1,
            description=(
                "Structured column to match. Supported: "
                "message_id, list, from_addr, in_reply_to, tid, commit_oid, "
                "body_sha256, subject_normalized, touched_files, "
                "touched_functions, references, subject_tags, signed_off_by, "
                "reviewed_by, acked_by, tested_by, co_developed_by, "
                "reported_by, fixes, link, closes, cc_stable."
            ),
        ),
    ],
    value: Annotated[str, Field(min_length=1, max_length=512)],
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    list: Annotated[str | None, Field(description="Restrict to one mailing list.")] = None,
    limit: Annotated[int, Field(ge=1, le=500)] = 100,
) -> RowsResponse:
    """`WHERE field = value` exact-equality scan over one structured column."""
    if field not in _EQ_FIELDS:
        raise ToolError(f"unknown field {field!r}; see tool description for the supported set")

    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    rows = await asyncio.to_thread(reader.eq, field, value, since_unix_ns, list, limit)
    return _rows_to_response(rows, tier="metadata")


async def lore_in_list(
    field: Annotated[str, Field(min_length=1, description="Same set as lore_eq.")],
    values: Annotated[list[str], Field(min_length=1, max_length=512)],
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    list: Annotated[str | None, Field(description="Restrict to one mailing list.")] = None,
    limit: Annotated[int, Field(ge=1, le=500)] = 100,
) -> RowsResponse:
    """`WHERE field IN (values)` set-membership over one structured column."""
    if field not in _EQ_FIELDS:
        raise ToolError(f"unknown field {field!r}")
    if not values:
        raise ToolError("values must be a non-empty list")

    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    rows = await asyncio.to_thread(reader.in_list, field, values, since_unix_ns, list, limit)
    return _rows_to_response(rows, tier="metadata")


async def lore_count(
    field: Annotated[str, Field(min_length=1, description="Same set as lore_eq.")],
    value: Annotated[str, Field(min_length=1, max_length=512)],
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    list: Annotated[str | None, Field(description="Restrict to one mailing list.")] = None,
) -> CountResponse:
    """Count + distinct-authors + date-range over the same predicate as lore_eq.

    Cheap relative to materializing rows; lets agents budget without
    pulling 10k rows just to know the size.
    """
    if field not in _EQ_FIELDS:
        raise ToolError(f"unknown field {field!r}")

    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    summary = await asyncio.to_thread(reader.count, field, value, since_unix_ns, list)
    return CountResponse(
        count=summary["count"],
        distinct_authors=summary["distinct_authors"],
        earliest_unix_ns=summary["earliest_unix_ns"],
        latest_unix_ns=summary["latest_unix_ns"],
        earliest_utc=_from_ns(summary["earliest_unix_ns"]),
        latest_utc=_from_ns(summary["latest_unix_ns"]),
        freshness=Freshness(),
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
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    limit: Annotated[int, Field(ge=1, le=500)] = 100,
) -> RowsResponse:
    """Case-insensitive byte substring scan over `subject_raw`."""
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    rows = await asyncio.to_thread(reader.substr_subject, needle, list, since_unix_ns, limit)
    return _rows_to_response(rows, tier="metadata")


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
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    limit: Annotated[int, Field(ge=1, le=500)] = 100,
) -> RowsResponse:
    """Substring scan inside one named trailer column."""
    if name.lower() not in _TRAILER_NAMES:
        raise ToolError(f"unknown trailer name {name!r}")

    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    rows = await asyncio.to_thread(
        reader.substr_trailers, name, value_substring, list, since_unix_ns, limit
    )
    return _rows_to_response(rows, tier="metadata")


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
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    limit: Annotated[int, Field(ge=1, le=200)] = 100,
) -> RowsResponse:
    """DFA-only regex scan over one of {subject, from_addr, body_prose, patch}."""
    if field not in _REGEX_FIELDS:
        raise ToolError(f"unknown regex field {field!r}")

    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    rows = await asyncio.to_thread(
        reader.regex,
        field,
        pattern,
        anchor_required,
        list,
        since_unix_ns,
        limit,
    )
    return _rows_to_response(
        rows, tier="metadata" if field in {"subject", "from_addr"} else "trigram"
    )


async def lore_diff(
    a: Annotated[str, Field(min_length=1, max_length=512, description="First message-id.")],
    b: Annotated[str, Field(min_length=1, max_length=512, description="Second message-id.")],
    mode: Annotated[
        str,
        Field(description='View to diff: "patch", "prose", or "raw".'),
    ] = "patch",
) -> DiffResponse:
    """Generalized message-vs-message unified diff."""
    if a == b:
        raise ToolError("a and b must be different message-ids")
    if mode not in {"patch", "prose", "raw"}:
        raise ToolError(f"unknown diff mode {mode!r}")

    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    result = await asyncio.to_thread(reader.diff, a, b, mode)
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
        freshness=Freshness(),
    )
