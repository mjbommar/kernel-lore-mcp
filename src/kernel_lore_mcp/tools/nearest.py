"""lore_nearest + lore_similar — embedding-tier semantic retrieval.

`lore_nearest`: caller hands us a freeform query string; we embed it
locally via `FastembedEmbedder` (matching the model the index was
built with), then ask the Rust-side HNSW for top-k. Returns
`NearestResponse` with cosine similarities.

`lore_similar`: caller hands us a known message-id; we look up its
stored vector in the index (no re-embedding) and return top-k
neighbours. Useful for "more like this" without burning the
embedding model on every call.

Both tools fail loudly when the embedding index hasn't been built
yet (`kernel-lore-embed` CLI bootstraps it). They never silently
return empty.
"""

from __future__ import annotations

import asyncio
from datetime import UTC, datetime
from functools import lru_cache
from typing import Annotated

from fastmcp.exceptions import ToolError
from pydantic import Field

from kernel_lore_mcp.config import Settings
from kernel_lore_mcp.embedding import DEFAULT_MODEL, Embedder, FastembedEmbedder, l2_normalize
from kernel_lore_mcp.mapping import cite_key, lore_url
from kernel_lore_mcp.models import Freshness, NearestHit, NearestResponse


@lru_cache(maxsize=4)
def _embedder_for(model: str) -> Embedder:
    """Process-lifetime cache. fastembed model load is expensive."""
    return FastembedEmbedder(model_name=model)


def _row_to_nearest_hit(row: dict, score: float) -> NearestHit:
    subject = row.get("subject_normalized") or row.get("subject_raw") or ""
    date_ns = row.get("date_unix_ns")
    date = datetime.fromtimestamp(date_ns / 1_000_000_000, tz=UTC) if date_ns else None
    return NearestHit(
        message_id=row["message_id"],
        cite_key=cite_key(row),
        score=float(score),
        list=row["list"],
        from_addr=row.get("from_addr"),
        subject=subject,
        date=date,
        has_patch=bool(row.get("has_patch")),
        lore_url=lore_url(row),
    )


async def lore_nearest(
    query: Annotated[
        str,
        Field(
            min_length=1,
            max_length=2048,
            description=(
                "Free-text natural-language query. Embedded locally via the "
                "model that was used to build the index (see /status for the "
                "model name); ANN-searched against the stored vectors."
            ),
        ),
    ],
    k: Annotated[int, Field(ge=1, le=200)] = 25,
) -> NearestResponse:
    """Semantic nearest-neighbour search."""
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    index_model = await asyncio.to_thread(reader.embedding_model)
    index_dim = await asyncio.to_thread(reader.embedding_dim)
    if index_model is None or index_dim is None:
        raise ToolError(
            "embedding index not built yet — run `kernel-lore-embed --data-dir <path>` "
            "to bootstrap, then retry"
        )

    embedder = await asyncio.to_thread(_embedder_for, index_model)
    if embedder.dim != index_dim:
        raise ToolError(
            f"embedder dim {embedder.dim} != index dim {index_dim} (model "
            f"{index_model!r}); index needs a rebuild"
        )

    [vec] = await asyncio.to_thread(embedder.embed, [query])
    vec = l2_normalize(vec)
    hits = await asyncio.to_thread(reader.nearest, vec, k)

    rows: list[NearestHit] = []
    for mid, score in hits:
        row = await asyncio.to_thread(reader.fetch_message, mid)
        if row is not None:
            rows.append(_row_to_nearest_hit(row, score))
    return NearestResponse(
        results=rows,
        model=index_model,
        dim=index_dim,
        freshness=Freshness(),
    )


async def lore_similar(
    message_id: Annotated[
        str,
        Field(
            min_length=1,
            max_length=512,
            description="Seed message-id; we look up its stored vector and ANN-search.",
        ),
    ],
    k: Annotated[int, Field(ge=1, le=200)] = 25,
    include_seed: Annotated[
        bool,
        Field(
            description=(
                "When true, the seed message is included in results "
                "(typically as the highest-similarity hit)."
            ),
        ),
    ] = False,
) -> NearestResponse:
    """Find messages most similar to a known message-id."""
    from kernel_lore_mcp import _core

    settings = Settings()
    reader = _core.Reader(settings.data_dir)
    index_model = await asyncio.to_thread(reader.embedding_model)
    index_dim = await asyncio.to_thread(reader.embedding_dim)
    if index_model is None or index_dim is None:
        raise ToolError(
            "embedding index not built yet — run `kernel-lore-embed --data-dir <path>` "
            "to bootstrap, then retry"
        )

    over_k = k + (0 if include_seed else 1)
    hits = await asyncio.to_thread(reader.nearest_to_mid, message_id, over_k)
    if not hits:
        raise ToolError(f"message_id {message_id!r} not present in the embedding index")

    rows: list[NearestHit] = []
    for mid, score in hits:
        if not include_seed and mid == message_id:
            continue
        row = await asyncio.to_thread(reader.fetch_message, mid)
        if row is not None:
            rows.append(_row_to_nearest_hit(row, score))
        if len(rows) >= k:
            break

    return NearestResponse(
        results=rows,
        model=index_model,
        dim=index_dim,
        freshness=Freshness(),
    )


__all__ = ["DEFAULT_MODEL", "lore_nearest", "lore_similar"]
