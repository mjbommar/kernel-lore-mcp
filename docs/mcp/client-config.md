# MCP — client configuration

Copy-paste snippets once the server is live.

## Claude Code (local stdio)

`~/.config/claude-code/mcp.json`:
```json
{
  "mcpServers": {
    "kernel-lore": {
      "command": "uvx",
      "args": ["kernel-lore-mcp", "--transport", "stdio"]
    }
  }
}
```

## Claude Code (remote HTTP)

```json
{
  "mcpServers": {
    "kernel-lore": {
      "url": "https://lore-mcp.example.com/mcp"
    }
  }
}
```

## Cursor

Settings → MCP → Add server → URL `https://lore-mcp.example.com/mcp`.

## Codex

`codex.yaml`:
```yaml
mcp:
  servers:
    kernel-lore:
      url: https://lore-mcp.example.com/mcp
```

## Zed

`settings.json`:
```json
{
  "context_servers": {
    "kernel-lore": {
      "command": {
        "path": "uvx",
        "args": ["kernel-lore-mcp", "--transport", "stdio"]
      }
    }
  }
}
```

## Direct REST (non-MCP clients)

```
GET  /search?q=dfhh:smb_check_perm_dacl+rt:30.days.ago..&limit=25
GET  /thread/<mid>
GET  /patch/<mid>
GET  /activity?file=fs/smb/server/smbacl.c&since=30d
GET  /message/<mid>
GET  /status
```

Same JSON shape as the MCP tool outputs minus the MCP envelope.
