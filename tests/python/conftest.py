"""Shared pytest fixtures.

The canonical pattern for testing FastMCP 3.x servers is the
in-process `fastmcp.Client(server)` — it exercises the full wire
contract (tools/list, tools/call, resources, pagination) without
standing up a subprocess.
"""

from __future__ import annotations

from collections.abc import AsyncIterator

import pytest
from fastmcp import Client, FastMCP

from kernel_lore_mcp.server import build_server


@pytest.fixture
def server() -> FastMCP:
    return build_server()


@pytest.fixture
async def client(server: FastMCP) -> AsyncIterator[Client]:
    async with Client(server) as c:
        yield c
