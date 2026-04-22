"""Hosted-profile logging tests.

These pin the operator-facing runtime posture introduced for the
hosted-readiness plan:
  * hosted mode explicitly quiets noisy third-party loggers
  * local mode can restore verbose library logs for debugging
  * CLI `--mode` overrides env and reaches both Settings and logging
"""

from __future__ import annotations

import logging
from pathlib import Path

from kernel_lore_mcp import __main__ as cli
from kernel_lore_mcp.logging_ import configure


def test_configure_hosted_mode_quiets_noisy_loggers() -> None:
    configure(transport="http", mode="hosted", level="INFO")

    assert logging.getLogger("uvicorn.access").level == logging.WARNING
    assert logging.getLogger("httpx").level == logging.WARNING
    assert logging.getLogger("httpcore").level == logging.WARNING
    assert logging.getLogger("fastmcp.server.context.to_client").level == logging.WARNING


def test_configure_local_mode_restores_requested_log_level() -> None:
    configure(transport="http", mode="hosted", level="INFO")
    configure(transport="http", mode="local", level="DEBUG")

    assert logging.getLogger("uvicorn.access").level == logging.DEBUG
    assert logging.getLogger("httpx").level == logging.DEBUG
    assert logging.getLogger("httpcore").level == logging.DEBUG
    assert logging.getLogger("fastmcp.server.context.to_client").level == logging.DEBUG


def test_serve_cli_mode_overrides_env_and_reaches_logging(
    monkeypatch,
    tmp_path: Path,
) -> None:
    recorded: dict[str, object] = {}

    class DummyServer:
        def run(self, **kwargs: object) -> None:
            recorded["run_kwargs"] = kwargs

    def fake_configure_logging(*, transport: str, mode: str, level: str) -> None:
        recorded["configure_logging"] = {
            "transport": transport,
            "mode": mode,
            "level": level,
        }

    def fake_build_server(settings):
        recorded["settings_mode"] = settings.mode
        recorded["settings_data_dir"] = str(settings.data_dir)
        return DummyServer()

    monkeypatch.setattr(cli, "configure_logging", fake_configure_logging)
    monkeypatch.setenv("KLMCP_DATA_DIR", str(tmp_path / "data"))
    monkeypatch.setenv("KLMCP_MODE", "local")

    import kernel_lore_mcp.server as server_mod

    monkeypatch.setattr(server_mod, "build_server", fake_build_server)

    rc = cli.main(
        [
            "serve",
            "--transport",
            "http",
            "--mode",
            "hosted",
            "--port",
            "9099",
        ]
    )

    assert rc == 0
    assert recorded["settings_mode"] == "hosted"
    assert recorded["configure_logging"] == {
        "transport": "http",
        "mode": "hosted",
        "level": "INFO",
    }
    assert recorded["run_kwargs"] == {
        "transport": "http",
        "host": "127.0.0.1",
        "port": 9099,
    }
