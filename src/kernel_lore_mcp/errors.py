"""Structured error envelope for MCP tools.

The model sees the exception message as another prompt, so errors
are prompts. `LoreError` is a `ToolError` subclass that formats a
consistent three-part message:

    [code] what went wrong — why / what a valid input looks like.
    Echoed input: {field: 'stauts'}.
    Suggestion: did you mean 'status'?

This lets the agent self-correct (`did you mean`) instead of retrying
the exact same call. All error-raising paths in `tools/*.py` should
funnel through the module-level helpers so the shape stays uniform.

FastMCP sets `isError: true` on the tool result automatically when a
`ToolError` is raised; no manual flag is required. Transport-level
exceptions would terminate the call without feeding the message back
to the model — use `LoreError` instead so recovery works.
"""

from __future__ import annotations

import difflib
import json
from typing import Any

from fastmcp.exceptions import ToolError

_MAX_VALID_EXAMPLE = 24  # enum items listed in full before we abbreviate


class LoreError(ToolError):
    """Structured error. Subclass of ToolError so FastMCP sets isError."""

    def __init__(
        self,
        code: str,
        human_message: str,
        *,
        echoed_input: dict[str, Any] | None = None,
        valid_example: str | None = None,
        did_you_mean: str | None = None,
        retry_after_seconds: int | None = None,
    ) -> None:
        self.code = code
        self.human_message = human_message
        self.echoed_input = echoed_input or {}
        self.valid_example = valid_example
        self.did_you_mean = did_you_mean
        self.retry_after_seconds = retry_after_seconds
        super().__init__(self._format())

    def _format(self) -> str:
        parts = [f"[{self.code}] {self.human_message}"]
        if self.did_you_mean:
            parts.append(f"Did you mean '{self.did_you_mean}'?")
        if self.valid_example:
            parts.append(f"Example: {self.valid_example}")
        if self.echoed_input:
            parts.append(f"Echoed input: {json.dumps(self.echoed_input, sort_keys=True)}")
        if self.retry_after_seconds is not None:
            parts.append(f"Retry after {self.retry_after_seconds}s.")
        return " ".join(parts)


def unknown_enum(
    *,
    field_name: str,
    bad_value: str,
    valid: set[str] | list[str],
    code: str = "unknown_field_value",
) -> LoreError:
    """Raise-ready error for an unknown value from a closed enum.

    Adds a `did_you_mean` if the bad value is close enough to any valid
    option (difflib ratio ≥ 0.6). The list of valid options is surfaced
    in the message so the agent can retry without another round-trip.
    """
    valid_list = sorted(valid)
    match = difflib.get_close_matches(bad_value, valid_list, n=1, cutoff=0.6)
    suggestion = match[0] if match else None
    if len(valid_list) <= _MAX_VALID_EXAMPLE:
        example = ", ".join(valid_list)
    else:
        example = ", ".join(valid_list[:_MAX_VALID_EXAMPLE]) + ", ..."
    return LoreError(
        code,
        f"{field_name}={bad_value!r} is not one of the accepted values.",
        echoed_input={field_name: bad_value},
        valid_example=example,
        did_you_mean=suggestion,
    )


def not_found(*, what: str, message_id: str) -> LoreError:
    return LoreError(
        "not_found",
        f"{what} for message_id {message_id!r} not found in indexed corpus.",
        echoed_input={"message_id": message_id},
        valid_example="m1@example.com (any Message-ID already in the corpus)",
    )


def invalid_argument(
    *,
    name: str,
    reason: str,
    value: Any,
    example: str | None = None,
) -> LoreError:
    return LoreError(
        "invalid_argument",
        f"argument {name!r} rejected: {reason}",
        echoed_input={name: value},
        valid_example=example,
    )


def invalid_cursor(*, reason: str, cursor: str) -> LoreError:
    """Rejected pagination cursor.

    Until HMAC-validated paging lands (phase-5d), the only safe
    behavior is to reject any supplied cursor — the server never
    issues one, so any cursor the caller holds is either forged or
    stale.
    """
    preview = cursor[:48] + "…" if len(cursor) > 48 else cursor
    return LoreError(
        "invalid_cursor",
        f"pagination cursor rejected: {reason}",
        echoed_input={"cursor": preview},
        valid_example="omit the cursor field to request the first page",
    )


def query_too_long(*, name: str, length: int, limit: int) -> LoreError:
    """Request field exceeds the server's length cap.

    Kept separate from pydantic's raw ValidationError so agents get
    a consistent `[query_too_long]` shape with actionable guidance
    instead of a generic validation traceback.
    """
    return LoreError(
        "query_too_long",
        f"{name} is {length} characters; server cap is {limit}.",
        echoed_input={name: f"<{length}-char string>"},
        valid_example=f"truncate {name} to <= {limit} characters and retry",
    )


__all__ = [
    "LoreError",
    "invalid_argument",
    "invalid_cursor",
    "not_found",
    "query_too_long",
    "unknown_enum",
]
