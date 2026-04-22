"""Shared parsing for human-friendly time bounds on MCP tools."""

from __future__ import annotations

import re
from datetime import UTC, date, datetime, timedelta

from kernel_lore_mcp.errors import invalid_argument

TIME_BOUND_DESCRIPTION = (
    "ISO date/time, raw nanoseconds since epoch, or a relative window like `90d` / `6mo`."
)

_RELATIVE_RE = re.compile(r"^(?P<count>\d+)(?P<unit>mo|y|w|d|h|s)$", re.IGNORECASE)


def parse_time_bound(
    *,
    name: str,
    value: str,
    now: datetime | None = None,
) -> int:
    raw = value.strip()
    if not raw:
        raise invalid_argument(
            name=name,
            reason="time bound must be non-empty",
            value=value,
            example="2026-01-15 or 90d",
        )
    if raw.isdigit():
        return int(raw)

    match = _RELATIVE_RE.fullmatch(raw)
    if match:
        count = int(match.group("count"))
        unit = match.group("unit").lower()
        now_dt = now or datetime.now(tz=UTC)
        if unit == "s":
            delta = timedelta(seconds=count)
        elif unit == "h":
            delta = timedelta(hours=count)
        elif unit == "d":
            delta = timedelta(days=count)
        elif unit == "w":
            delta = timedelta(weeks=count)
        elif unit == "mo":
            delta = timedelta(days=30 * count)
        else:
            delta = timedelta(days=365 * count)
        return int((now_dt - delta).timestamp() * 1_000_000_000)

    try:
        if len(raw) == 10 and raw.count("-") == 2:
            dt = datetime.combine(date.fromisoformat(raw), datetime.min.time(), tzinfo=UTC)
        else:
            dt = datetime.fromisoformat(raw.replace("Z", "+00:00"))
            if dt.tzinfo is None:
                dt = dt.replace(tzinfo=UTC)
            else:
                dt = dt.astimezone(UTC)
    except ValueError as exc:
        raise invalid_argument(
            name=name,
            reason=(
                "time bound must be ISO-8601 (`2026-01-15`, `2026-01-15T00:00:00Z`), "
                "raw nanoseconds, or a relative window like `90d` / `6mo`"
            ),
            value=value,
            example="2026-01-15 or 90d",
        ) from exc
    return int(dt.timestamp() * 1_000_000_000)


def resolve_time_bounds(
    *,
    since: str | None = None,
    since_unix_ns: int | None = None,
    until: str | None = None,
    until_unix_ns: int | None = None,
) -> tuple[int | None, int | None]:
    if since is not None and since_unix_ns is not None:
        raise invalid_argument(
            name="since",
            reason="pass only one of `since` or `since_unix_ns`",
            value={"since": since, "since_unix_ns": since_unix_ns},
            example='{"since": "2026-01-15"}',
        )
    if until is not None and until_unix_ns is not None:
        raise invalid_argument(
            name="until",
            reason="pass only one of `until` or `until_unix_ns`",
            value={"until": until, "until_unix_ns": until_unix_ns},
            example='{"until": "2026-04-01"}',
        )

    resolved_since = since_unix_ns
    resolved_until = until_unix_ns
    if since is not None:
        resolved_since = parse_time_bound(name="since", value=since)
    if until is not None:
        resolved_until = parse_time_bound(name="until", value=until)

    if (
        resolved_since is not None
        and resolved_until is not None
        and resolved_since >= resolved_until
    ):
        raise invalid_argument(
            name="window",
            reason="since must be less than until",
            value={"since": resolved_since, "until": resolved_until},
            example='{"since": "2026-01-01", "until": "2026-02-01"}',
        )
    return resolved_since, resolved_until


__all__ = ["TIME_BOUND_DESCRIPTION", "parse_time_bound", "resolve_time_bounds"]
