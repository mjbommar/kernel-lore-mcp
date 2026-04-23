# Client configuration — copy-paste snippets

Hook `kernel-lore-mcp` into every MCP-speaking agent. All snippets
use **stdio transport** — simplest, no port binding, the agent
spawns the server on demand. Use HTTP only when the agent is on a
different machine than the server (rare for personal dev).

All snippets assume:

- `kernel-lore-mcp` is on `$PATH` (after `uv sync && uv run maturin
  develop --release`, the binary is at
  `<repo>/.venv/bin/kernel-lore-mcp`; the examples below use a
  literal path — replace with `which kernel-lore-mcp` if you
  install it system-wide).
- Your data dir lives at `/home/you/klmcp-data` (pick any path;
  matches the `KLMCP_DATA_DIR` you used when running
  `kernel-lore-sync`).

**Zero secrets to rotate, zero accounts to create.** See
CLAUDE.md § "Non-negotiable product constraints" — `kernel-lore-mcp`
is anonymous read-only on every deployment.

## Claude Code

Two config file locations, both work:

- **Project-scoped:** `<project-root>/.mcp.json` (shared with
  collaborators via git).
- **User-scoped:** `~/.claude/mcp.json` (personal).

```json
{
  "mcpServers": {
    "kernel-lore": {
      "type": "stdio",
      "command": "/home/you/kernel-lore-mcp/.venv/bin/kernel-lore-mcp",
      "args": ["--transport", "stdio"],
      "env": {
        "KLMCP_DATA_DIR": "/home/you/klmcp-data"
      }
    }
  }
}
```

Quick smoke, non-interactive:

```sh
claude --print \
    --mcp-config ~/.claude/mcp.json \
    --permission-mode bypassPermissions \
    --allowedTools "mcp__kernel-lore__lore_eq" \
    "list every linux-cifs message with from_addr=alice@example.com via lore_eq"
```

If it prints message-ids, wiring is correct. This exact shape is
what `scripts/agentic_smoke.sh` exercises live every commit against
the synthetic fixture.

## Codex (OpenAI CLI)

Config file: `~/.codex/config.toml`. Add a block:

```toml
[mcp_servers.kernel_lore]
command = "/home/you/kernel-lore-mcp/.venv/bin/kernel-lore-mcp"
args = ["--transport", "stdio"]

[mcp_servers.kernel_lore.env]
KLMCP_DATA_DIR = "/home/you/klmcp-data"
```

Or override per invocation (no config file edit needed):

```sh
codex exec \
    --sandbox read-only \
    --skip-git-repo-check \
    -c 'mcp_servers.kernel_lore.command="/home/you/kernel-lore-mcp/.venv/bin/kernel-lore-mcp"' \
    -c 'mcp_servers.kernel_lore.args=["--transport","stdio"]' \
    -c 'mcp_servers.kernel_lore.env={KLMCP_DATA_DIR="/home/you/klmcp-data"}' \
    "what are the five most recent patches touching fs/smb/server/?"
```

**Codex MCP server name must use underscores, not hyphens.** Codex
exposes prompts as `/mcp__<server>__<prompt>`, which can't contain a
hyphen in the server portion.

## Cursor

Config files (pick one):

- **Project-scoped:** `<project-root>/.cursor/mcp.json` (shared).
- **User-scoped:** `~/.cursor/mcp.json` (personal; Windows:
  `%USERPROFILE%\.cursor\mcp.json`).

```json
{
  "mcpServers": {
    "kernel-lore": {
      "command": "/home/you/kernel-lore-mcp/.venv/bin/kernel-lore-mcp",
      "args": ["--transport", "stdio"],
      "env": {
        "KLMCP_DATA_DIR": "/home/you/klmcp-data"
      }
    }
  }
}
```

Cursor picks up resources + tools + prompts automatically on save
(no restart). Verify in Settings → MCP Servers; green dot means
connected.

Resource templates (`lore://message/{mid}` etc.) are only supported
on Cursor **1.6+** (September 2025). Older Cursor versions still
get the tools + prompts surface.

## Zed

Edit `settings.json` (Cmd/Ctrl+,):

```json
{
  "context_servers": {
    "kernel-lore": {
      "source": "custom",
      "command": "/home/you/kernel-lore-mcp/.venv/bin/kernel-lore-mcp",
      "args": ["--transport", "stdio"],
      "env": {
        "KLMCP_DATA_DIR": "/home/you/klmcp-data"
      }
    }
  }
}
```

Zed auto-restarts the server process on save — no editor restart
needed. Status shows in the Agent panel (green = active).

**Zed only speaks stdio natively.** If you need HTTP (server runs
on a different box), use the `mcp-remote` bridge package as a
stdio→HTTP proxy; not required for personal use.

## Pattern: server-on-another-box (HTTP)

If `kernel-lore-mcp` runs on a VM and your agent runs on your
laptop, switch the server to HTTP:

```sh
# On the server:
kernel-lore-mcp serve --transport http --host 0.0.0.0 --port 8080
```

Agents point at `http://<server>:8080/mcp`. Still anonymous — nginx
in front of the server handles the per-IP rate limit
(60/min/IP) described in [`./transport-auth.md`](./transport-auth.md).

## Troubleshooting

### "no such tool: mcp__kernel-lore__..."

- The server isn't in your `--allowedTools` list. Either add the
  specific tool, or use a prefix glob: `mcp__kernel-lore__*`.
- The server didn't start. Run the command manually:
  ```sh
  KLMCP_DATA_DIR=/path/to/data kernel-lore-mcp --transport stdio </dev/null
  ```
  It should print nothing to stdout (stdio mode reserves stdout for
  JSON-RPC frames), then block waiting for a frame. If it writes
  anything to stdout outside the MCP framing, file a bug.

### "freshness_ok: false" or stale responses

Your data_dir is behind on ingest. Check without booting HTTP:

```sh
kernel-lore-mcp status --data-dir /home/you/klmcp-data
```

Run `kernel-lore-sync --data-dir /home/you/klmcp-data --with-over`
to force a fresh tick. If you need slower derived tiers rebuilt
without refetching lore, use `kernel-lore-reindex --data-dir
/home/you/klmcp-data`.

### Claude `-p` exits silently with HTTP transport

Known upstream bug
([anthropics/claude-code#32191](https://github.com/anthropics/claude-code/issues/32191)).
Use stdio in `-p` mode. HTTP works fine in the interactive TUI.

## Verification without API keys

The local MCP probe confirms the server is reachable and the
surface is complete — no LLM round-trip, no API key:

```sh
cd /path/to/kernel-lore-mcp
scripts/agentic_smoke.sh local
```

Expected output: `PASS: local probe — 6/6 tools present`, `5/5
resource templates`, `5/5 prompts`. Any FAIL means the surface is
drifted; re-check your install.

## Sources

- [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)
- [Codex MCP docs](https://developers.openai.com/codex/mcp)
- [Cursor MCP docs](https://docs.cursor.com/context/model-context-protocol)
- [Zed MCP docs](https://zed.dev/docs/ai/mcp)
- [FastMCP MCP-JSON reference](https://gofastmcp.com/integrations/mcp-json-configuration)
