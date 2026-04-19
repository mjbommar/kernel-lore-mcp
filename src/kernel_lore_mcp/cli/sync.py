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
import shutil
import sys


_BIN_NAME = "kernel-lore-sync"


def _find_rust_binary() -> str:
    """Locate the ``kernel-lore-sync`` Rust binary.

    Resolution order:
      1. ``$KLMCP_SYNC_BINARY`` (explicit override — useful for dev
         trees where the binary lives under ``target/release``).
      2. The first ``kernel-lore-sync`` on ``$PATH``.
      3. Fail loudly with a message pointing at the install docs.
    """
    override = os.environ.get("KLMCP_SYNC_BINARY")
    if override:
        if not os.path.isfile(override) or not os.access(override, os.X_OK):
            raise SystemExit(
                f"KLMCP_SYNC_BINARY={override} is not an executable file"
            )
        return override
    found = shutil.which(_BIN_NAME)
    if found:
        return found
    raise SystemExit(
        "kernel-lore-sync binary not found on PATH. "
        "Install via `cargo install --path .` from the repo root, "
        "or point KLMCP_SYNC_BINARY at the built binary "
        "(e.g. target/release/kernel-lore-sync)."
    )


def main() -> None:
    """Exec the Rust binary with the arguments we were invoked with.

    Using `os.execvp` (not `subprocess.run`) so the Rust binary
    inherits our stdin/stdout/stderr and exit code directly — no
    wrapper-layer translation of tracing JSON lines or systemd signal
    handling. This also means the Rust binary's `--help` renders as
    if you invoked it directly.
    """
    bin_path = _find_rust_binary()
    os.execvp(bin_path, [bin_path, *sys.argv[1:]])


if __name__ == "__main__":
    main()
