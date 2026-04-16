"""kernel-lore-ingest — walk grokmirror shards + ingest them.

Python wrapper over `_core.ingest_shard`. Exposed as a wheel-shipped
console script so `uv tool install kernel-lore-mcp` gives you a
working ingest path without needing a Rust toolchain.

The Rust-side binary (``src/bin/ingest.rs``) is the fast path for
hosted deploys — it rayon-fans-out across shards and shares one
tantivy IndexWriter. This Python variant is serial; correct but
slower. For a personal-scoped 5-list mirror the difference is a few
minutes, not worth requiring everyone to build Rust.

Usage (same flags as the Rust binary):

    kernel-lore-ingest \\
        --data-dir   $KLMCP_DATA_DIR \\
        --lore-mirror $KLMCP_DATA_DIR/shards \\
        [--list linux-cifs] \\
        [--run-id run-abc]

Walks `<lore-mirror>/<list>/git/<N>.git` for every list directory
under the mirror root (or the one named via ``--list``) and calls
``_core.ingest_shard`` on each. Idempotent: shards whose HEAD OID
matches the last-indexed OID are a no-op.
"""

from __future__ import annotations

import argparse
import logging
import os
import sys
import time
from pathlib import Path


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="kernel-lore-ingest")
    p.add_argument(
        "--data-dir",
        default=os.environ.get("KLMCP_DATA_DIR"),
        help="Project data root. Env: KLMCP_DATA_DIR.",
    )
    p.add_argument(
        "--lore-mirror",
        default=os.environ.get("KLMCP_LORE_MIRROR_DIR"),
        help="grokmirror toplevel. Env: KLMCP_LORE_MIRROR_DIR.",
    )
    p.add_argument(
        "--list",
        dest="only_list",
        default=None,
        help="Restrict to one list (e.g. linux-cifs).",
    )
    p.add_argument(
        "--run-id",
        default=None,
        help="Stable id for this run (default: auto-generated from unix ts).",
    )
    p.add_argument(
        "--log-level",
        default=os.environ.get("KLMCP_LOG_LEVEL", "INFO"),
    )
    p.add_argument(
        "--with-bm25",
        action="store_true",
        default=False,
        help=(
            "Build BM25 inline (slower). Default: skip BM25 for ~12x "
            "faster ingest; run --rebuild-bm25 afterward."
        ),
    )
    p.add_argument(
        "--rebuild-bm25",
        action="store_true",
        default=False,
        help="ONLY rebuild BM25 from the existing store, then exit.",
    )
    return p


def _discover_shards(mirror_root: Path, only_list: str | None) -> list[tuple[str, str, Path]]:
    """Enumerate `<list>/git/<N>.git` directories under the mirror.

    Returns list of `(list_name, shard_number, git_dir)` sorted so
    processing is deterministic across runs.
    """
    out: list[tuple[str, str, Path]] = []
    if not mirror_root.is_dir():
        return out
    for list_dir in sorted(p for p in mirror_root.iterdir() if p.is_dir()):
        list_name = list_dir.name
        if only_list is not None and list_name != only_list:
            continue
        git_root = list_dir / "git"
        if not git_root.is_dir():
            continue
        for shard_dir in sorted(git_root.iterdir()):
            if not shard_dir.is_dir():
                continue
            stem = shard_dir.name
            if not stem.endswith(".git"):
                continue
            shard_num = stem[: -len(".git")]
            out.append((list_name, shard_num, shard_dir))
    return out


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)

    if not args.data_dir:
        print("ERROR: --data-dir or KLMCP_DATA_DIR required", file=sys.stderr)
        return 2

    data_dir = Path(args.data_dir)
    data_dir.mkdir(parents=True, exist_ok=True)

    # --rebuild-bm25: standalone BM25 rebuild from existing store.
    if args.rebuild_bm25:
        logging.basicConfig(
            level=getattr(logging, args.log_level.upper(), logging.INFO),
            stream=sys.stderr,
            format="[%(asctime)s] %(message)s",
        )
        log = logging.getLogger("kernel-lore-ingest")
        log.info("rebuilding BM25 from store at %s", data_dir)
        started = time.monotonic()
        from kernel_lore_mcp import _core

        count = _core.rebuild_bm25(data_dir)
        log.info(
            "BM25 rebuild complete: %d docs, %.1fs",
            count,
            time.monotonic() - started,
        )
        return 0

    if not args.lore_mirror:
        print("ERROR: --lore-mirror or KLMCP_LORE_MIRROR_DIR required", file=sys.stderr)
        return 2
    lore_mirror = Path(args.lore_mirror)
    data_dir.mkdir(parents=True, exist_ok=True)

    logging.basicConfig(
        level=getattr(logging, args.log_level.upper(), logging.INFO),
        stream=sys.stderr,
        format="[%(asctime)s] %(message)s",
    )
    log = logging.getLogger("kernel-lore-ingest")

    run_id = args.run_id or f"run-{int(time.time())}"

    shards = _discover_shards(lore_mirror, args.only_list)
    if not shards:
        log.warning(
            "no shards under %s (expected <list>/git/<N>.git layout); nothing to ingest",
            lore_mirror,
        )
        return 0

    log.info(
        "ingest starting: data_dir=%s mirror=%s shards=%d run_id=%s",
        data_dir,
        lore_mirror,
        len(shards),
        run_id,
    )

    from kernel_lore_mcp import _core

    started = time.monotonic()
    total_ingested = 0
    total_failed = 0
    for i, (list_name, shard_num, shard_dir) in enumerate(shards, start=1):
        per_shard_run_id = f"{run_id}-{list_name}-{shard_num}"
        shard_started = time.monotonic()
        try:
            stats = _core.ingest_shard(
                data_dir=data_dir,
                shard_path=shard_dir,
                list=list_name,
                shard=shard_num,
                run_id=per_shard_run_id,
            )
        except Exception as exc:
            total_failed += 1
            log.error(
                "[%d/%d] shard %s/%s FAILED: %s",
                i,
                len(shards),
                list_name,
                shard_num,
                exc,
            )
            continue
        total_ingested += int(stats.get("ingested", 0) or 0)
        log.info(
            "[%d/%d] shard %s/%s ingested=%d (%.1fs)",
            i,
            len(shards),
            list_name,
            shard_num,
            stats.get("ingested", 0),
            time.monotonic() - shard_started,
        )

    log.info(
        "ingest complete: shards=%d failed=%d ingested=%d elapsed_s=%.1f",
        len(shards),
        total_failed,
        total_ingested,
        time.monotonic() - started,
    )

    return 2 if total_failed > 0 else 0


if __name__ == "__main__":
    sys.exit(main())
