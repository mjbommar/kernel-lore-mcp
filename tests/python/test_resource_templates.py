"""Phase 10 — RFC-6570 templated resources.

Covers the 5 `lore://` templates registered via FastMCP:

  lore://message/{mid}      — real body (MIME text/plain)
  lore://thread/{tid}       — concatenated bodies (MIME text/plain)
  lore://patch/{mid}        — patch payload (MIME text/x-diff)
  lore://maintainer/{path}  — stub (MIME text/plain; Phase 18A)
  lore://patchwork/{msg_id} — stub (MIME application/json; Phase 19A)

Invariants:
* `list_resource_templates()` surfaces all five with correct
  `uriTemplate` and `mimeType`.
* `read_resource()` dispatches to the template, returning the right
  body + MIME for real data + for stubs.
* Unknown Message-IDs raise a `ResourceError`-class error whose
  message carries the offending mid (not a generic wrapper).
"""

from __future__ import annotations

import json
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
        run_id="run-templates",
    )
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        async with Client(build_server()) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


EXPECTED_TEMPLATES = {
    "lore://message/{mid}": "text/plain",
    "lore://thread/{tid}": "text/plain",
    "lore://patch/{mid}": "text/x-diff",
    "lore://maintainer/{path}": "text/plain",
    "lore://patchwork/{msg_id}": "application/json",
}


@pytest.mark.asyncio
async def test_list_resource_templates_surfaces_all_five(client_with_data: Client) -> None:
    templates = await client_with_data.list_resource_templates()
    by_uri = {t.uriTemplate: t for t in templates}
    for uri, mime in EXPECTED_TEMPLATES.items():
        assert uri in by_uri, f"template {uri} not registered"
        assert by_uri[uri].mimeType == mime, f"{uri}: expected {mime}, got {by_uri[uri].mimeType}"


@pytest.mark.asyncio
async def test_read_lore_message_returns_real_body(client_with_data: Client) -> None:
    contents = await client_with_data.read_resource("lore://message/m1@x")
    assert len(contents) == 1
    body = contents[0]
    # FastMCP returns TextResourceContents for str-returning resources.
    assert body.mimeType == "text/plain"
    assert hasattr(body, "text") and "ksmbd" in body.text.lower()
    # The mbox keeps the raw headers we seeded.
    assert "Message-ID: <m1@x>" in body.text


@pytest.mark.asyncio
async def test_read_lore_patch_strips_prose(client_with_data: Client) -> None:
    contents = await client_with_data.read_resource("lore://patch/m1@x")
    body = contents[0]
    assert body.mimeType == "text/x-diff"
    # Diff payload starts at the first `diff --git ...` line; prose must
    # not be in the response.
    assert body.text.lstrip().startswith("diff --git ")
    assert "Prose here" not in body.text


@pytest.mark.asyncio
async def test_read_lore_thread_concatenates(client_with_data: Client) -> None:
    # Seed with m1 — thread walker will include m2 via in_reply_to.
    contents = await client_with_data.read_resource("lore://thread/m1@x")
    body = contents[0]
    assert body.mimeType == "text/plain"
    assert "Message-ID: <m1@x>" in body.text
    assert "Message-ID: <m2@x>" in body.text


@pytest.mark.asyncio
async def test_read_lore_maintainer_stub(client_with_data: Client) -> None:
    contents = await client_with_data.read_resource("lore://maintainer/fs%2Fsmb%2Fserver")
    body = contents[0]
    assert body.mimeType == "text/plain"
    assert "Not yet implemented" in body.text
    assert "Phase 18A" in body.text


@pytest.mark.asyncio
async def test_read_lore_patchwork_stub_is_json(client_with_data: Client) -> None:
    contents = await client_with_data.read_resource("lore://patchwork/m1@x")
    body = contents[0]
    assert body.mimeType == "application/json"
    payload = json.loads(body.text)
    assert payload["status"] == "not_yet_implemented"
    assert payload["phase"] == "19A"


@pytest.mark.asyncio
async def test_read_missing_message_raises(client_with_data: Client) -> None:
    # ResourceError surfaces to the client as an exception; message
    # carries the offending mid (not a generic wrapper).
    with pytest.raises(Exception) as exc_info:
        await client_with_data.read_resource("lore://message/does-not-exist@x")
    assert "does-not-exist@x" in str(exc_info.value)


@pytest.mark.asyncio
async def test_blind_spots_still_reachable(client_with_data: Client) -> None:
    # The static resource from earlier phases must coexist with the
    # new templates.
    contents = await client_with_data.read_resource("blind-spots://coverage")
    body = contents[0]
    assert body.mimeType == "text/plain"
    assert "security@kernel.org" in body.text
