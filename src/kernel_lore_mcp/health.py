"""Cheap deployment-health probes shared by status + backoff logic."""

from __future__ import annotations

import fcntl
import json
import os
import time
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

_ACTIVE_SYNC_STALE_AFTER_SECONDS = 30


def writer_lock_present(data_dir: Path) -> bool:
    """True when the writer lock is actively held by another process."""
    path = data_dir / "state" / "writer.lock"
    if not path.exists():
        return False

    fd = os.open(path, os.O_RDWR)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            return True
        fcntl.flock(fd, fcntl.LOCK_UN)
        return False
    finally:
        os.close(fd)


def read_sync_state(data_dir: Path) -> dict[str, Any] | None:
    """Read the live sync-state file if present.

    The Rust sync binary updates ``state/sync.json`` as it progresses.
    A stale leftover file should not be treated as active forever, so
    we require both a recent heartbeat and the writer lockfile.
    """
    path = data_dir / "state" / "sync.json"
    if not path.exists():
        return None

    writer_active = writer_lock_present(data_dir)
    try:
        payload = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return {
            "active": False,
            "stale": True,
            "writer_lock_present": writer_active,
            "note": "sync status file unreadable",
        }

    now = int(time.time())
    updated_unix_secs = _coerce_int(payload.get("updated_unix_secs"))
    started_unix_secs = _coerce_int(payload.get("started_unix_secs"))
    heartbeat_age_seconds = (
        max(0, now - updated_unix_secs) if updated_unix_secs is not None else None
    )
    active = bool(payload.get("active"))
    if heartbeat_age_seconds is not None:
        active = (
            active
            and writer_active
            and heartbeat_age_seconds <= _ACTIVE_SYNC_STALE_AFTER_SECONDS
        )
    else:
        active = active and writer_active

    out = dict(payload)
    out["writer_lock_present"] = writer_active
    out["heartbeat_age_seconds"] = heartbeat_age_seconds
    out["active"] = active
    out["stale"] = bool(payload.get("active")) and not active
    if started_unix_secs is not None:
        out["started_utc"] = _utc_from_unix(started_unix_secs)
    if updated_unix_secs is not None:
        out["updated_utc"] = _utc_from_unix(updated_unix_secs)
    return out


def suggest_retry_after_seconds(
    *,
    data_dir: Path,
    cost_class: str | None = None,
    timed_out: bool = False,
    query_wall_clock_ms: int | None = None,
) -> tuple[int, str | None]:
    """Return an in-band retry hint for MCP callers.

    MCP tool errors are surfaced inside the tool result rather than as
    transport-level HTTP 429 responses, so the useful hint lives in the
    structured error text. Keep it coarse and conservative; we want the
    caller to back off a bit, not pretend we know an exact queue ETA.
    """
    if cost_class == "cheap":
        retry_after = 2
    elif cost_class == "expensive":
        retry_after = 15
    else:
        retry_after = 5

    if timed_out:
        retry_after = max(retry_after, 5)
        if query_wall_clock_ms is not None:
            retry_after = max(retry_after, max(1, round(query_wall_clock_ms / 1000)))

    sync = read_sync_state(data_dir)
    if sync and sync.get("active"):
        stage = str(sync.get("stage") or "").strip()
        if stage == "bm25_commit":
            return max(retry_after, 20), "writer-heavy sync stage `bm25_commit` is active"
        if stage:
            return max(retry_after, 10), f"sync stage `{stage}` is active"
        return max(retry_after, 10), "a sync is active on this box"

    if writer_lock_present(data_dir):
        return max(retry_after, 10), "a writer lock is active on this box"

    return retry_after, None


def _coerce_int(value: object) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return int(value)
    return None


def _utc_from_unix(value: int) -> str:
    return datetime.fromtimestamp(value, tz=UTC).isoformat()
