"""Type stubs for the Rust extension module.

Keep in lockstep with `src/python.rs`. One declaration per exported
symbol; no docstrings needed here — the Rust side carries those.
"""

from __future__ import annotations

from os import PathLike
from typing import Any, TypedDict

class IngestStats(TypedDict):
    ingested: int
    skipped_no_m: int
    skipped_empty: int
    skipped_no_mid: int
    parquet_path: str | None

def version() -> str: ...
def ingest_shard(
    data_dir: str | PathLike[str],
    shard_path: str | PathLike[str],
    list: str,
    shard: str,
    run_id: str,
) -> IngestStats: ...

class Reader:
    def __init__(self, data_dir: str | PathLike[str]) -> None: ...
    def fetch_message(self, message_id: str) -> dict[str, Any] | None: ...
    def activity(
        self,
        file: str | None = ...,
        function: str | None = ...,
        since_unix_ns: int | None = ...,
        list: str | None = ...,
        limit: int = ...,
    ) -> list[dict[str, Any]]: ...
    def series_timeline(self, message_id: str) -> list[dict[str, Any]]: ...
    def expand_citation(self, token: str, limit: int = ...) -> list[dict[str, Any]]: ...
    def fetch_body(self, message_id: str) -> bytes | None: ...
