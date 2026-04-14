"""End-to-end tests for the PyO3 surface.

Builds a synthetic public-inbox-style bare git repo, runs
`_core.ingest_shard`, then exercises every `_core.Reader` method
against the real Parquet + compressed store on disk.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from kernel_lore_mcp import _core
from tests.python.fixtures import make_synthetic_shard


@pytest.fixture
def ingested(tmp_path: Path) -> tuple[Path, dict]:
    """Ingest a two-message synthetic shard; return (data_dir, stats)."""
    shard_dir = tmp_path / "shards" / "0.git"
    shard_dir.parent.mkdir(parents=True)
    make_synthetic_shard(shard_dir)

    data_dir = tmp_path / "data"
    data_dir.mkdir()

    stats = _core.ingest_shard(
        data_dir=data_dir,
        shard_path=shard_dir,
        list="linux-cifs",
        shard="0",
        run_id="run-0001",
    )
    return data_dir, stats


def test_ingest_stats_shape(ingested: tuple[Path, dict]) -> None:
    _data, stats = ingested
    assert stats["ingested"] == 2
    assert stats["skipped_no_m"] == 0
    assert stats["skipped_empty"] == 0
    assert stats["skipped_no_mid"] == 0
    assert stats["parquet_path"] is not None
    assert stats["parquet_path"].endswith("run-0001.parquet")


def test_fetch_message_roundtrip(ingested: tuple[Path, dict]) -> None:
    data_dir, _stats = ingested
    reader = _core.Reader(data_dir)
    row = reader.fetch_message("m1@x")
    assert row is not None
    assert row["message_id"] == "m1@x"
    assert row["list"] == "linux-cifs"
    assert row["has_patch"] is True
    assert row["series_version"] == 3
    assert row["series_index"] == 1
    assert row["series_total"] == 2
    assert row["is_cover_letter"] is False
    assert any("carol@example.com" in r for r in row["reviewed_by"])
    assert any("stable@" in s for s in row["cc_stable"])
    assert "fs/smb/server/smbacl.c" in row["touched_files"]
    assert "smb_check_perm_dacl" in row["touched_functions"]
    assert row["schema_version"] == 1


def test_fetch_message_none_when_absent(ingested: tuple[Path, dict]) -> None:
    data_dir, _stats = ingested
    reader = _core.Reader(data_dir)
    assert reader.fetch_message("does-not-exist@x") is None


def test_activity_by_file_returns_only_matching_rows(ingested: tuple[Path, dict]) -> None:
    data_dir, _stats = ingested
    reader = _core.Reader(data_dir)
    rows = reader.activity(file="fs/smb/server/smbacl.c")
    assert len(rows) == 1
    assert rows[0]["message_id"] == "m1@x"

    empty = reader.activity(file="no/such/file.c")
    assert empty == []


def test_activity_by_function(ingested: tuple[Path, dict]) -> None:
    data_dir, _stats = ingested
    reader = _core.Reader(data_dir)
    rows = reader.activity(function="smb2_create")
    assert len(rows) == 1
    assert rows[0]["message_id"] == "m2@x"


def test_activity_list_filter(ingested: tuple[Path, dict]) -> None:
    data_dir, _stats = ingested
    reader = _core.Reader(data_dir)
    rows = reader.activity(function="smb2_create", list="linux-cifs")
    assert len(rows) == 1
    assert reader.activity(function="smb2_create", list="linux-nfs") == []


def test_activity_since_filter(ingested: tuple[Path, dict]) -> None:
    data_dir, _stats = ingested
    reader = _core.Reader(data_dir)
    # Set the cutoff AFTER m1's date but before m2's; only m2 survives.
    # m1 = 12:00:00 UTC, m2 = 12:05:00 UTC on 2026-04-14.
    m2_nanos = reader.fetch_message("m2@x")["date_unix_ns"]
    cutoff = m2_nanos - 1
    rows = reader.activity(file=None, function=None, since_unix_ns=cutoff)
    assert {r["message_id"] for r in rows} == {"m2@x"}


def test_expand_citation_via_fixes_sha(ingested: tuple[Path, dict]) -> None:
    data_dir, _stats = ingested
    reader = _core.Reader(data_dir)
    rows = reader.expand_citation("deadbeef01234567")
    assert len(rows) == 1
    assert rows[0]["message_id"] == "m1@x"


def test_expand_citation_via_message_id(ingested: tuple[Path, dict]) -> None:
    data_dir, _stats = ingested
    reader = _core.Reader(data_dir)
    rows = reader.expand_citation("<m2@x>")
    assert len(rows) == 1
    assert rows[0]["message_id"] == "m2@x"


def test_fetch_body_roundtrip(ingested: tuple[Path, dict]) -> None:
    data_dir, _stats = ingested
    reader = _core.Reader(data_dir)

    body = reader.fetch_body("m1@x")
    assert body is not None
    assert isinstance(body, bytes)
    # Uncompressed body sha256 stored on the row matches the raw bytes
    # we just decompressed.
    row = reader.fetch_message("m1@x")
    assert row is not None
    assert len(body) == row["body_length"]

    import hashlib

    assert hashlib.sha256(body).hexdigest() == row["body_sha256"]

    # Missing message-id returns None.
    assert reader.fetch_body("does-not-exist@x") is None


def test_generation_bumped_on_ingest(tmp_path: Path) -> None:
    shard_dir = tmp_path / "shards" / "0.git"
    shard_dir.parent.mkdir(parents=True)
    make_synthetic_shard(shard_dir)

    data_dir = tmp_path / "data"
    data_dir.mkdir()

    gen_file = data_dir / "state" / "generation"
    assert not gen_file.exists()

    _core.ingest_shard(
        data_dir=data_dir,
        shard_path=shard_dir,
        list="linux-cifs",
        shard="0",
        run_id="run-0001",
    )

    assert gen_file.exists()
    assert gen_file.read_text().strip() == "1"


def test_incremental_skip_on_second_run(tmp_path: Path) -> None:
    shard_dir = tmp_path / "shards" / "0.git"
    shard_dir.parent.mkdir(parents=True)
    make_synthetic_shard(shard_dir)

    data_dir = tmp_path / "data"
    data_dir.mkdir()

    first = _core.ingest_shard(
        data_dir=data_dir,
        shard_path=shard_dir,
        list="linux-cifs",
        shard="0",
        run_id="run-a",
    )
    second = _core.ingest_shard(
        data_dir=data_dir,
        shard_path=shard_dir,
        list="linux-cifs",
        shard="0",
        run_id="run-b",
    )
    assert first["ingested"] == 2
    assert second["ingested"] == 0
    assert second["parquet_path"] is None
