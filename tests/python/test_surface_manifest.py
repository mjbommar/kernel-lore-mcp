"""CI drift-detector for the MCP surface manifest.

Asserts every name in `REQUIRED_TOOLS` / `REQUIRED_RESOURCE_TEMPLATES`
/ `REQUIRED_PROMPTS` / `REQUIRED_STATIC_RESOURCES` actually exists
on the live server. Fails loudly when a tool is renamed or
dropped without a corresponding manifest update — prevents the
class of bug where `scripts/klmcp-doctor.sh` false-fails because
the hand-maintained list drifted from registration.

These are subset checks, not equality, so adding new optional tools
/ templates without updating the manifest is fine. The manifest
captures the *contractual* minimum.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest
import pytest_asyncio
from fastmcp import Client

from kernel_lore_mcp import _core
from kernel_lore_mcp._surface_manifest import (
    REQUIRED_PROMPTS,
    REQUIRED_RESOURCE_TEMPLATES,
    REQUIRED_STATIC_RESOURCES,
    REQUIRED_TOOLS,
)
from kernel_lore_mcp.server import build_server
from tests.python.fixtures import make_synthetic_shard


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
        run_id="surface-test",
    )
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        async with Client(build_server()) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


@pytest.mark.asyncio
async def test_every_required_tool_is_registered(client: Client) -> None:
    live = {t.name for t in await client.list_tools()}
    missing = REQUIRED_TOOLS - live
    assert not missing, (
        f"tools missing from live server: {sorted(missing)}. "
        "Register them in src/kernel_lore_mcp/server.py or remove "
        "them from _surface_manifest.py."
    )


@pytest.mark.asyncio
async def test_every_required_resource_template_is_registered(
    client: Client,
) -> None:
    live = {t.uriTemplate for t in await client.list_resource_templates()}
    missing = REQUIRED_RESOURCE_TEMPLATES - live
    assert not missing, (
        f"resource templates missing from live server: {sorted(missing)}. "
        "Check src/kernel_lore_mcp/resources/templates.py."
    )


@pytest.mark.asyncio
async def test_every_required_prompt_is_registered(client: Client) -> None:
    live = {p.name for p in await client.list_prompts()}
    missing = REQUIRED_PROMPTS - live
    assert not missing, (
        f"prompts missing from live server: {sorted(missing)}. "
        "Check src/kernel_lore_mcp/prompts.py (register_prompts)."
    )


@pytest.mark.asyncio
async def test_every_required_static_resource_is_registered(
    client: Client,
) -> None:
    live = {str(r.uri) for r in await client.list_resources()}
    missing = REQUIRED_STATIC_RESOURCES - live
    assert not missing, (
        f"static resources missing from live server: {sorted(missing)}. "
        "Check src/kernel_lore_mcp/server.py."
    )
