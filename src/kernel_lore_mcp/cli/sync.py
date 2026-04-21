"""kernel-lore-sync — Python wrapper around the Rust binary.

Internalizes the grokmirror + kernel-lore-ingest pipeline into one
process: fetches the lore manifest, diffs against the local cache,
gix-fetches changed shards, ingests them, and bumps the generation
marker — all under one writer lock.

This wrapper exists so `uv tool install kernel-lore-mcp` users get
the sync CLI without having to locate the compiled Rust binary on
their PATH. It exec's the Rust binary that ships inside the wheel's
``bin/`` directory (see `pyproject.toml` `[project.scripts]`).

Usage:
    kernel-lore-sync \\
        --data-dir $KLMCP_DATA_DIR \\
        [--manifest-url https://lore.kernel.org/manifest.js.gz] \\
        [--include '/lkml/*'] [--exclude '/private/*']

See `src/bin/sync.rs` for the full flag list and exit-code contract.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

_BIN_NAME = "kernel-lore-sync"


def _is_executable_file(path: str | Path) -> bool:
    p = Path(path)
    return p.is_file() and os.access(p, os.X_OK)


def _same_path(a: str | Path | None, b: str | Path | None) -> bool:
    if not a or not b:
        return False
    a_path = Path(a)
    b_path = Path(b)
    try:
        return a_path.samefile(b_path)
    except OSError:
        return a_path.resolve() == b_path.resolve()


def _current_wrapper_path() -> str | None:
    argv0 = sys.argv[0]
    if argv0:
        argv0_path = Path(argv0)
        if argv0_path.exists():
            return str(argv0_path.resolve())
    return None


def _dev_binary_candidates() -> list[str]:
    """Likely local build outputs for `uv run kernel-lore-sync`.

    `uv run` puts `.venv/bin` first on PATH, so a plain PATH search
    resolves back to the console-script wrapper and recurses forever.
    When the caller is inside a source checkout, prefer an already-
    built `target/release/kernel-lore-sync` from the cwd or one of its
    ancestors.
    """
    out: list[str] = []
    seen: set[str] = set()
    cur = Path.cwd().resolve()
    while True:
        cand = str(cur / "target" / "release" / _BIN_NAME)
        if cand not in seen:
            out.append(cand)
            seen.add(cand)
        parent = cur.parent
        if parent == cur:
            break
        cur = parent
    return out


def _path_binary_candidates() -> list[str]:
    out: list[str] = []
    for entry in os.get_exec_path():
        if not entry:
            continue
        out.append(str(Path(entry) / _BIN_NAME))
    return out


def _find_rust_binary() -> str:
    """Locate the ``kernel-lore-sync`` Rust binary.

    Resolution order:
      1. ``$KLMCP_SYNC_BINARY`` (explicit override — useful for dev
         trees where the binary lives under ``target/release``).
      2. A nearby `target/release/kernel-lore-sync` in the current
         working tree or one of its ancestors.
      3. The first executable ``kernel-lore-sync`` on ``$PATH`` that
         is NOT this console-script wrapper.
      4. Fail loudly with a message pointing at the install docs.
    """
    current_wrapper = _current_wrapper_path()
    override = os.environ.get("KLMCP_SYNC_BINARY")
    if override:
        if not _is_executable_file(override):
            raise SystemExit(
                f"KLMCP_SYNC_BINARY={override} is not an executable file"
            )
        if _same_path(override, current_wrapper):
            raise SystemExit(
                "KLMCP_SYNC_BINARY points at the Python console-script wrapper, "
                "not the Rust binary. Point it at the built binary instead "
                "(e.g. target/release/kernel-lore-sync)."
            )
        return override

    for cand in _dev_binary_candidates():
        if _is_executable_file(cand) and not _same_path(cand, current_wrapper):
            return cand

    for cand in _path_binary_candidates():
        if _is_executable_file(cand) and not _same_path(cand, current_wrapper):
            return cand

    raise SystemExit(
        "kernel-lore-sync binary not found on PATH. "
        "If you're in a source checkout, build it with "
        "`cargo build --release --bin kernel-lore-sync`. "
        "Otherwise install via `cargo install --path .`, or point "
        "KLMCP_SYNC_BINARY at the built binary "
        "(e.g. target/release/kernel-lore-sync)."
    )


def main() -> None:
    """Exec the Rust binary with the arguments we were invoked with.

    Using `os.execv` (not `subprocess.run`) so the Rust binary
    inherits our stdin/stdout/stderr and exit code directly — no
    wrapper-layer translation of tracing JSON lines or systemd signal
    handling. This also means the Rust binary's `--help` renders as
    if you invoked it directly.
    """
    bin_path = _find_rust_binary()
    os.execv(bin_path, [bin_path, *sys.argv[1:]])  # noqa: S606


if __name__ == "__main__":
    main()
