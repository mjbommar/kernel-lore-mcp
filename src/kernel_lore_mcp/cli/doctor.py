"""kernel-lore-doctor — inspect index + shard health and optionally heal."""

from __future__ import annotations

import os
import sys
from pathlib import Path

_BIN_NAME = "kernel-lore-doctor"
_DEV_TREE_SENTINELS = ("Cargo.toml", "pyproject.toml", "src/bin/sync.rs")
_DEV_TREE_WATCH_GLOBS = (
    "Cargo.toml",
    "Cargo.lock",
    "pyproject.toml",
    "build.rs",
    "src/**/*.rs",
)


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


def _source_checkout_root(start: Path | None = None) -> Path | None:
    cur = (start or Path.cwd()).resolve()
    while True:
        if all((cur / rel).exists() for rel in _DEV_TREE_SENTINELS):
            return cur
        parent = cur.parent
        if parent == cur:
            return None
        cur = parent


def _latest_source_mtime_ns(root: Path) -> int:
    latest = 0
    for rel in _DEV_TREE_WATCH_GLOBS:
        for path in root.glob(rel):
            try:
                latest = max(latest, path.stat().st_mtime_ns)
            except OSError:
                continue
    return latest


def _dev_binary_is_stale(binary_path: str | Path) -> bool:
    root = _source_checkout_root()
    if root is None:
        return False
    try:
        binary_mtime = Path(binary_path).stat().st_mtime_ns
    except OSError:
        return False
    latest_source = _latest_source_mtime_ns(root)
    return latest_source > binary_mtime


def _path_binary_candidates() -> list[str]:
    out: list[str] = []
    for entry in os.get_exec_path():
        if not entry:
            continue
        out.append(str(Path(entry) / _BIN_NAME))
    return out


def _find_rust_binary() -> str:
    current_wrapper = _current_wrapper_path()
    override = os.environ.get("KLMCP_DOCTOR_BINARY")
    if override:
        if not _is_executable_file(override):
            raise SystemExit(
                f"KLMCP_DOCTOR_BINARY={override} is not an executable file"
            )
        if _same_path(override, current_wrapper):
            raise SystemExit(
                "KLMCP_DOCTOR_BINARY points at the Python console-script wrapper, "
                "not the Rust binary. Point it at the built binary instead "
                "(e.g. target/release/kernel-lore-doctor)."
            )
        return override

    for cand in _dev_binary_candidates():
        if _is_executable_file(cand) and not _same_path(cand, current_wrapper):
            if _dev_binary_is_stale(cand):
                raise SystemExit(
                    f"{cand} is older than the source checkout. "
                    "Rebuild it with `cargo build --release --bin kernel-lore-doctor` "
                    "or point KLMCP_DOCTOR_BINARY at a freshly built binary."
                )
            return cand

    for cand in _path_binary_candidates():
        if _is_executable_file(cand) and not _same_path(cand, current_wrapper):
            return cand

    raise SystemExit(
        "kernel-lore-doctor binary not found on PATH. "
        "If you're in a source checkout, build it with "
        "`cargo build --release --bin kernel-lore-doctor`. "
        "Otherwise install via `cargo install --path .`, or point "
        "KLMCP_DOCTOR_BINARY at the built binary "
        "(e.g. target/release/kernel-lore-doctor)."
    )


def main() -> None:
    bin_path = _find_rust_binary()
    os.execv(bin_path, [bin_path, *sys.argv[1:]])  # noqa: S606


if __name__ == "__main__":
    main()
