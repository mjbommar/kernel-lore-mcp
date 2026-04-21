"""Process-local `_core.Reader` cache.

Heavy query paths like cross-list `patch_search` and the router's
trigram branch benefit from reusing the same Rust `Reader` instance:
its per-process caches for trigram segments, BM25 readers, and stores
otherwise get rebuilt on every tool call.

Keep the cache tiny and keyed by canonicalized data-dir path so tests
that point different tempdirs at the process do not cross-contaminate.
"""

from __future__ import annotations

from functools import lru_cache
from pathlib import Path

from kernel_lore_mcp import _core
from kernel_lore_mcp.config import get_settings


def _reader_key(data_dir: Path) -> str:
    return str(Path(data_dir).expanduser().resolve(strict=False))


@lru_cache(maxsize=4)
def _reader_for(data_dir_key: str) -> _core.Reader:
    return _core.Reader(data_dir_key)


def get_reader() -> _core.Reader:
    return _reader_for(_reader_key(get_settings().data_dir))


def clear_reader_cache() -> None:
    _reader_for.cache_clear()


__all__ = ["clear_reader_cache", "get_reader"]
