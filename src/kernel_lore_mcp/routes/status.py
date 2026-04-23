"""`/status` endpoint — exposed via `@mcp.custom_route`.

Returns enough state for LLM callers (and external monitoring) to
calibrate freshness without calling any MCP tool:

  * `generation` — monotonic counter bumped on each ingest commit.
  * `last_ingest_utc` — mtime of the generation file.
  * `per_list[<list>]` — shard oids we've ingested, and the newest
    message date per list from the metadata tier.
  * `blind_spots_ref` — canonical pointer to the coverage resource.

Cached for 30 seconds to avoid hammering the filesystem + metadata
Parquet scan under load; invalidation is simply time-based since
ingest bumps the generation file under a lockfile.
"""

from __future__ import annotations

import time
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from starlette.requests import Request
from starlette.responses import JSONResponse

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.health import (
    read_generation,
    read_sync_state,
    read_tier_generations,
    read_tier_statuses,
    writer_lock_present,
)

# Cache keyed by data_dir so multi-config / test embedding doesn't
# cross-contaminate. TTL from settings.freshness_cache_ttl_seconds.
_cache: dict[str, tuple[float, dict[str, Any]]] = {}
def _per_list_shards(data_dir: Path) -> dict[str, list[dict[str, str]]]:
    """Enumerate `<data_dir>/state/shards/<list>/<shard>.oid` files."""
    shards_root = data_dir / "state" / "shards"
    if not shards_root.exists():
        return {}
    out: dict[str, list[dict[str, str]]] = {}
    for list_dir in sorted(shards_root.iterdir()):
        if not list_dir.is_dir():
            continue
        rows: list[dict[str, str]] = []
        for oid_file in sorted(list_dir.glob("*.oid")):
            rows.append(
                {
                    "shard": oid_file.stem,
                    "head_oid": oid_file.read_text().strip(),
                }
            )
        if rows:
            out[list_dir.name] = rows
    return out


def _build_status(settings: Settings, *, include_per_list: bool) -> dict[str, Any]:
    data_dir = settings.data_dir
    generation, last_ingest = read_generation(data_dir)
    tier_generations = read_tier_generations(data_dir)
    tier_status = read_tier_statuses(data_dir)
    writer_lock = writer_lock_present(data_dir)
    sync = read_sync_state(data_dir)

    interval = settings.grokmirror_interval_seconds
    last_ingest_age_seconds: int | None = None
    freshness_ok: bool | None = None
    if last_ingest is not None:
        last_ingest_age_seconds = max(
            0,
            int((datetime.now(tz=UTC) - last_ingest).total_seconds()),
        )
        # 3x the configured interval is the "we should alert" threshold
        # that pairs with the Prometheus gauge.
        freshness_ok = last_ingest_age_seconds < 3 * interval

    body = {
        "service": "kernel-lore-mcp",
        "version": _native_version(),
        "generation": generation,
        "last_ingest_utc": last_ingest.isoformat() if last_ingest else None,
        "last_ingest_age_seconds": last_ingest_age_seconds,
        "configured_interval_seconds": interval,
        "freshness_ok": freshness_ok,
        "tier_generations": tier_generations,
        "tier_status": tier_status,
        "writer_lock_present": writer_lock,
        "sync_active": bool(sync and sync.get("active")),
        "sync": sync,
        "per_list_omitted": not include_per_list,
        "capabilities": capabilities(data_dir),
        "blind_spots_ref": "blind-spots://coverage",
    }
    if include_per_list:
        body["per_list"] = _per_list_shards(data_dir)
    return body


def capabilities(data_dir: Path) -> dict[str, bool]:
    """Which optional tiers are provisioned on this deployment?

    Callers + monitoring use this to distinguish "feature returned
    no results" from "feature not available on this server". Each
    field is a cheap filesystem probe — no index open, no SQL.
    """
    state = data_dir / "state"
    tier_generations = read_tier_generations(data_dir)
    # Per-tier generation markers are written at the end of ingest
    # only when the tier committed cleanly; missing marker == tier
    # never written on this data_dir OR a legacy pre-marker build.
    # `path_vocab_ready` stays a cheap "is the file provisioned?"
    # probe; `/status.tier_status.path_vocab` carries the drift signal.
    return {
        "metadata_ready": _has_any(data_dir / "metadata"),
        "over_db_ready": (data_dir / "over.db").exists(),
        "bm25_ready": (state / "bm25.generation").exists(),
        "trigram_ready": (state / "trigram.generation").exists(),
        "tid_ready": (state / "tid.generation").exists(),
        "path_vocab_generation_ready": tier_generations.get("path_vocab") is not None,
        "path_vocab_ready": (data_dir / "paths" / "vocab.txt").exists(),
        "embedding_ready": (data_dir / "embeddings" / "meta.json").exists(),
        "maintainers_ready": _maintainers_ready(data_dir),
        "git_sidecar_ready": (data_dir / "git_sidecar.db").exists(),
    }


def _has_any(root: Path) -> bool:
    """True when `root` exists and holds at least one regular file
    anywhere beneath it. Used to test tiers whose shape is
    "directory full of Parquet shards" (metadata/ under ingest)."""
    if not root.exists() or not root.is_dir():
        return False
    for _ in root.rglob("*"):
        return True
    return False


def _maintainers_ready(data_dir: Path) -> bool:
    """MAINTAINERS can live at `<data_dir>/MAINTAINERS` (the default)
    or an explicit absolute path in `$KLMCP_MAINTAINERS_FILE` — this
    mirrors the loader discipline in src/reader.rs."""
    import os

    override = os.environ.get("KLMCP_MAINTAINERS_FILE")
    if override:
        return Path(override).exists()
    return (data_dir / "MAINTAINERS").exists()


def _native_version() -> str:
    try:
        from kernel_lore_mcp import _core

        return _core.version()
    except Exception:
        return "unknown"


def get_status(
    settings: Settings | None = None,
    *,
    include_per_list: bool = False,
) -> dict[str, Any]:
    """Cached status builder. Used by both the MCP route and tests.

    Cache is keyed by `data_dir` so multiple in-process servers (or
    tests with different tmp_paths) don't cross-contaminate. TTL
    comes from `settings.freshness_cache_ttl_seconds`.
    """
    if settings is None:
        from kernel_lore_mcp.config import get_settings

        settings = get_settings()
    cache_key = f"{settings.data_dir}|per_list={int(include_per_list)}"
    ttl = settings.freshness_cache_ttl_seconds
    now = time.monotonic()
    live_writer = writer_lock_present(settings.data_dir)
    effective_ttl = min(ttl, 1) if live_writer else ttl
    if cache_key in _cache:
        cached_at, body = _cache[cache_key]
        if now - cached_at < effective_ttl:
            return body
    body = _build_status(settings, include_per_list=include_per_list)
    _cache[cache_key] = (now, body)
    return body


def clear_cache() -> None:
    """For tests: wipe the cache so the next `get_status()` rereads state."""
    _cache.clear()


async def status_endpoint(request: Request) -> JSONResponse:
    """Starlette/FastMCP custom-route handler."""
    raw = (request.query_params.get("per_list") or "").strip().lower()
    include_per_list = raw in {"1", "true", "yes", "on"}
    return JSONResponse(get_status(include_per_list=include_per_list))
