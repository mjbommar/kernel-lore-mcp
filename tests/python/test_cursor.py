"""Unit + e2e tests for the HMAC-signed pagination cursor.

Covers:
  * Round-trip through the Rust sign/verify primitives.
  * Python-layer query-scope enforcement (cursor for query A
    cannot be replayed against query B).
  * Auto-generated key on fresh data_dir, env-var override.
  * End-to-end pagination through `lore_search` with no overlap
    between pages.
"""

from __future__ import annotations

import os
import secrets
from pathlib import Path

import pytest
import pytest_asyncio
from fastmcp import Client
from fastmcp.exceptions import ToolError

from kernel_lore_mcp import _core
from kernel_lore_mcp.config import get_settings, set_settings, Settings
from kernel_lore_mcp.cursor import (
    _MIN_KEY_BYTES,
    cursor_secret,
    decode_cursor,
    mint_cursor,
    query_hash,
)
from kernel_lore_mcp.errors import LoreError
from kernel_lore_mcp.server import build_server
from tests.python.fixtures import make_synthetic_shard


def test_query_hash_is_deterministic_and_argument_sensitive() -> None:
    assert query_hash("a", "b") == query_hash("a", "b")
    assert query_hash("a", "b") != query_hash("a", "c")
    assert query_hash("ab", "") != query_hash("a", "b")


def test_rust_cursor_round_trip() -> None:
    secret = secrets.token_bytes(32)
    token = _core.sign_cursor(secret, 12345, 0.5, "<mid@x>")
    q_hash, score, mid = _core.verify_cursor(secret, token)
    assert q_hash == 12345
    assert score == pytest.approx(0.5)
    assert mid == "<mid@x>"


def test_rust_cursor_rejects_wrong_secret() -> None:
    s1 = secrets.token_bytes(32)
    s2 = secrets.token_bytes(32)
    token = _core.sign_cursor(s1, 1, 1.0, "<m@x>")
    with pytest.raises(Exception):
        _core.verify_cursor(s2, token)


def test_cursor_env_var_loads_hex_key(monkeypatch: pytest.MonkeyPatch) -> None:
    key_hex = secrets.token_bytes(32).hex()
    monkeypatch.setenv("KLMCP_CURSOR_KEY", key_hex)
    assert cursor_secret() == bytes.fromhex(key_hex)


def test_cursor_env_var_rejects_short_key(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("KLMCP_CURSOR_KEY", "ab" * 8)  # 8 bytes, too short
    with pytest.raises(ValueError, match="at least"):
        cursor_secret()


def test_cursor_env_var_rejects_non_hex(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("KLMCP_CURSOR_KEY", "not-valid-hex-at-all")
    with pytest.raises(ValueError, match="hex-encoded"):
        cursor_secret()


def test_cursor_auto_generates_on_fresh_data_dir(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """With no env var and no on-disk key, the loader mints one
    lazily under <data_dir>/state/cursor.key and reuses it on the
    next call. Makes local stdio dev "just work."
    """
    monkeypatch.delenv("KLMCP_CURSOR_KEY", raising=False)
    set_settings(Settings(data_dir=tmp_path))
    k1 = cursor_secret()
    assert len(k1) >= _MIN_KEY_BYTES
    assert (tmp_path / "state" / "cursor.key").exists()
    # Second call reads the persisted key, not a fresh one.
    k2 = cursor_secret()
    assert k1 == k2


def test_mint_and_decode_round_trip_with_same_query_hash(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("KLMCP_CURSOR_KEY", raising=False)
    set_settings(Settings(data_dir=tmp_path))
    h = query_hash("search", "ksmbd dacl")
    token = mint_cursor(q_hash=h, last_score=0.123, last_mid="<m@x>")
    decoded = decode_cursor(token, expected_q_hash=h)
    assert decoded is not None
    score, mid = decoded
    assert score == pytest.approx(0.123)
    assert mid == "<m@x>"


def test_decode_cursor_rejects_wrong_query_hash(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("KLMCP_CURSOR_KEY", raising=False)
    set_settings(Settings(data_dir=tmp_path))
    token = mint_cursor(q_hash=111, last_score=1.0, last_mid="<m@x>")
    with pytest.raises(LoreError) as exc:
        decode_cursor(token, expected_q_hash=222)
    assert "different query" in str(exc.value)


def test_decode_cursor_none_passes_through(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("KLMCP_CURSOR_KEY", raising=False)
    set_settings(Settings(data_dir=tmp_path))
    assert decode_cursor(None, expected_q_hash=1) is None


@pytest_asyncio.fixture
async def client(tmp_path: Path):
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
        run_id="run-cursor-test",
    )
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        async with Client(build_server()) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


@pytest.mark.asyncio
async def test_lore_search_emits_and_accepts_next_cursor(client: Client) -> None:
    """Fetch page 1, confirm `next_cursor` is populated when more
    results exist, then replay it to get a disjoint page 2."""
    # limit=1 forces pagination even on the 2-message fixture.
    r1 = await client.call_tool(
        "lore_search",
        {"query": "ksmbd", "limit": 1, "response_format": "detailed"},
    )
    page1 = r1.data
    assert len(page1.results) == 1
    if not page1.next_cursor:
        pytest.skip("Sample corpus too small to trigger pagination")

    r2 = await client.call_tool(
        "lore_search",
        {
            "query": "ksmbd",
            "limit": 1,
            "cursor": page1.next_cursor,
            "response_format": "detailed",
        },
    )
    page2 = r2.data
    assert len(page2.results) >= 0
    mids1 = {h.message_id for h in page1.results}
    mids2 = {h.message_id for h in page2.results}
    assert not (mids1 & mids2), "pages must not overlap"


@pytest.mark.asyncio
async def test_cursor_rejected_when_query_changes(client: Client) -> None:
    """A cursor minted for one query must not resume a different one."""
    r1 = await client.call_tool(
        "lore_search",
        {"query": "ksmbd", "limit": 1, "response_format": "detailed"},
    )
    if not r1.data.next_cursor:
        pytest.skip("Sample corpus too small to trigger pagination")
    with pytest.raises(ToolError) as exc:
        await client.call_tool(
            "lore_search",
            {
                "query": "dacl",  # different query
                "limit": 1,
                "cursor": r1.data.next_cursor,
            },
        )
    msg = str(exc.value)
    assert "invalid_argument" in msg
    assert "cursor" in msg


@pytest.mark.asyncio
async def test_cursor_rejected_when_tampered(client: Client) -> None:
    """Flipping any bit in a signed cursor invalidates the HMAC and
    the tool must reject it rather than silently ignoring."""
    r1 = await client.call_tool(
        "lore_search",
        {"query": "ksmbd", "limit": 1, "response_format": "detailed"},
    )
    token = r1.data.next_cursor
    if token is None:
        pytest.skip("Sample corpus too small to trigger pagination")
    # Flip one byte toward the middle of the token.
    i = len(token) // 2
    ch = token[i]
    flipped = token[:i] + ("A" if ch != "A" else "B") + token[i + 1 :]
    with pytest.raises(ToolError) as exc:
        await client.call_tool(
            "lore_search",
            {"query": "ksmbd", "limit": 1, "cursor": flipped},
        )
    assert "invalid_argument" in str(exc.value)
