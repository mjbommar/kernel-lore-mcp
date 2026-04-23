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
DEFAULT_TIER_NAMES = ("over", "bm25", "trigram", "tid", "path_vocab")
_SYNC_SHED_STAGES = {
    "ingest",
    "bm25_commit",
    "tid_rebuild",
    "path_vocab_rebuild",
    "generation_bump",
    "save_manifest",
}


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


def read_generation(data_dir: Path) -> tuple[int, datetime | None]:
    """Read the corpus generation counter + its mtime.

    `generation` is the raw-corpus freshness signal used by `/status`
    and tool freshness envelopes. A missing file means no ingest has
    committed against this data_dir yet.
    """
    path = data_dir / "state" / "generation"
    if not path.exists():
        return (0, None)
    try:
        generation = int(path.read_text().strip())
    except (OSError, ValueError):
        generation = 0
    try:
        mtime = datetime.fromtimestamp(path.stat().st_mtime, tz=UTC)
    except OSError:
        mtime = None
    return (generation, mtime)


def read_tier_generations(
    data_dir: Path,
    tier_names: tuple[str, ...] = DEFAULT_TIER_NAMES,
) -> dict[str, int | None]:
    """Read per-tier generation markers from `<data_dir>/state/`.

    Missing or unreadable markers collapse to `None`, which callers
    interpret as the legacy / not-yet-written state rather than as a
    hard error.
    """
    state_dir = data_dir / "state"
    out: dict[str, int | None] = {}
    for tier in tier_names:
        path = state_dir / f"{tier}.generation"
        if not path.exists():
            out[tier] = None
            continue
        try:
            out[tier] = int(path.read_text().strip())
        except (OSError, ValueError):
            out[tier] = None
    return out


def tier_status(marker_generation: int | None, corpus_generation: int) -> str:
    if marker_generation is None:
        return "marker absent"
    if marker_generation == corpus_generation:
        return "in sync"
    if marker_generation < corpus_generation:
        return f"behind by {corpus_generation - marker_generation}"
    return f"ahead by {marker_generation - corpus_generation}"


def read_tier_statuses(
    data_dir: Path,
    tier_names: tuple[str, ...] = DEFAULT_TIER_NAMES,
) -> dict[str, str]:
    corpus_generation, _ = read_generation(data_dir)
    tiers = read_tier_generations(data_dir, tier_names)
    return {tier: tier_status(tiers[tier], corpus_generation) for tier in tier_names}


def tier_generation_in_sync(data_dir: Path, tier: str) -> bool | None:
    """Return whether one tier marker is current against the corpus.

    `None` means the marker file is absent, which we preserve as a
    distinct legacy / unknown state instead of conflating it with a
    hard stale result.
    """
    corpus_generation, _ = read_generation(data_dir)
    marker_generation = read_tier_generations(data_dir, (tier,)).get(tier)
    if marker_generation is None:
        return None
    return marker_generation >= corpus_generation


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
            active and writer_active and heartbeat_age_seconds <= _ACTIVE_SYNC_STALE_AFTER_SECONDS
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


def should_shed_tool_call(*, data_dir: Path, cost_class: str) -> tuple[bool, str | None]:
    """Whether the server should fast-reject this tool class.

    Cheap indexed lookups stay available as long as the process is up.
    Moderate and expensive tools shed aggressively while a writer-heavy
    sync stage is active so a serving box does not spend its remaining
    headroom on work we already expect to be slow or likely to timeout.
    """
    if cost_class == "cheap":
        return False, None

    sync = read_sync_state(data_dir)
    if sync and sync.get("active"):
        stage = str(sync.get("stage") or "").strip()
        if stage in _SYNC_SHED_STAGES:
            return True, f"sync stage `{stage}` is reserving capacity"

    return False, None


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
