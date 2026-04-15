"""Subprocess + stdio MCP plumbing test.

Boots the `kernel-lore-mcp` binary in a real subprocess (the same
way `claude --mcp-config` and `codex exec` would), then talks to it
over stdio using the official `mcp` Python SDK as a client. Verifies:

  * the binary actually starts in stdio mode
  * `tools/list` returns our v1 surface
  * a structured tool call (`lore_eq`) round-trips correctly

This catches regressions in the entry point / argument parsing /
logging-to-stderr discipline that the in-process FastMCP Client
tests can't see — most importantly, that nothing leaks to stdout
outside the JSON-RPC framing.
"""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

import pytest
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from kernel_lore_mcp import _core
from tests.python.fixtures import make_synthetic_shard

# Resolve the venv-installed entry point. Tests run via `uv run pytest`
# so .venv is active.
_REPO_ROOT = Path(__file__).resolve().parents[2]
_VENV_BIN = _REPO_ROOT / ".venv" / "bin" / "kernel-lore-mcp"


pytestmark = pytest.mark.skipif(
    not _VENV_BIN.exists(),
    reason="kernel-lore-mcp console script not installed (uv sync didn't run?)",
)


@pytest.fixture
def ingested_data_dir(tmp_path: Path) -> Path:
    shard = tmp_path / "shards" / "0.git"
    shard.parent.mkdir(parents=True)
    make_synthetic_shard(shard)
    data_dir = tmp_path / "data"
    data_dir.mkdir()
    _core.ingest_shard(
        data_dir=data_dir,
        shard_path=shard,
        list="linux-cifs",
        shard="0",
        run_id="stdio-plumbing-001",
    )
    return data_dir


@pytest.fixture
def server_params(ingested_data_dir: Path) -> StdioServerParameters:
    env = os.environ.copy()
    env["KLMCP_DATA_DIR"] = str(ingested_data_dir)
    # We avoid uv resolution overhead by invoking the script directly.
    return StdioServerParameters(
        command=str(_VENV_BIN),
        args=["--transport", "stdio"],
        env=env,
    )


@pytest.mark.asyncio
async def test_stdio_handshake_and_tools_list(
    server_params: StdioServerParameters,
) -> None:
    async with (
        stdio_client(server_params) as (read, write),
        ClientSession(read, write) as session,
    ):
        await session.initialize()
        tools = await session.list_tools()
        names = {t.name for t in tools.tools}
        for required in (
            "lore_search",
            "lore_message",
            "lore_eq",
            "lore_in_list",
            "lore_substr_subject",
            "lore_regex",
            "lore_diff",
            "lore_thread",
            "lore_explain_patch",
            "lore_nearest",
            "lore_similar",
        ):
            assert required in names, f"missing tool {required}"


@pytest.mark.asyncio
async def test_stdio_tool_call_lore_eq_roundtrip(
    server_params: StdioServerParameters,
) -> None:
    async with (
        stdio_client(server_params) as (read, write),
        ClientSession(read, write) as session,
    ):
        await session.initialize()
        result = await session.call_tool(
            "lore_eq",
            {"field": "from_addr", "value": "alice@example.com"},
        )
        # FastMCP returns structured content for pydantic returns.
        payload = result.structuredContent
        assert payload is not None
        assert payload["total"] == 2
        mids = {h["message_id"] for h in payload["results"]}
        assert mids == {"m1@x", "m2@x"}
        # readOnlyHint must round-trip — look for it on the
        # corresponding tool def from list_tools.
        tools = await session.list_tools()
        tool = next(t for t in tools.tools if t.name == "lore_eq")
        assert tool.annotations is not None
        assert tool.annotations.readOnlyHint is True


@pytest.mark.asyncio
async def test_stdio_blind_spots_resource(server_params: StdioServerParameters) -> None:
    async with (
        stdio_client(server_params) as (read, write),
        ClientSession(read, write) as session,
    ):
        await session.initialize()
        resources = await session.list_resources()
        uris = {str(r.uri) for r in resources.resources}
        assert "blind-spots://coverage" in uris
        body = await session.read_resource("blind-spots://coverage")
        text = "".join(getattr(c, "text", "") or "" for c in body.contents)
        assert "security@kernel.org" in text


@pytest.mark.skipif(
    not (os.environ.get("KLMCP_LIVE_AGENT") and shutil.which("claude")),
    reason="set KLMCP_LIVE_AGENT=1 and install claude CLI to run (auth via its own config)",
)
def test_claude_print_drives_lore_eq(ingested_data_dir: Path, tmp_path: Path) -> None:
    """Live opt-in: Claude Code with --print + --mcp-config calls
    lore_eq and returns at least one expected message-id.
    """
    import json
    import subprocess

    cfg = tmp_path / "claude.mcp.json"
    cfg.write_text(
        json.dumps(
            {
                "mcpServers": {
                    "kernel-lore": {
                        "type": "stdio",
                        "command": str(_VENV_BIN),
                        "args": ["--transport", "stdio"],
                        "env": {"KLMCP_DATA_DIR": str(ingested_data_dir)},
                    }
                }
            }
        )
    )
    out = subprocess.run(
        [
            "claude",
            "--print",
            "--mcp-config",
            str(cfg),
            "--strict-mcp-config",
            "--permission-mode",
            "bypassPermissions",
            "--allowedTools",
            "mcp__kernel-lore__lore_eq",
            "--output-format",
            "text",
            (
                "List every linux-cifs message authored by alice@example.com "
                "using the lore_eq tool with field=from_addr, "
                "value=alice@example.com. Reply with ONLY the message-ids on "
                "separate lines, no prose."
            ),
        ],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    combined = (out.stdout or "") + (out.stderr or "")
    assert "m1@x" in combined or "m2@x" in combined, combined[:1000]


@pytest.mark.skipif(
    not (os.environ.get("KLMCP_LIVE_AGENT") and shutil.which("codex")),
    reason="set KLMCP_LIVE_AGENT=1 and install codex CLI to run (auth via its own config)",
)
def test_codex_exec_drives_lore_eq(ingested_data_dir: Path, tmp_path: Path) -> None:
    """Live opt-in: Codex CLI exec with stdio MCP server config calls
    lore_eq and returns at least one expected message-id.
    """
    import subprocess

    # Pass MCP server config inline via `-c` so codex keeps its real
    # ~/.codex/auth.json. Setting CODEX_HOME=/tmp/... would discard
    # that auth and 401 against api.openai.com.
    out = subprocess.run(
        [
            "codex",
            "exec",
            "--sandbox",
            "read-only",
            "--skip-git-repo-check",
            "-c",
            f'mcp_servers.kernel_lore.command="{_VENV_BIN}"',
            "-c",
            'mcp_servers.kernel_lore.args=["--transport","stdio"]',
            "-c",
            f'mcp_servers.kernel_lore.env={{KLMCP_DATA_DIR="{ingested_data_dir}"}}',
            (
                "Use the kernel_lore MCP server's lore_eq tool with "
                "field=from_addr and value=alice@example.com. List the "
                "returned message_ids verbatim, one per line."
            ),
        ],
        capture_output=True,
        text=True,
        timeout=180,
        env=os.environ.copy(),
        check=False,
    )
    combined = (out.stdout or "") + (out.stderr or "")
    assert "m1@x" in combined or "m2@x" in combined, combined[:2000]


def _unused_python_check() -> None:
    """Sanity-stub: ensure pytest collects this module under py3.12+."""
    assert sys.version_info >= (3, 12)
