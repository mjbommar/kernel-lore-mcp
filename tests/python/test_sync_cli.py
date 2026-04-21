from __future__ import annotations

from pathlib import Path

import pytest

from kernel_lore_mcp.cli import sync as sync_cli


def _make_executable(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\nexit 0\n")
    path.chmod(0o755)
    return path


def test_find_rust_binary_prefers_nearby_target_release(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repo = tmp_path / "repo"
    wrapper = _make_executable(repo / ".venv" / "bin" / "kernel-lore-sync")
    built = _make_executable(repo / "target" / "release" / "kernel-lore-sync")

    monkeypatch.chdir(repo)
    monkeypatch.setenv("PATH", str(wrapper.parent))
    monkeypatch.setattr(sync_cli.sys, "argv", [str(wrapper)])

    assert sync_cli._find_rust_binary() == str(built)


def test_find_rust_binary_skips_console_script_on_path(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    wrapper = _make_executable(tmp_path / "venv-bin" / "kernel-lore-sync")
    real = _make_executable(tmp_path / "real-bin" / "kernel-lore-sync")

    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("PATH", f"{wrapper.parent}:{real.parent}")
    monkeypatch.setattr(sync_cli.sys, "argv", [str(wrapper)])

    assert sync_cli._find_rust_binary() == str(real)


def test_find_rust_binary_fails_loudly_when_only_wrapper_exists(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    wrapper = _make_executable(tmp_path / "venv-bin" / "kernel-lore-sync")

    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("PATH", str(wrapper.parent))
    monkeypatch.setattr(sync_cli.sys, "argv", [str(wrapper)])

    with pytest.raises(SystemExit, match="binary not found on PATH"):
        sync_cli._find_rust_binary()


def test_find_rust_binary_rejects_override_pointing_at_wrapper(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    wrapper = _make_executable(tmp_path / "venv-bin" / "kernel-lore-sync")

    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("KLMCP_SYNC_BINARY", str(wrapper))
    monkeypatch.setattr(sync_cli.sys, "argv", [str(wrapper)])

    with pytest.raises(SystemExit, match="console-script wrapper"):
        sync_cli._find_rust_binary()
