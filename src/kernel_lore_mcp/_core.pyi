"""Type stubs for the Rust extension module.

Keep in lockstep with `src/python.rs`. One declaration per exported
symbol; no docstrings needed here — the Rust side carries those.
"""

from __future__ import annotations

from os import PathLike
from typing import Any, Callable, TypedDict

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

class TidRebuildResult(TypedDict):
    path: str
    rows: int

def rebuild_tid(data_dir: str | PathLike[str]) -> TidRebuildResult: ...
def rebuild_bm25(data_dir: str | PathLike[str]) -> int: ...
def backfill_subject_normalized(data_dir: str | PathLike[str]) -> int: ...

class EmbeddingMeta(TypedDict):
    model: str
    dim: int
    metric: str
    count: int
    schema_version: int

def build_embedding_index(
    data_dir: str | PathLike[str],
    model: str,
    dim: int,
    message_ids: list[str],
    vectors: list[list[float]],
) -> EmbeddingMeta: ...
def embedding_meta(data_dir: str | PathLike[str]) -> EmbeddingMeta | None: ...

class EmbeddingBuilder:
    def __init__(
        self,
        data_dir: str | PathLike[str],
        model: str,
        dim: int,
    ) -> None: ...
    def add(self, message_id: str, vector: list[float]) -> None: ...
    def add_batch(self, message_ids: list[str], vectors: list[list[float]]) -> None: ...
    def finalize(self, build_hnsw: bool = ...) -> EmbeddingMeta: ...
    def __len__(self) -> int: ...

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
    def thread(
        self,
        message_id: str,
        max_messages: int = ...,
    ) -> list[dict[str, Any]]: ...
    def router_search(
        self,
        query: str,
        limit: int = ...,
    ) -> dict[str, Any]:
        """Returns {"hits": list[dict], "default_applied": list[str]}."""
        ...
    def patch_search(
        self,
        needle: str,
        list: str | None = ...,
        limit: int = ...,
        fuzzy_edits: int = ...,
    ) -> list[dict[str, Any]]: ...
    def prose_search(
        self,
        query: str,
        limit: int = ...,
    ) -> list[dict[str, Any]]: ...
    def fetch_body(self, message_id: str) -> bytes | None: ...
    def eq(
        self,
        field: str,
        value: str,
        since_unix_ns: int | None = ...,
        list: str | None = ...,
        limit: int = ...,
    ) -> list[dict[str, Any]]: ...
    def in_list(
        self,
        field: str,
        values: list[str],
        since_unix_ns: int | None = ...,
        list: str | None = ...,
        limit: int = ...,
    ) -> list[dict[str, Any]]: ...
    def count(
        self,
        field: str,
        value: str,
        since_unix_ns: int | None = ...,
        list: str | None = ...,
    ) -> dict[str, Any]: ...
    def author_profile(
        self,
        addr: str,
        list: str | None = ...,
        since_unix_ns: int | None = ...,
        limit: int = ...,
        include_mentions: bool = ...,
        mention_limit: int = ...,
    ) -> dict[str, Any]: ...
    def maintainer_profile(
        self,
        path: str,
        window_days: int = ...,
        activity_limit: int = ...,
    ) -> dict[str, Any]: ...
    def substr_subject(
        self,
        needle: str,
        list: str | None = ...,
        since_unix_ns: int | None = ...,
        limit: int = ...,
    ) -> list[dict[str, Any]]: ...
    def substr_trailers(
        self,
        name: str,
        value_substring: str,
        list: str | None = ...,
        since_unix_ns: int | None = ...,
        limit: int = ...,
    ) -> list[dict[str, Any]]: ...
    def regex(
        self,
        field: str,
        pattern: str,
        anchor_required: bool = ...,
        list: str | None = ...,
        since_unix_ns: int | None = ...,
        limit: int = ...,
    ) -> list[dict[str, Any]]: ...
    def diff(
        self,
        a: str,
        b: str,
        mode: str = ...,
    ) -> dict[str, Any]: ...
    def nearest(
        self,
        query_vec: list[float],
        k: int = ...,
    ) -> list[tuple[str, float]]: ...
    def nearest_to_mid(
        self,
        message_id: str,
        k: int = ...,
    ) -> list[tuple[str, float]]: ...
    def embedding_dim(self) -> int | None: ...
    def embedding_model(self) -> str | None: ...
    def scan_batches(
        self,
        callback: Callable[[list[dict[str, Any]]], bool],
        batch_size: int = ...,
        list: str | None = ...,
        since_unix_ns: int | None = ...,
    ) -> None: ...
    def generation(self) -> int: ...
    def generation_mtime_ns(self) -> int | None: ...
    def path_mentions(
        self,
        path: str,
        match_mode: str = ...,
        list: str | None = ...,
        since_unix_ns: int | None = ...,
        limit: int = ...,
    ) -> list[dict[str, Any]]: ...
