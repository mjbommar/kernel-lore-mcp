#!/usr/bin/env bash
# klmcp-grok-pull.sh — one tick of the ingest loop.
#
# Renders the grokmirror.conf template with real paths, runs
# `grok-pull` once (no --continuous), then touches the ingest
# trigger file so the paired path-activated systemd unit fires
# klmcp-ingest.service.
#
# Exits non-zero on grok-pull failure so systemd will count
# restarts and back off; ingest never runs against a half-pulled
# state.
#
# Env (all have defaults):
#   KLMCP_DATA_DIR — required; root of data/state.
#   KLMCP_GROKMIRROR_CONF_TEMPLATE
#                  — config template path (default: this repo's
#                    scripts/grokmirror.conf).
#   KLMCP_POST_PULL_HOOK
#                  — hook script path (default: this repo's
#                    scripts/post-pull-hook.sh; production
#                    override: /usr/local/lib/kernel-lore-mcp/
#                    post-pull-hook.sh).
#   KLMCP_INGEST_TRIGGER
#                  — path touched on success (default:
#                    $KLMCP_DATA_DIR/state/grokpull.trigger).
#
# Cross-refs: docs/ops/update-frequency.md, docs/ops/runbook.md.

set -euo pipefail

: "${KLMCP_DATA_DIR:?KLMCP_DATA_DIR must be set}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
template="${KLMCP_GROKMIRROR_CONF_TEMPLATE:-$repo_root/scripts/grokmirror.conf}"
hook="${KLMCP_POST_PULL_HOOK:-$repo_root/scripts/post-pull-hook.sh}"
trigger="${KLMCP_INGEST_TRIGGER:-$KLMCP_DATA_DIR/state/grokpull.trigger}"
last_success="$KLMCP_DATA_DIR/state/grokpull.last_success"

mkdir -p \
    "$KLMCP_DATA_DIR/shards" \
    "$KLMCP_DATA_DIR/objstore" \
    "$KLMCP_DATA_DIR/state" \
    "$KLMCP_DATA_DIR/logs"

if ! command -v grok-pull >/dev/null 2>&1; then
    echo "ERROR: grok-pull not on PATH; install grokmirror (uv tool install grokmirror) first" >&2
    exit 2
fi

if [[ ! -f "$template" ]]; then
    echo "ERROR: grokmirror config template $template not found" >&2
    exit 2
fi

if [[ ! -x "$hook" ]]; then
    echo "ERROR: post-pull hook $hook missing or not executable" >&2
    exit 2
fi

# Render the template into a run-local config. grokmirror's
# ConfigParser does NOT expand env vars, so we do the substitution
# ourselves. Use a sentinel (@VAR@) rather than ${VAR} to avoid
# clashing with any git-config fields the user might embed later.
conf="$KLMCP_DATA_DIR/state/grokmirror.rendered.conf"
sed \
    -e "s|@KLMCP_DATA_DIR@|$KLMCP_DATA_DIR|g" \
    -e "s|@KLMCP_POST_PULL_HOOK@|$hook|g" \
    "$template" > "$conf"

started=$(date -u +%s)
echo "[klmcp-grok-pull] starting tick, conf=$conf data_dir=$KLMCP_DATA_DIR"

# `-n` = skip mtime check (we trust kernel.org's etag + our 5-min
# cadence to be cache-polite). We do NOT pass `-o` / --continuous;
# the systemd timer drives the cadence, so each invocation is a
# single pull.
grok-pull -c "$conf" -n

ended=$(date -u +%s)
elapsed=$(( ended - started ))

# Touch trigger for path-unit debounce; record last-success for
# /status freshness probes.
date -u +%s > "$last_success"
touch "$trigger"

echo "[klmcp-grok-pull] OK — ${elapsed}s; trigger=$trigger"
