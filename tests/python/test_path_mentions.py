"""Phase 13a-file — path-mention reverse index via Aho-Corasick.

Tests cover: exact / basename / prefix modes, path tier builds from
the synthetic fixture's `touched_files[]`, scan finds body mentions,
tool registration + MCP round-trip.

The synthetic fixture carries two messages with `diff --git` headers:
  m1: fs/smb/server/smbacl.c
  m2: fs/smb/server/smb2pdu.c
So the touched_files union = {fs/smb/server/smbacl.c, fs/smb/server/smb2pdu.c}.
Both paths appear in the bodies (via the diff headers themselves).
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


def _setup_with_vocab(tmp_path: Path) -> Path:
    """Ingest + build path vocab."""
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
        run_id="path-test",
    )
    # Build the path vocab from the ingested metadata.
    reader = _core.Reader(data_dir)
    # Collect all touched_files from all rows.
    rows = reader.eq("list", "linux-cifs", None, None, 1000)
    all_paths: set[str] = set()
    for r in rows:
        for f in r.get("touched_files") or []:
            all_paths.add(f)
    # Write vocab file so path_mentions() can find it.
    vocab_dir = data_dir / "paths"
    vocab_dir.mkdir(exist_ok=True)
    (vocab_dir / "vocab.txt").write_text("\n".join(sorted(all_paths)))
    return data_dir


@pytest_asyncio.fixture
async def client_with_vocab(tmp_path: Path) -> AsyncIterator[Client]:
    data_dir = _setup_with_vocab(tmp_path)
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        async with Client(build_server()) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


@pytest.mark.asyncio
async def test_path_mentions_tool_registered(client_with_vocab: Client) -> None:
    tools = await client_with_vocab.list_tools()
    names = {t.name for t in tools}
    assert "lore_path_mentions" in names


@pytest.mark.asyncio
async def test_exact_match_finds_smbacl(client_with_vocab: Client) -> None:
    result = await client_with_vocab.call_tool(
        "lore_path_mentions",
        {"path": "fs/smb/server/smbacl.c", "match": "exact"},
    )
    mids = {h.message_id for h in result.data.results}
    assert "m1@x" in mids


@pytest.mark.asyncio
async def test_basename_match_finds_across_paths(client_with_vocab: Client) -> None:
    # basename "smb2pdu.c" should find m2
    result = await client_with_vocab.call_tool(
        "lore_path_mentions",
        {"path": "smb2pdu.c", "match": "basename"},
    )
    mids = {h.message_id for h in result.data.results}
    assert "m2@x" in mids


@pytest.mark.asyncio
async def test_prefix_match_finds_all_under_dir(client_with_vocab: Client) -> None:
    result = await client_with_vocab.call_tool(
        "lore_path_mentions",
        {"path": "fs/smb/server/", "match": "prefix"},
    )
    mids = {h.message_id for h in result.data.results}
    # Both m1 and m2 touch paths under fs/smb/server/
    assert mids == {"m1@x", "m2@x"}


@pytest.mark.asyncio
async def test_missing_path_returns_empty(client_with_vocab: Client) -> None:
    result = await client_with_vocab.call_tool(
        "lore_path_mentions",
        {"path": "does/not/exist.c", "match": "exact"},
    )
    assert result.data.results == []


def test_rust_path_tier_roundtrip(tmp_path: Path) -> None:
    """Direct Python → _core test without MCP layer."""
    data_dir = _setup_with_vocab(tmp_path)
    reader = _core.Reader(data_dir)
    rows = reader.path_mentions("smbacl.c", "basename")
    mids = {r["message_id"] for r in rows}
    assert "m1@x" in mids
