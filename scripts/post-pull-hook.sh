#!/usr/bin/env bash
# post-pull-hook.sh — grokmirror post_update_hook.
#
# Runs after grok-pull finishes. Touches the ingest trigger file
# that the klmcp-ingest.path systemd unit watches, so ingest fires
# exactly once per successful pull regardless of how many shards
# changed.
#
# Installed at /usr/local/lib/kernel-lore-mcp/post-pull-hook.sh
# per scripts/grokmirror.conf.

set -euo pipefail

: "${KLMCP_DATA_DIR:?KLMCP_DATA_DIR must be set}"

trigger="${KLMCP_INGEST_TRIGGER:-$KLMCP_DATA_DIR/state/grokpull.trigger}"
mkdir -p "$(dirname "$trigger")"
touch "$trigger"

# grokmirror passes the list of changed repos as CLI args; we log
# but don't act on them — the ingest scanner re-walks every shard
# anyway (incremental from last-OID, so cost is zero for unchanged
# shards).
echo "[post-pull-hook] updated shards: $* — ingest trigger touched"
