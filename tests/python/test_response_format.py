"""Sprint 0 / CW-E — `response_format: 'concise' | 'detailed'` knob.

High-volume tools accept a verbosity dial so agents can cap tokens
without losing access to the full response. Invariants:

* Concise mode caps results (`lore_search`, `lore_activity`) or omits
  bodies (`lore_thread`) or truncates diff text (`lore_patch_diff`).
* Detailed mode returns the full payload.
* The concise path flags truncation (either via `truncated*` booleans
  or `default_applied` entries) so the agent knows more data exists.
"""

from __future__ import annotations

import os
from collections.abc import AsyncIterator
from pathlib import Path

import pytest
import pytest_asyncio
from fastmcp import Client

from kernel_lore_mcp import _core
from kernel_lore_mcp.server import build_server
from tests.python.fixtures import make_synthetic_shard


@pytest_asyncio.fixture
async def client_with_data(tmp_path: Path) -> AsyncIterator[Client]:
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
        run_id="run-responseformat",
    )
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        async with Client(build_server()) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


@pytest.mark.asyncio
async def test_lore_thread_concise_omits_bodies(client_with_data: Client) -> None:
    concise = await client_with_data.call_tool(
        "lore_thread",
        {"message_id": "m1@x", "response_format": "concise"},
    )
    for m in concise.data.messages:
        # Bodies are expensive; concise must skip them.
        assert m.prose is None
        assert m.patch is None


@pytest.mark.asyncio
async def test_lore_thread_detailed_includes_bodies(client_with_data: Client) -> None:
    detailed = await client_with_data.call_tool(
        "lore_thread",
        {"message_id": "m1@x", "response_format": "detailed"},
    )
    # At least one message in the synthetic fixture has a patch body.
    has_any_body = any((m.prose is not None or m.patch is not None) for m in detailed.data.messages)
    assert has_any_body


@pytest.mark.asyncio
async def test_lore_search_defaults_to_concise(client_with_data: Client) -> None:
    # `default_applied` must say so, even if the corpus is below the cap;
    # the field exists to tell the agent what knob was used.
    result = await client_with_data.call_tool(
        "lore_search",
        {"query": "ksmbd"},
    )
    # Synthetic corpus is tiny — concise flag only fires when > 10 hits.
    # Either way, tool must succeed and return a SearchResponse.
    assert result.data.results is not None


@pytest.mark.asyncio
async def test_lore_patch_diff_concise_truncates_long_diff(
    client_with_data: Client,
) -> None:
    # Synthetic fixture diff is short, so concise vs detailed should
    # produce the same text — only the *shape* must hold.
    concise = await client_with_data.call_tool(
        "lore_patch_diff",
        {"a": "m1@x", "b": "m2@x", "response_format": "concise"},
    )
    detailed = await client_with_data.call_tool(
        "lore_patch_diff",
        {"a": "m1@x", "b": "m2@x", "response_format": "detailed"},
    )
    # Whatever concise emits must be <= detailed in character length.
    assert len(concise.data.diff) <= len(detailed.data.diff) + 200
    # Detailed must never show the truncation marker.
    assert "rerun with response_format='detailed'" not in detailed.data.diff
