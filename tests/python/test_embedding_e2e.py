"""End-to-end tests for the Phase-8 embedding tier.

We bypass the real fastembed model load (~75 MB download, ~2 s
warmup) by feeding `_core.build_embedding_index` synthetic 8-dim
vectors keyed on each message-id. The Rust HNSW + the
lore_similar / lore_nearest tools then exercise the full path.

The fastembed-backed `lore_nearest` is covered by a separate
`@pytest.mark.live` test that's skipped by default.
"""

from __future__ import annotations

import math
import os
from collections.abc import AsyncIterator
from pathlib import Path

import pytest
import pytest_asyncio
from fastmcp import Client
from fastmcp.exceptions import ToolError

from kernel_lore_mcp import _core
from kernel_lore_mcp.server import build_server
from tests.python.fixtures import make_synthetic_shard


def _norm(vec: list[float]) -> list[float]:
    n = math.sqrt(sum(x * x for x in vec)) or 1.0
    return [x / n for x in vec]


@pytest_asyncio.fixture
async def client(tmp_path: Path) -> AsyncIterator[Client]:
    shard_dir = tmp_path / "shards" / "0.git"
    shard_dir.parent.mkdir(parents=True)
    make_synthetic_shard(shard_dir)
    data_dir = tmp_path / "data"
    data_dir.mkdir()
    _core.ingest_shard(
        data_dir=data_dir,
        shard_path=shard_dir,
        list="linux-cifs",
        shard="0",
        run_id="run-0001",
    )

    # Synthetic 8-dim vectors. m1 lives near (1, 0, ..., 0); m2 near
    # (0, 1, 0, ..., 0). A query close to m1's axis must rank m1
    # first; nearest_to_mid("m1@x") puts m1 first too.
    _core.build_embedding_index(
        data_dir=data_dir,
        model="test/synthetic-8",
        dim=8,
        message_ids=["m1@x", "m2@x"],
        vectors=[
            _norm([1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            _norm([0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ],
    )

    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        async with Client(build_server()) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


@pytest.mark.asyncio
async def test_embedding_tools_listed(client: Client) -> None:
    tools = await client.list_tools()
    names = {t.name for t in tools}
    assert {"lore_nearest", "lore_similar"}.issubset(names)


@pytest.mark.asyncio
async def test_lore_similar_returns_nearest_excluding_seed(client: Client) -> None:
    result = await client.call_tool("lore_similar", {"message_id": "m1@x", "k": 5})
    data = result.data
    mids = [h.message_id for h in data.results]
    # Seed not included by default; only m2 left.
    assert mids == ["m2@x"]
    assert data.model == "test/synthetic-8"
    assert data.dim == 8
    # Cosine similarity of orthogonal unit vectors is ~0.
    assert abs(data.results[0].score) < 1e-3


@pytest.mark.asyncio
async def test_lore_similar_with_include_seed(client: Client) -> None:
    result = await client.call_tool(
        "lore_similar", {"message_id": "m1@x", "k": 5, "include_seed": True}
    )
    mids = [h.message_id for h in result.data.results]
    assert mids[0] == "m1@x"
    assert mids[1] == "m2@x"


@pytest.mark.asyncio
async def test_lore_similar_unknown_mid_raises(client: Client) -> None:
    with pytest.raises(ToolError, match="not present in the embedding index"):
        await client.call_tool("lore_similar", {"message_id": "does-not-exist@x"})


@pytest.mark.asyncio
async def test_lore_nearest_requires_built_index(tmp_path: Path) -> None:
    """Without `_core.build_embedding_index`, lore_nearest must fail
    loudly — never silently return empty.
    """
    # Clean data_dir, no embedding index built.
    data_dir = tmp_path / "data"
    data_dir.mkdir()

    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        async with Client(build_server()) as c:
            with pytest.raises(ToolError, match="not built"):
                await c.call_tool("lore_nearest", {"query": "anything"})
            with pytest.raises(ToolError, match="not built"):
                await c.call_tool("lore_similar", {"message_id": "anything"})
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


def test_embedding_meta_roundtrip(tmp_path: Path) -> None:
    data_dir = tmp_path / "data"
    data_dir.mkdir()
    _core.build_embedding_index(
        data_dir=data_dir,
        model="test/m",
        dim=4,
        message_ids=["a", "b", "c"],
        vectors=[
            _norm([1.0, 0.0, 0.0, 0.0]),
            _norm([0.0, 1.0, 0.0, 0.0]),
            _norm([0.0, 0.0, 1.0, 0.0]),
        ],
    )
    meta = _core.embedding_meta(data_dir)
    assert meta is not None
    assert meta["model"] == "test/m"
    assert meta["dim"] == 4
    assert meta["count"] == 3
    assert meta["metric"] == "cosine"
    assert meta["schema_version"] == 1


def test_embedding_meta_returns_none_when_absent(tmp_path: Path) -> None:
    assert _core.embedding_meta(tmp_path) is None
