"""kernel-lore-mcp status subcommand — JSON shape + freshness math.

Three invariants pinned:
 1. Empty data_dir returns generation=0 with a 'note' and no
    freshness_ok bool.
 2. Populated data_dir after ingest returns generation >= 1,
    last_ingest_utc is a valid ISO 8601 string, age is small,
    freshness_ok is True.
 3. Backdating the generation file flips freshness_ok to False.

Same shape as the /status HTTP route, minus per_list. Zero
dependency on booting the server.
"""

from __future__ import annotations

import io
import json
import os
import time
from contextlib import redirect_stdout
from pathlib import Path

from kernel_lore_mcp import _core
from kernel_lore_mcp.__main__ import main as cli_main
from tests.python.fixtures import make_synthetic_shard


def _run(argv: list[str]) -> dict:
    buf = io.StringIO()
    with redirect_stdout(buf):
        rc = cli_main(argv)
    assert rc == 0, f"cli exit {rc}"
    return json.loads(buf.getvalue())


def test_status_on_empty_data_dir(tmp_path: Path) -> None:
    data = tmp_path / "data"
    data.mkdir()
    out = _run(["status", "--data-dir", str(data)])
    assert out["service"] == "kernel-lore-mcp"
    assert out["generation"] == 0
    assert out["last_ingest_utc"] is None
    assert out["last_ingest_age_seconds"] is None
    assert out["freshness_ok"] is None
    assert out["tier_generations"]["path_vocab"] is None
    assert out["tier_status"]["path_vocab"] == "marker absent"
    assert out["writer_lock_present"] is False
    assert out["sync_active"] is False
    assert out["sync"] is None
    assert "note" in out


def test_status_after_real_ingest(tmp_path: Path) -> None:
    shard_dir = tmp_path / "shards" / "0.git"
    shard_dir.parent.mkdir(parents=True)
    make_synthetic_shard(shard_dir)
    data = tmp_path / "data"
    data.mkdir()
    _core.ingest_shard(
        data_dir=data,
        shard_path=shard_dir,
        list="linux-cifs",
        shard="0",
        run_id="status-cli",
    )

    out = _run(["status", "--data-dir", str(data)])
    assert out["generation"] >= 1
    assert out["last_ingest_utc"] is not None
    assert out["last_ingest_age_seconds"] is not None
    assert out["last_ingest_age_seconds"] >= 0
    assert out["last_ingest_age_seconds"] < 120
    assert out["freshness_ok"] is True
    assert out["tier_generations"]["trigram"] == out["generation"]
    assert out["tier_status"]["trigram"] == "in sync"
    assert out["tier_generations"]["path_vocab"] is None
    assert out["tier_status"]["path_vocab"] == "marker absent"
    assert out["writer_lock_present"] is False
    assert out["sync_active"] is False


def test_status_reports_stale_after_backdated_mtime(tmp_path: Path) -> None:
    shard_dir = tmp_path / "shards" / "0.git"
    shard_dir.parent.mkdir(parents=True)
    make_synthetic_shard(shard_dir)
    data = tmp_path / "data"
    data.mkdir()
    _core.ingest_shard(
        data_dir=data,
        shard_path=shard_dir,
        list="linux-cifs",
        shard="0",
        run_id="status-stale",
    )

    gen_file = data / "state" / "generation"
    # 3x default interval (300) + buffer.
    stale_mtime = time.time() - (3 * 300) - 60
    os.utime(gen_file, (stale_mtime, stale_mtime))

    out = _run(["status", "--data-dir", str(data)])
    assert out["freshness_ok"] is False
    assert out["last_ingest_age_seconds"] > 3 * 300


def test_status_reports_behind_path_vocab_marker(tmp_path: Path) -> None:
    shard_dir = tmp_path / "shards" / "0.git"
    shard_dir.parent.mkdir(parents=True)
    make_synthetic_shard(shard_dir)
    data = tmp_path / "data"
    data.mkdir()
    _core.ingest_shard(
        data_dir=data,
        shard_path=shard_dir,
        list="linux-cifs",
        shard="0",
        run_id="status-path-vocab",
    )

    state_dir = data / "state"
    (data / "paths").mkdir()
    (data / "paths" / "vocab.txt").write_text("fs/smb/server/smbacl.c\n")
    (state_dir / "path_vocab.generation").write_text("0\n")

    out = _run(["status", "--data-dir", str(data)])
    assert out["tier_generations"]["path_vocab"] == 0
    assert out["tier_status"]["path_vocab"].startswith("behind by ")
