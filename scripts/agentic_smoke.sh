#!/usr/bin/env bash
# Non-interactive smoke test: drive `kernel-lore-mcp` over stdio from
# the actual `claude` and `codex` CLIs and confirm both can call our
# tools end-to-end against a real ingested corpus.
#
# Hits the real Anthropic + OpenAI APIs — costs a few cents per run.
# Requires:
#   - `claude` and `codex` on $PATH (verified at start)
#   - `ANTHROPIC_API_KEY` for claude, `OPENAI_API_KEY` for codex (or
#     they'll fall back to whatever auth they have)
#   - `kernel-lore-mcp` installed into the project venv (we use
#     <repo>/.venv/bin/kernel-lore-mcp directly so the spawning agent
#     gets the right wheel)
#
# Usage:
#   scripts/agentic_smoke.sh                 # both agents
#   scripts/agentic_smoke.sh claude          # just claude
#   scripts/agentic_smoke.sh codex           # just codex

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

agent="${1:-both}"

if ! command -v claude >/dev/null && [[ "$agent" =~ ^(both|claude)$ ]]; then
    echo "ERROR: claude CLI not on PATH" >&2
    exit 2
fi
if ! command -v codex >/dev/null && [[ "$agent" =~ ^(both|codex)$ ]]; then
    echo "ERROR: codex CLI not on PATH" >&2
    exit 2
fi
if [[ ! -x "$repo_root/.venv/bin/kernel-lore-mcp" ]]; then
    echo "ERROR: $repo_root/.venv/bin/kernel-lore-mcp missing — run 'uv sync && uv run maturin develop --release' first" >&2
    exit 2
fi

work="$(mktemp -d -t klmcp-smoke-XXXX)"
trap 'rm -rf "$work"' EXIT
echo "[smoke] workspace: $work"

# 1. Build a synthetic shard + ingest it.
python_setup() {
    local data_dir="$1"
    local shard_dir="$2"
    "$repo_root/.venv/bin/python" - <<EOF
import sys
from pathlib import Path
sys.path.insert(0, "$repo_root")
from tests.python.fixtures import make_synthetic_shard
from kernel_lore_mcp import _core
shard = Path("$shard_dir")
shard.parent.mkdir(parents=True, exist_ok=True)
make_synthetic_shard(shard)
data = Path("$data_dir")
data.mkdir(parents=True, exist_ok=True)
stats = _core.ingest_shard(
    data_dir=data,
    shard_path=shard,
    list="linux-cifs",
    shard="0",
    run_id="smoke-001",
)
print(f"[smoke] ingested {stats['ingested']} messages")
EOF
}

data_dir="$work/data"
shard_dir="$work/shards/0.git"
python_setup "$data_dir" "$shard_dir"

# 2. The MCP server we want both agents to load. We point command at
#    the venv-resolved entry point so the agent doesn't need uv on
#    PATH.
mcp_cmd="$repo_root/.venv/bin/kernel-lore-mcp"

# Common prompt — shaped to force at least one tool call.
prompt='List every linux-cifs message authored by alice@example.com using the lore_eq tool with field=from_addr, value=alice@example.com. Reply with ONLY the message-ids on separate lines, no prose.'

# ---------------------------------------------------------------- claude
if [[ "$agent" =~ ^(both|claude)$ ]]; then
    claude_cfg="$work/claude.mcp.json"
    cat >"$claude_cfg" <<EOF
{
  "mcpServers": {
    "kernel-lore": {
      "type": "stdio",
      "command": "$mcp_cmd",
      "args": ["--transport", "stdio"],
      "env": {
        "KLMCP_DATA_DIR": "$data_dir"
      }
    }
  }
}
EOF
    echo
    echo "[smoke] === claude ==="
    out="$(claude \
        --print \
        --mcp-config "$claude_cfg" \
        --strict-mcp-config \
        --permission-mode bypassPermissions \
        --allowedTools "mcp__kernel-lore__lore_eq" \
        --output-format text \
        "$prompt" 2>&1)"
    echo "$out"
    if grep -qE '\bm[12]@x\b' <<<"$out"; then
        echo "[smoke] PASS: claude returned at least one expected message-id"
    else
        echo "[smoke] FAIL: claude output did not mention m1@x or m2@x"
        exit 1
    fi
fi

# ---------------------------------------------------------------- codex
# Keep the user's real CODEX_HOME so ~/.codex/auth.json works.
# Inject the MCP server via -c overrides; CODEX_HOME=/tmp/... would
# 401 against api.openai.com.
if [[ "$agent" =~ ^(both|codex)$ ]]; then
    echo
    echo "[smoke] === codex ==="
    out="$(codex exec \
        --sandbox read-only \
        --skip-git-repo-check \
        -c "mcp_servers.kernel_lore.command=\"$mcp_cmd\"" \
        -c 'mcp_servers.kernel_lore.args=["--transport","stdio"]' \
        -c "mcp_servers.kernel_lore.env={KLMCP_DATA_DIR=\"$data_dir\"}" \
        "$prompt" 2>&1 || true)"
    echo "$out"
    if grep -qE '\bm[12]@x\b' <<<"$out"; then
        echo "[smoke] PASS: codex returned at least one expected message-id"
    else
        echo "[smoke] FAIL: codex output did not mention m1@x or m2@x"
        exit 1
    fi
fi

echo
echo "[smoke] all requested agents passed"
