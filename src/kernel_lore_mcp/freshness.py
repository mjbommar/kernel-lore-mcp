"""Build populated `Freshness` envelopes from the Rust reader.

Every MCP tool that returns a response carrying a `freshness` field
should call `build_freshness(reader)` instead of constructing an
empty `Freshness()`. The helper reads the index-generation counter
and the generation-file mtime from the reader and derives `as_of`
(= commit time) + `lag_seconds` (= now - commit). `last_ingest_utc`
mirrors `as_of` for wire-compat with older clients that already
recognized that field.

A fresh data_dir (no ingest has run) leaves every field `None` /
`[]`; the client then knows the server has no data to age.
"""

from __future__ import annotations

from datetime import UTC, datetime
from typing import TYPE_CHECKING

from kernel_lore_mcp.models import Freshness

if TYPE_CHECKING:
    from kernel_lore_mcp import _core


def build_freshness(reader: _core.Reader) -> Freshness:
    try:
        generation = reader.generation()
    except Exception:
        generation = None
    try:
        mtime_ns = reader.generation_mtime_ns()
    except Exception:
        mtime_ns = None

    as_of: datetime | None = None
    lag_seconds: int | None = None
    if mtime_ns is not None:
        as_of = datetime.fromtimestamp(mtime_ns / 1_000_000_000, tz=UTC)
        now = datetime.now(tz=UTC)
        delta = (now - as_of).total_seconds()
        lag_seconds = max(0, int(delta))

    return Freshness(
        as_of=as_of,
        lag_seconds=lag_seconds,
        generation=generation,
        last_ingest_utc=as_of,
    )


__all__ = ["build_freshness"]
