#!/usr/bin/env bash
# klmcp-grok-pull.sh — one tick of the ingest loop.
#
# Invoked by the klmcp-grokmirror.service systemd unit every
# KLMCP_GROKMIRROR_INTERVAL_SECONDS (default 300s). Steps:
#
#   1. Run `grok-pull` with the config in this repo. Manifest +
#      delta packfiles only; no HTTP to lore's web surface.
#   2. On success, touch the ingest-trigger file so the paired
#      path-activated systemd unit fires `klmcp-ingest.service`.
#   3. Log last-success timestamp for /status + freshness probes.
#
# Exits non-zero on grok-pull failure so systemd will count
# restarts and back off; ingest never runs against a half-pulled
# state.
#
# Env (all have defaults):
#   KLMCP_DATA_DIR — required; root of data/state.
#   KLMCP_GROKMIRROR_CONF — config path (default: this repo's
#                           scripts/grokmirror.conf).
#   KLMCP_INGEST_TRIGGER — path touched on success (default:
#                           $KLMCP_DATA_DIR/state/grokpull.trigger).
#
# Cross-refs: docs/ops/update-frequency.md, docs/ops/runbook.md.

set -euo pipefail

: "${KLMCP_DATA_DIR:?KLMCP_DATA_DIR must be set}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
conf="${KLMCP_GROKMIRROR_CONF:-$repo_root/scripts/grokmirror.conf}"
trigger="${KLMCP_INGEST_TRIGGER:-$KLMCP_DATA_DIR/state/grokpull.trigger}"
last_success="$KLMCP_DATA_DIR/state/grokpull.last_success"

mkdir -p \
    "$KLMCP_DATA_DIR/shards" \
    "$KLMCP_DATA_DIR/state" \
    "$KLMCP_DATA_DIR/logs"

if ! command -v grok-pull >/dev/null 2>&1; then
    echo "ERROR: grok-pull not on PATH; install grokmirror (apt/pip) first" >&2
    exit 2
fi

if [[ ! -f "$conf" ]]; then
    echo "ERROR: grokmirror config $conf not found" >&2
    exit 2
fi

started=$(date -u +%s)
echo "[klmcp-grok-pull] starting tick, conf=$conf data_dir=$KLMCP_DATA_DIR"

# --purge is intentional — grokmirror handles upstream repack
# safely. --verbose is kept off to avoid log spam; failures surface
# via the exit code + systemd journal.
KLMCP_DATA_DIR="$KLMCP_DATA_DIR" grok-pull --config "$conf" --no-purge

ended=$(date -u +%s)
elapsed=$(( ended - started ))

# Touch trigger for path-unit debounce; record last-success for
# /status freshness probes.
date -u +%s > "$last_success"
touch "$trigger"

echo "[klmcp-grok-pull] OK — ${elapsed}s; trigger=$trigger"
