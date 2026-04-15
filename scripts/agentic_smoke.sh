#!/usr/bin/env bash
# Non-interactive smoke test: drive `kernel-lore-mcp` over stdio from
# the actual `claude` and `codex` CLIs and confirm both can exercise
# the full surface we've shipped:
#
#   Stage 1 (tool call)          — lore_eq structured result.
#   Stage 2 (sampling fallback)  — lore_classify_patch returns "bugfix".
#   Stage 3 (resource read)      — lore://message/m1@x body.
#   Stage 4 (prompt / resource
#            listing via MCP     — probes /list for the fast feedback
#            client)               loop, skipping LLM round-trips.
#
# Hits the real Anthropic + OpenAI APIs in stages 1-3 — costs a few
# cents per run. Stage 4 uses the local MCP Python client and hits no
# external API.
#
# Requires:
#   - `claude` and `codex` on $PATH
#   - ANTHROPIC_API_KEY / OPENAI_API_KEY (or equivalent auth already
#     in `~/.claude` / `~/.codex`)
#   - `kernel-lore-mcp` installed into the project venv
#
# Usage:
#   scripts/agentic_smoke.sh                 # both agents + local probe
#   scripts/agentic_smoke.sh claude          # claude + local probe
#   scripts/agentic_smoke.sh codex           # codex + local probe
#   scripts/agentic_smoke.sh local           # just the local MCP probe
#                                              (no LLM round-trips)

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

agent="${1:-both}"

if [[ "$agent" =~ ^(both|claude)$ ]] && ! command -v claude >/dev/null; then
    echo "ERROR: claude CLI not on PATH" >&2
    exit 2
fi
if [[ "$agent" =~ ^(both|codex)$ ]] && ! command -v codex >/dev/null; then
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

mcp_cmd="$repo_root/.venv/bin/kernel-lore-mcp"

prompt_tool='List every linux-cifs message authored by alice@example.com using the lore_eq tool with field=from_addr, value=alice@example.com. Reply with ONLY the message-ids on separate lines, no prose.'
prompt_classify='Call lore_classify_patch with message_id=m1@x. Reply with ONLY the single-word label from the response (e.g. bugfix). No prose, no JSON, no explanation.'
prompt_resource='Read the MCP resource lore://message/m1@x and print the Message-ID header line that starts with "Message-ID:". Reply with ONLY that one line.'

check_contains() {
    local label="$1"
    local needle="$2"
    local haystack="$3"
    if grep -qE "$needle" <<<"$haystack"; then
        echo "[smoke] PASS: $label"
    else
        echo "[smoke] FAIL: $label — output did not match $needle"
        echo "----- captured output -----"
        echo "$haystack"
        echo "---------------------------"
        exit 1
    fi
}

run_claude() {
    local label="$1" prompt="$2" needle="$3" allowed="$4"
    echo
    echo "[smoke] === claude / $label ==="
    local out
    out="$(claude \
        --print \
        --mcp-config "$claude_cfg" \
        --strict-mcp-config \
        --permission-mode bypassPermissions \
        --allowedTools "$allowed" \
        --output-format text \
        "$prompt" 2>&1 || true)"
    echo "$out"
    check_contains "claude / $label" "$needle" "$out"
}

run_codex() {
    local label="$1" prompt="$2" needle="$3"
    echo
    echo "[smoke] === codex / $label ==="
    local out
    out="$(codex exec \
        --sandbox read-only \
        --skip-git-repo-check \
        -c "mcp_servers.kernel_lore.command=\"$mcp_cmd\"" \
        -c 'mcp_servers.kernel_lore.args=["--transport","stdio"]' \
        -c "mcp_servers.kernel_lore.env={KLMCP_DATA_DIR=\"$data_dir\"}" \
        "$prompt" 2>&1 || true)"
    echo "$out"
    check_contains "codex / $label" "$needle" "$out"
}

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
    run_claude "tool call (lore_eq)" "$prompt_tool" '\bm[12]@x\b' \
        "mcp__kernel-lore__lore_eq"
    run_claude "sampling fallback (lore_classify_patch)" "$prompt_classify" \
        '\bbugfix\b' \
        "mcp__kernel-lore__lore_classify_patch"
    run_claude "resource read (lore://message)" "$prompt_resource" \
        'Message-ID: <m1@x>' \
        "mcp__kernel-lore__*,ReadMcpResource"
fi

# ---------------------------------------------------------------- codex
if [[ "$agent" =~ ^(both|codex)$ ]]; then
    run_codex "tool call (lore_eq)" "$prompt_tool" '\bm[12]@x\b'
    run_codex "sampling fallback (lore_classify_patch)" "$prompt_classify" \
        '\bbugfix\b'
    # Codex MCP resource surface rolled out in April 2026 but support
    # varies; tolerate either a direct resource read or a tool-mediated
    # fetch (codex may choose to wrap through a shell).
    run_codex "resource read (lore://message)" "$prompt_resource" \
        'Message-ID: <m1@x>'
fi

# ---------------------------------------------------------------- local probe
# Cheap feedback loop that exercises the MCP surface WITHOUT any LLM
# round-trip: list tools / resources / prompts via the in-process
# fastmcp client and confirm all three tables are populated with the
# shapes shipped in Sprint 0 + Phase 10 + Phase 11 + Phase 12.
echo
echo "[smoke] === local MCP probe (no LLM) ==="
"$repo_root/.venv/bin/python" - <<PY
import asyncio, os, sys
from fastmcp import Client
from kernel_lore_mcp.server import build_server

os.environ["KLMCP_DATA_DIR"] = "$data_dir"

async def main():
    async with Client(build_server()) as c:
        tools = {t.name for t in await c.list_tools()}
        templates = {t.uriTemplate for t in await c.list_resource_templates()}
        prompts = {p.name for p in await c.list_prompts()}

    need_tools = {
        "lore_search", "lore_eq", "lore_patch_search",
        "lore_summarize_thread", "lore_classify_patch",
        "lore_explain_review_status",
    }
    need_templates = {
        "lore://message/{mid}", "lore://thread/{tid}",
        "lore://patch/{mid}", "lore://maintainer/{path}",
        "lore://patchwork/{msg_id}",
    }
    need_prompts = {
        "klmcp_pre_disclosure_novelty_check",
        "klmcp_cve_chain_expand",
        "klmcp_series_version_diff",
        "klmcp_recent_reviewers_for",
        "klmcp_cross_subsystem_pattern_transfer",
    }

    def diff(label, want, have):
        missing = want - have
        if missing:
            print(f"[smoke] FAIL: local probe — {label} missing: {sorted(missing)}", file=sys.stderr)
            sys.exit(1)
        print(f"[smoke] PASS: local probe — {len(want)}/{len(want)} {label} present")

    diff("tools", need_tools, tools)
    diff("resource templates", need_templates, templates)
    diff("prompts", need_prompts, prompts)

asyncio.run(main())
PY

echo
echo "[smoke] all requested agents passed"
