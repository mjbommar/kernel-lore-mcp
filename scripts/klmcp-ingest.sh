#!/usr/bin/env bash
# klmcp-ingest.sh — ingest every mirrored shard into the three-tier
# index. Paired with klmcp-grok-pull.sh via a path-activated systemd
# unit (see scripts/systemd/klmcp-ingest.path).
#
# Runs the Rust-side ingest pipeline through the project's installed
# entry point, which:
#   * acquires the exclusive writer flock (state.rs::acquire_writer_lock),
#   * walks every <list>/<shard>.git under $KLMCP_DATA_DIR/shards,
#   * appends new messages to the compressed store + metadata Parquet,
#   * rebuilds trigram + BM25 + embedding tiers for touched shards,
#   * atomically bumps state/generation (query readers reload on next
#     request).
#
# Exits non-zero on failure so systemd can count restarts. The writer
# flock means a racing ingest returns fast with "another ingest is
# already running" — safe no-op.
#
# Env:
#   KLMCP_DATA_DIR — required; root of data/state.
#   KLMCP_INGEST_DEBOUNCE_SECONDS — min gap between consecutive ingest
#                                   runs regardless of trigger rate
#                                   (default 30).
#
# Cross-refs: docs/ops/update-frequency.md, docs/ops/runbook.md.

set -euo pipefail

: "${KLMCP_DATA_DIR:?KLMCP_DATA_DIR must be set}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
debounce="${KLMCP_INGEST_DEBOUNCE_SECONDS:-30}"
last_run="$KLMCP_DATA_DIR/state/ingest.last_run"

mkdir -p "$KLMCP_DATA_DIR/state" "$KLMCP_DATA_DIR/logs"

now=$(date -u +%s)
if [[ -f "$last_run" ]]; then
    prev=$(cat "$last_run" 2>/dev/null || echo 0)
    age=$(( now - prev ))
    if (( age < debounce )); then
        echo "[klmcp-ingest] debounced ($age s < $debounce s since last run)"
        exit 0
    fi
fi

# Locate the ingest CLI. Prefer a system-installed binary; fall back
# to the project venv (the canonical layout per CLAUDE.md).
if command -v kernel-lore-ingest >/dev/null 2>&1; then
    ingest="kernel-lore-ingest"
elif [[ -x "$repo_root/.venv/bin/kernel-lore-ingest" ]]; then
    ingest="$repo_root/.venv/bin/kernel-lore-ingest"
else
    echo "ERROR: kernel-lore-ingest not found on PATH or in .venv/bin" >&2
    exit 2
fi

echo "[klmcp-ingest] starting, data_dir=$KLMCP_DATA_DIR ingest=$ingest"
started=$(date -u +%s)

"$ingest" --data-dir "$KLMCP_DATA_DIR" --shards-root "$KLMCP_DATA_DIR/shards"

ended=$(date -u +%s)
elapsed=$(( ended - started ))
echo "$ended" > "$last_run"
echo "[klmcp-ingest] OK — ${elapsed}s"
