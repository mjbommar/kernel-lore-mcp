#!/usr/bin/env bash
# klmcp-doctor.sh — end-to-end sanity check for a kernel-lore-mcp
# deployment. Answers: "does this install actually work?"
#
# Runs in ~10 seconds on a warm box, hits no external service, burns
# zero API tokens. Use after first install, after a dependency
# upgrade, or before opening a bug.
#
# Usage:
#   scripts/klmcp-doctor.sh
#
# Exits 0 only if every PASS. Prints one-line "[doctor] PASS/WARN/FAIL"
# per check with actionable fix hints.

set -u   # no -e; we want to score all checks, not exit on the first FAIL.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

pass=0
fail=0
warn=0

ok()   { echo "[doctor] PASS: $*"; pass=$((pass + 1)); }
bad()  { echo "[doctor] FAIL: $*"; fail=$((fail + 1)); }
meh()  { echo "[doctor] WARN: $*"; warn=$((warn + 1)); }

# ---------------------------------------------------------------- toolchain
if command -v rustc >/dev/null 2>&1; then
    ver="$(rustc --version | awk '{print $2}')"
    # Require 1.85+ per rust-toolchain.toml. Sort ascending: if the
    # minimum comes first, the installed version is >= minimum.
    min=1.85.0
    smallest="$(printf '%s\n%s\n' "$ver" "$min" | sort -V | head -1)"
    if [[ "$smallest" == "$min" ]]; then
        ok "rustc $ver (>= $min)"
    else
        bad "rustc $ver is older than $min — upgrade via rustup"
    fi
else
    bad "rustc not on PATH — curl https://sh.rustup.rs | sh"
fi

if command -v python3 >/dev/null 2>&1; then
    py_ver="$(python3 -c 'import sys; print(f"{sys.version_info[0]}.{sys.version_info[1]}")')"
    case "$py_ver" in
        3.12|3.13|3.14|3.15|3.16) ok "python3 $py_ver (>= 3.12)" ;;
        *) bad "python3 $py_ver older than 3.12 — upgrade" ;;
    esac
else
    bad "python3 not on PATH"
fi

if command -v uv >/dev/null 2>&1; then
    ok "uv $(uv --version | awk '{print $2}')"
else
    bad "uv not on PATH — curl -LsSf https://astral.sh/uv/install.sh | sh"
fi

if command -v grok-pull >/dev/null 2>&1; then
    ok "grok-pull $(grok-pull --version 2>&1 | head -1)"
else
    meh "grok-pull not on PATH — uv tool install grokmirror (only needed for real ingest)"
fi

# ---------------------------------------------------------------- binaries
if [[ -x "$repo_root/.venv/bin/kernel-lore-mcp" ]]; then
    ok "kernel-lore-mcp binary present at .venv/bin"
elif command -v kernel-lore-mcp >/dev/null 2>&1; then
    ok "kernel-lore-mcp binary present on PATH"
else
    bad "kernel-lore-mcp missing — run: uv sync && uv run maturin develop --release"
fi

if [[ -x "$repo_root/target/release/kernel-lore-ingest" ]]; then
    ok "kernel-lore-ingest binary present at target/release"
elif [[ -x "$repo_root/.venv/bin/kernel-lore-ingest" ]]; then
    ok "kernel-lore-ingest binary present at .venv/bin"
elif command -v kernel-lore-ingest >/dev/null 2>&1; then
    ok "kernel-lore-ingest binary present on PATH"
else
    bad "kernel-lore-ingest missing — run: cargo build --release --bin kernel-lore-ingest"
fi

# ---------------------------------------------------------------- smoke
# Synthetic-fixture ingest + MCP surface probe in a tempdir. Reuses
# the fixtures that power tests/python/fixtures + the agentic-smoke
# local probe.
work="$(mktemp -d -t klmcp-doctor-XXXX)"
trap 'rm -rf "$work"' EXIT

if "$repo_root/.venv/bin/python" - <<PY 2>"$work/py.err"
import sys
from pathlib import Path
sys.path.insert(0, "$repo_root")
from tests.python.fixtures import make_synthetic_shard
from kernel_lore_mcp import _core
shard = Path("$work/shards/0.git")
shard.parent.mkdir(parents=True, exist_ok=True)
make_synthetic_shard(shard)
data = Path("$work/data")
data.mkdir(parents=True, exist_ok=True)
stats = _core.ingest_shard(data_dir=data, shard_path=shard,
                           list="linux-cifs", shard="0",
                           run_id="doctor")
assert stats["ingested"] == 2, f"expected 2 ingested, got {stats['ingested']}"
print("ok")
PY
then
    ok "synthetic-fixture ingest (2 msgs into tmpdir)"
else
    bad "synthetic ingest failed — see $work/py.err for details"
fi

# MCP surface probe — must see every tool/resource/prompt shipped.
# Reuses the local probe from scripts/agentic_smoke.sh but runs
# in-process so we can assert on exact names.
if "$repo_root/.venv/bin/python" - <<PY 2>"$work/mcp.err"
import asyncio, os, sys
os.environ["KLMCP_DATA_DIR"] = "$work/data"
sys.path.insert(0, "$repo_root")
from fastmcp import Client
from kernel_lore_mcp.server import build_server
# Single source of truth for the "must exist" surface — see
# src/kernel_lore_mcp/_surface_manifest.py. Drift between this list
# and the live server registration fails the paired pytest in CI
# (tests/python/test_surface_manifest.py).
from kernel_lore_mcp._surface_manifest import (
    REQUIRED_TOOLS as NEED_TOOLS,
    REQUIRED_RESOURCE_TEMPLATES as NEED_TEMPLATES,
    REQUIRED_PROMPTS as NEED_PROMPTS,
)

async def main():
    async with Client(build_server()) as c:
        tools = {t.name for t in await c.list_tools()}
        templates = {t.uriTemplate for t in await c.list_resource_templates()}
        prompts = {p.name for p in await c.list_prompts()}
    missing = []
    if NEED_TOOLS - tools:
        missing.append(f"tools={sorted(NEED_TOOLS - tools)}")
    if NEED_TEMPLATES - templates:
        missing.append(f"templates={sorted(NEED_TEMPLATES - templates)}")
    if NEED_PROMPTS - prompts:
        missing.append(f"prompts={sorted(NEED_PROMPTS - prompts)}")
    if missing:
        print("MISSING:", *missing, file=sys.stderr)
        sys.exit(1)
    print("ok")

asyncio.run(main())
PY
then
    ok "MCP surface complete (tools + resource templates + prompts)"
else
    bad "MCP surface drift — see $work/mcp.err"
fi

# status subcommand — canary for packaging.
if "$repo_root/.venv/bin/kernel-lore-mcp" status \
        --data-dir "$work/data" >"$work/status.json" 2>/dev/null; then
    if grep -q '"freshness_ok": true' "$work/status.json"; then
        ok "kernel-lore-mcp status returns freshness_ok=true"
    else
        meh "status ran but freshness_ok not true — check $work/status.json"
    fi
else
    bad "kernel-lore-mcp status failed"
fi

# ---------------------------------------------------------------- summary
echo
if (( fail == 0 )); then
    echo "[doctor] OK — $pass checks passed, $warn warnings"
    exit 0
else
    echo "[doctor] FAIL — $pass passed, $fail failed, $warn warnings"
    exit 1
fi
