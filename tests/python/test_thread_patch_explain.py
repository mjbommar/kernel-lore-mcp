"""End-to-end tests for the four phase-5b tools:
lore_thread, lore_patch, lore_patch_diff, lore_explain_patch.
"""

from __future__ import annotations

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

# A two-version synthetic series — m1 (v1) and m2 (v2), where m2's
# subject is the same series and patch hunk so lore_patch_diff has
# something to compare. m3 is a reply to m2 so the thread walker
# can reach all three from any seed.
SERIES_MESSAGES: list[bytes] = [
    b"From: Alice <alice@example.com>\r\n"
    b"Subject: [PATCH v1 1/1] ksmbd: tighten ACL bounds\r\n"
    b"Date: Mon, 14 Apr 2026 12:00:00 +0000\r\n"
    b"Message-ID: <m1@x>\r\n"
    b"\r\n"
    b"Initial version of the patch.\r\n"
    b"Signed-off-by: Alice <alice@example.com>\r\n"
    b"---\r\n"
    b"diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c\r\n"
    b"--- a/fs/smb/server/smbacl.c\r\n"
    b"+++ b/fs/smb/server/smbacl.c\r\n"
    b"@@ -1,1 +1,2 @@ int smb_check_perm_dacl(struct ksmbd_conn *c)\r\n"
    b" a\r\n"
    b"+if (ace_size < sizeof(struct smb_ace)) return -EINVAL;\r\n",
    b"From: Alice <alice@example.com>\r\n"
    b"Subject: [PATCH v2 1/1] ksmbd: tighten ACL bounds\r\n"
    b"Date: Mon, 14 Apr 2026 13:00:00 +0000\r\n"
    b"Message-ID: <m2@x>\r\n"
    b"In-Reply-To: <m1@x>\r\n"
    b"References: <m1@x>\r\n"
    b"\r\n"
    b"v2: also bound the upper end.\r\n"
    b"Signed-off-by: Alice <alice@example.com>\r\n"
    b"---\r\n"
    b"diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c\r\n"
    b"--- a/fs/smb/server/smbacl.c\r\n"
    b"+++ b/fs/smb/server/smbacl.c\r\n"
    b"@@ -1,1 +1,3 @@ int smb_check_perm_dacl(struct ksmbd_conn *c)\r\n"
    b" a\r\n"
    b"+if (ace_size < sizeof(struct smb_ace)) return -EINVAL;\r\n"
    b"+if (ace_size > MAX_ACE_SIZE) return -EINVAL;\r\n",
    b"From: Reviewer <r@example.com>\r\n"
    b"Subject: Re: [PATCH v2 1/1] ksmbd: tighten ACL bounds\r\n"
    b"Date: Mon, 14 Apr 2026 14:00:00 +0000\r\n"
    b"Message-ID: <m3@x>\r\n"
    b"In-Reply-To: <m2@x>\r\n"
    b"References: <m1@x> <m2@x>\r\n"
    b"\r\n"
    b"Looks good. Reviewed-by: Reviewer <r@example.com>\r\n",
]


@pytest_asyncio.fixture
async def client(tmp_path: Path) -> AsyncIterator[Client]:
    shard_dir = tmp_path / "shards" / "0.git"
    shard_dir.parent.mkdir(parents=True)
    make_synthetic_shard(shard_dir, SERIES_MESSAGES)

    data_dir = tmp_path / "data"
    data_dir.mkdir()
    _core.ingest_shard(
        data_dir=data_dir,
        shard_path=shard_dir,
        list="linux-cifs",
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
async def test_lore_thread_walks_full_conversation(client: Client) -> None:
    # Seeding from m3 must reach m1 and m2 via in_reply_to /
    # references walking.
    result = await client.call_tool("lore_thread", {"message_id": "m3@x"})
    data = result.data
    mids = [m.hit.message_id for m in data.messages]
    assert set(mids) == {"m1@x", "m2@x", "m3@x"}
    # Ordered by date.
    assert mids == ["m1@x", "m2@x", "m3@x"]
    assert data.truncated is False
    # The patch carrier (m2) should have its patch field populated.
    by_mid = {m.hit.message_id: m for m in data.messages}
    assert by_mid["m2@x"].patch is not None
    assert by_mid["m3@x"].patch is None  # reply has no patch


@pytest.mark.asyncio
async def test_lore_patch_returns_diff_text(client: Client) -> None:
    result = await client.call_tool("lore_patch", {"message_id": "m1@x"})
    data = result.data
    assert data.hit.message_id == "m1@x"
    assert data.patch.startswith("diff --git ")
    assert "smb_check_perm_dacl" in data.patch
    assert len(data.body_sha256) == 64


@pytest.mark.asyncio
async def test_lore_patch_rejects_message_with_no_patch(client: Client) -> None:
    with pytest.raises(ToolError, match="no patch payload"):
        await client.call_tool("lore_patch", {"message_id": "m3@x"})


@pytest.mark.asyncio
async def test_lore_patch_diff_compares_versions(client: Client) -> None:
    result = await client.call_tool(
        "lore_patch_diff",
        {"a": "m1@x", "b": "m2@x"},
    )
    data = result.data
    assert data.a.message_id == "m1@x"
    assert data.b.message_id == "m2@x"
    # v2 added a MAX_ACE_SIZE check; the unified diff should mention it.
    assert "MAX_ACE_SIZE" in data.diff
    assert data.diff.startswith("--- a/m1@x")


@pytest.mark.asyncio
async def test_lore_patch_diff_rejects_same_mid(client: Client) -> None:
    with pytest.raises(ToolError, match="must be different"):
        await client.call_tool("lore_patch_diff", {"a": "m1@x", "b": "m1@x"})


@pytest.mark.asyncio
async def test_lore_explain_patch_one_call_view(client: Client) -> None:
    result = await client.call_tool("lore_explain_patch", {"message_id": "m2@x"})
    data = result.data
    assert data.hit.message_id == "m2@x"
    assert data.prose is not None
    assert "v2" in data.prose
    assert data.patch is not None
    assert "MAX_ACE_SIZE" in data.patch
    # Series timeline picks up m1 + m2 (same normalized subject + author).
    series_mids = [e.message_id for e in data.series]
    assert set(series_mids) == {"m1@x", "m2@x"}
    # m3 directly replies to m2, so it shows up downstream.
    downstream_mids = {h.message_id for h in data.downstream}
    assert downstream_mids == {"m3@x"}
