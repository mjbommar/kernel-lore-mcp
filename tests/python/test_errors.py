"""Sprint 0 / CW-D — LoreError envelope + did-you-mean recovery.

The model sees an error as another prompt. A good error answers three
questions: what happened, why, how to fix. `unknown_enum` adds a
`did_you_mean` hint based on difflib ratio ≥ 0.6, which lets the
agent self-correct on typos without a human loop.
"""

from __future__ import annotations

import pytest
from fastmcp import Client
from fastmcp.exceptions import ToolError

from kernel_lore_mcp.errors import LoreError, invalid_argument, not_found, unknown_enum
from kernel_lore_mcp.server import build_server


def test_lore_error_is_toolerror_subclass() -> None:
    # FastMCP's `isError: true` contract keys on ToolError; our subclass
    # must not break that invariant.
    err = LoreError("x", "y")
    assert isinstance(err, ToolError)


def test_unknown_enum_suggests_typo_fix() -> None:
    err = unknown_enum(
        field_name="field",
        bad_value="stauts",
        valid={"status", "from_addr", "list"},
    )
    assert err.did_you_mean == "status"
    assert "Did you mean 'status'" in str(err)
    assert "unknown_field_value" in str(err)
    assert "stauts" in str(err)


def test_unknown_enum_no_suggestion_for_unrelated_value() -> None:
    err = unknown_enum(
        field_name="field",
        bad_value="completely-unrelated",
        valid={"status", "from_addr", "list"},
    )
    assert err.did_you_mean is None
    assert "Did you mean" not in str(err)


def test_unknown_enum_lists_valid_set_in_message() -> None:
    valid = {"alpha", "beta", "gamma"}
    err = unknown_enum(field_name="field", bad_value="x", valid=valid)
    # Valid options should be discoverable from the error message.
    for v in valid:
        assert v in str(err)


def test_not_found_echoes_message_id() -> None:
    err = not_found(what="message", message_id="m-missing@x")
    assert "m-missing@x" in str(err)
    assert "not_found" in str(err)


def test_invalid_argument_includes_reason_and_example() -> None:
    err = invalid_argument(
        name="a",
        reason="must differ from b",
        value="m1@x",
        example='{"a": "m1@x", "b": "m2@x"}',
    )
    assert "invalid_argument" in str(err)
    assert "must differ from b" in str(err)
    assert "Example:" in str(err)


def test_lore_error_formats_retry_after() -> None:
    err = LoreError("rate_limited", "busy", retry_after_seconds=7)
    assert "Retry after 7s." in str(err)


@pytest.mark.asyncio
async def test_live_tool_error_carries_structured_code() -> None:
    """End-to-end: a bad field name produces a LoreError, and the
    model sees the error code + did-you-mean in the message.
    """
    async with Client(build_server()) as c:
        with pytest.raises(ToolError) as exc_info:
            await c.call_tool(
                "lore_eq",
                {"field": "from_add", "value": "anyone@x"},  # typo of 'from_addr'
            )
    msg = str(exc_info.value)
    assert "unknown_field_value" in msg
    assert "from_addr" in msg  # did-you-mean
