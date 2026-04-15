"""HTTP transport end-to-end smoke.

Spawns `kernel-lore-mcp serve --transport http --host 127.0.0.1
--port <ephemeral>` in a subprocess, connects a fastmcp.Client to
the /mcp endpoint, lists tools, calls one, reads one resource,
closes.

Confirms the runbook's "streamable HTTP for hosted" path works
end-to-end. If HTTP has a regression, it fails here instead of in
someone's production deploy.

Skipped when the project venv or ingest binary can't be located —
this is a deployment-readiness test, not a unit test.
"""

from __future__ import annotations

import asyncio
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

import pytest
from fastmcp import Client

from kernel_lore_mcp import _core
from tests.python.fixtures import make_synthetic_shard

REPO_ROOT = Path(__file__).resolve().parents[2]


def _free_port() -> int:
    """Reserve + release a port so the subprocess can rebind it."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _find_server_cmd() -> list[str] | None:
    cand = REPO_ROOT / ".venv" / "bin" / "kernel-lore-mcp"
    if cand.exists():
        return [str(cand)]
    which = shutil.which("kernel-lore-mcp")
    if which:
        return [which]
    return None


@pytest.mark.asyncio
async def test_http_transport_round_trip(tmp_path: Path) -> None:
    server_cmd = _find_server_cmd()
    if server_cmd is None:
        pytest.skip("kernel-lore-mcp binary not in .venv or on PATH")

    # Seed a corpus so tool calls have something to return.
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
        run_id="http-transport",
    )

    port = _free_port()
    env = os.environ.copy()
    env["KLMCP_DATA_DIR"] = str(data_dir)
    env["KLMCP_LOG_LEVEL"] = "WARNING"

    proc = subprocess.Popen(
        [*server_cmd, "serve", "--transport", "http", "--host", "127.0.0.1", "--port", str(port)],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        # Poll until the server is listening, capped at ~5s.
        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline:
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
                s.settimeout(0.2)
                try:
                    s.connect(("127.0.0.1", port))
                    break
                except OSError:
                    await asyncio.sleep(0.1)
        else:
            stdout, stderr = proc.communicate(timeout=2)
            pytest.fail(
                f"server never bound port {port}\n"
                f"stdout: {stdout.decode(errors='replace')}\n"
                f"stderr: {stderr.decode(errors='replace')}"
            )

        url = f"http://127.0.0.1:{port}/mcp/"
        async with Client(url) as c:
            tools = {t.name for t in await c.list_tools()}
            assert "lore_eq" in tools, f"tools missing lore_eq: {tools}"
            assert "lore_message" in tools

            # Tool call round-trip.
            result = await c.call_tool(
                "lore_eq",
                {"field": "from_addr", "value": "alice@example.com"},
            )
            mids = {h.message_id for h in result.data.results}
            assert mids == {"m1@x", "m2@x"}, f"unexpected hits: {mids}"

            # Resource read round-trip (Phase 10).
            contents = await c.read_resource("lore://message/m1@x")
            assert contents
            first = contents[0]
            # TextResourceContents carries `.text`.
            assert hasattr(first, "text")
            assert "Message-ID: <m1@x>" in first.text
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=2)


@pytest.mark.asyncio
async def test_status_endpoint_over_http(tmp_path: Path) -> None:
    """The /status route is served on the same port as the MCP
    endpoint and should return freshness_ok=true right after ingest.
    """
    server_cmd = _find_server_cmd()
    if server_cmd is None:
        pytest.skip("kernel-lore-mcp binary not in .venv or on PATH")

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
        run_id="status-http",
    )

    port = _free_port()
    env = os.environ.copy()
    env["KLMCP_DATA_DIR"] = str(data_dir)
    env["KLMCP_LOG_LEVEL"] = "WARNING"

    proc = subprocess.Popen(
        [*server_cmd, "serve", "--transport", "http", "--host", "127.0.0.1", "--port", str(port)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    try:
        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline:
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
                s.settimeout(0.2)
                try:
                    s.connect(("127.0.0.1", port))
                    break
                except OSError:
                    await asyncio.sleep(0.1)
        else:
            pytest.fail(f"server never bound port {port}")

        import httpx

        async with httpx.AsyncClient(timeout=5.0) as http:
            r = await http.get(f"http://127.0.0.1:{port}/status")
            assert r.status_code == 200
            body = r.json()
            assert body["service"] == "kernel-lore-mcp"
            assert body["generation"] >= 1
            assert body["freshness_ok"] is True
            assert body["configured_interval_seconds"] == 300

            m = await http.get(f"http://127.0.0.1:{port}/metrics")
            assert m.status_code == 200
            assert "kernel_lore_mcp_freshness_ok 1.0" in m.text
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=2)


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-x", "-q"]))
