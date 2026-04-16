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

# Cache keyed by data_dir so multi-config / test embedding doesn't
# cross-contaminate. TTL from settings.freshness_cache_ttl_seconds.
_cache: dict[str, tuple[float, dict[str, Any]]] = {}


def _read_generation(data_dir: Path) -> tuple[int, datetime | None]:
    path = data_dir / "state" / "generation"
    if not path.exists():
        return (0, None)
    try:
        gen = int(path.read_text().strip())
    except ValueError:
        gen = 0
    mtime = datetime.fromtimestamp(path.stat().st_mtime, tz=UTC)
    return (gen, mtime)


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


def _build_status(settings: Settings) -> dict[str, Any]:
    data_dir = settings.data_dir
    generation, last_ingest = _read_generation(data_dir)
    per_list = _per_list_shards(data_dir)

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

    return {
        "service": "kernel-lore-mcp",
        "version": _native_version(),
        "generation": generation,
        "last_ingest_utc": last_ingest.isoformat() if last_ingest else None,
        "last_ingest_age_seconds": last_ingest_age_seconds,
        "configured_interval_seconds": interval,
        "freshness_ok": freshness_ok,
        "per_list": per_list,
        "blind_spots_ref": "blind-spots://coverage",
    }


def _native_version() -> str:
    try:
        from kernel_lore_mcp import _core

        return _core.version()
    except Exception:
        return "unknown"


def get_status(settings: Settings | None = None) -> dict[str, Any]:
    """Cached status builder. Used by both the MCP route and tests.

    Cache is keyed by `data_dir` so multiple in-process servers (or
    tests with different tmp_paths) don't cross-contaminate. TTL
    comes from `settings.freshness_cache_ttl_seconds`.
    """
    if settings is None:
        from kernel_lore_mcp.config import get_settings

        settings = get_settings()
    cache_key = str(settings.data_dir)
    ttl = settings.freshness_cache_ttl_seconds
    now = time.monotonic()
    if cache_key in _cache:
        cached_at, body = _cache[cache_key]
        if now - cached_at < ttl:
            return body
    body = _build_status(settings)
    _cache[cache_key] = (now, body)
    return body


def clear_cache() -> None:
    """For tests: wipe the cache so the next `get_status()` rereads state."""
    _cache.clear()


async def status_endpoint(request: Request) -> JSONResponse:
    """Starlette/FastMCP custom-route handler."""
    return JSONResponse(get_status())
