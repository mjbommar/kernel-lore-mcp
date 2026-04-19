"""Adversarial regression suite for the MCP surface.

Every case here maps back to a finding from the manual test diary
(`scratch/mcp_test_diary.md`). If one of these ever regresses, a
production agent could loop, hang, or read from a forged cursor.
"""

from __future__ import annotations

import pytest
from fastmcp import Client
from fastmcp.exceptions import ToolError

from kernel_lore_mcp.server import build_server


# --- Finding 1 — hyphens in free text -----------------------------

@pytest.mark.asyncio
async def test_hyphenated_free_text_does_not_reject() -> None:
    """`use-after-free` used to trigger a tantivy phrase-query error
    because hyphens forced an implicit phrase into a positionless
    field. The router now rewrites `-` → ` ` before BM25 and surfaces
    `hyphen-split` in `default_applied`.
    """
    async with Client(build_server()) as c:
        res = await c.call_tool(
            "lore_search", {"query": "use-after-free cifs", "limit": 5}
        )
    defaults = res.data.default_applied
    assert "hyphen-split" in defaults


# --- Finding 2 — lore_thread on missing mid -----------------------

@pytest.mark.asyncio
async def test_thread_on_missing_mid_short_circuits() -> None:
    """A bogus mid used to fall through to thread_via_parquet_scan —
    a full Parquet sweep that took ~5 s on a 17.6M-row corpus and hit
    the request-timeout cap. Now: indexed fetch_message → None →
    structured `not_found`, not a wall-clock cliff.
    """
    async with Client(build_server()) as c:
        with pytest.raises(ToolError) as exc_info:
            await c.call_tool(
                "lore_thread",
                {"message_id": "<definitely-not-real@nowhere.invalid>"},
            )
    msg = str(exc_info.value)
    assert "not_found" in msg


# --- Finding 3 — forged cursor ------------------------------------

@pytest.mark.asyncio
async def test_forged_cursor_is_rejected() -> None:
    """Before the fix, the lore_search tool silently discarded any
    caller-supplied cursor (`_ = cursor  # TODO`). The MCP spec +
    CLAUDE.md require HMAC-signed cursors; acceptance of arbitrary
    garbage violated that contract.

    Current behavior until phase-5d pagination ships: any cursor
    supplied is rejected as `invalid_cursor`.
    """
    async with Client(build_server()) as c:
        with pytest.raises(ToolError) as exc_info:
            await c.call_tool(
                "lore_search",
                {
                    "query": "ksmbd",
                    "limit": 5,
                    "cursor": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                },
            )
    msg = str(exc_info.value)
    assert "invalid_cursor" in msg


# --- Finding 4 — query length cap ---------------------------------

@pytest.mark.asyncio
async def test_oversized_query_raises_structured_error() -> None:
    """Old behavior: pydantic's raw ValidationError for a query
    exceeding 512 chars. New: structured `query_too_long` at the new
    2048-char cap — consistent shape with other LoreErrors.
    """
    async with Client(build_server()) as c:
        with pytest.raises(ToolError) as exc_info:
            await c.call_tool("lore_search", {"query": "x" * 5_000, "limit": 5})
    msg = str(exc_info.value)
    assert "query_too_long" in msg


@pytest.mark.asyncio
async def test_query_under_cap_is_accepted() -> None:
    """The cap is a safety bound, not a narrow window. 1.5 KB of
    realistic text (stack-trace-sized) must still pass.
    """
    snippet = "ksmbd dacl check " * 80  # ~1360 chars
    assert 1_000 < len(snippet) < 2_048
    async with Client(build_server()) as c:
        # May return empty hits (depends on fixture corpus) but must
        # not raise a length error.
        res = await c.call_tool("lore_search", {"query": snippet, "limit": 5})
    assert res is not None


@pytest.mark.asyncio
async def test_empty_query_raises_structured_error() -> None:
    """Empty query used to give pydantic's min_length violation; now
    it rides the same invalid_argument pipeline as other input bugs.
    """
    async with Client(build_server()) as c:
        with pytest.raises(ToolError) as exc_info:
            await c.call_tool("lore_search", {"query": "", "limit": 5})
    msg = str(exc_info.value)
    assert "invalid_argument" in msg


# --- Pre-existing hardening — positions-off phrase reject ---------

@pytest.mark.asyncio
async def test_phrase_query_on_prose_still_rejected() -> None:
    """Phrase queries remain rejected — the index is WithFreqs by
    design. The error must direct the agent to `dfb:"..."` instead.
    """
    async with Client(build_server()) as c:
        with pytest.raises(ToolError) as exc_info:
            await c.call_tool(
                "lore_search", {"query": '"use after free"', "limit": 5}
            )
    msg = str(exc_info.value)
    assert "phrase" in msg.lower() or "positions" in msg.lower()
