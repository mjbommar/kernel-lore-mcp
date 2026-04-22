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

SYZ_HASH = "ac3c79181f6aecc5120c"

FIX_STATUS_MESSAGES: list[bytes] = [
    (
        b"From: syzbot+ac3c79181f6aecc5120c@syzkaller.appspotmail.com\r\n"
        b"Subject: [syzbot] KASAN: use-after-free in foo\r\n"
        b"Date: Tue, 14 Apr 2026 10:00:00 +0000\r\n"
        b"Message-ID: <report@x>\r\n"
        b"\r\n"
        b"syzbot report body\r\n"
        b"See https://syzkaller.appspot.com/bug?extid=ac3c79181f6aecc5120c\r\n"
    ),
    (
        b"From: Developer <dev@example.com>\r\n"
        b"Subject: [PATCH] foo: guard stale pointer\r\n"
        b"Date: Tue, 14 Apr 2026 11:00:00 +0000\r\n"
        b"Message-ID: <fix@x>\r\n"
        b"\r\n"
        b"Reported-by: syzbot+ac3c79181f6aecc5120c@syzkaller.appspotmail.com\r\n"
        b"Link: https://syzkaller.appspot.com/bug?extid=ac3c79181f6aecc5120c\r\n"
        b"Link: https://lore.kernel.org/all/report@x/\r\n"
        b"Signed-off-by: Developer <dev@example.com>\r\n"
        b"---\r\n"
        b"diff --git a/drivers/foo.c b/drivers/foo.c\r\n"
        b"--- a/drivers/foo.c\r\n"
        b"+++ b/drivers/foo.c\r\n"
        b"@@ -1,1 +1,2 @@ int foo_fix(void)\r\n"
        b" a\r\n"
        b"+b\r\n"
    ),
]


@pytest_asyncio.fixture
async def client(tmp_path: Path) -> AsyncIterator[Client]:
    shard_dir = tmp_path / "shards" / "0.git"
    shard_dir.parent.mkdir(parents=True)
    make_synthetic_shard(shard_dir, FIX_STATUS_MESSAGES)

    data_dir = tmp_path / "data"
    data_dir.mkdir()
    _core.ingest_shard(
        data_dir=data_dir,
        shard_path=shard_dir,
        list="linux-kernel",
        shard="0",
        run_id="run-0001",
    )

    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        async with Client(build_server()) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


@pytest.mark.asyncio
async def test_fix_status_correlates_pending_patch_from_syzbot_seed(client: Client) -> None:
    result = await client.call_tool("lore_fix_status", {"message_id": "report@x"})
    data = result.data

    assert data.seed_message_id == "report@x"
    assert data.syzbot_hash_queried == SYZ_HASH
    assert data.state == "pending_patch"
    assert data.backend == "lore_correlated"
    assert data.confidence == "medium"
    assert len(data.fix_candidates) == 1
    assert data.fix_candidates[0].message_id == "fix@x"
    assert data.fix_candidates[0].has_patch is True
    assert data.fix_candidates[0].matched_by


@pytest.mark.asyncio
async def test_fix_status_accepts_direct_syzbot_hash_query(client: Client) -> None:
    result = await client.call_tool("lore_fix_status", {"syzbot_hash": SYZ_HASH})
    data = result.data

    assert data.syzbot_hash_queried == SYZ_HASH
    assert data.state == "pending_patch"
    assert {candidate.message_id for candidate in data.fix_candidates} == {"fix@x"}
