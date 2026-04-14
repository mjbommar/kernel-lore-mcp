# MCP — transport and auth

## Transport: Streamable HTTP only

- **SSE is deprecated** (April 1 2026 per MCP spec). Do not ship.
- **Streamable HTTP** is what Claude Code, Claude.ai Connectors,
  Cursor, Codex, and Zed all speak as of April 2026.
- `stdio` transport is supported for local dev / Claude Code local
  config only.
- WebSocket is not in the MCP spec; don't build it.

## FastMCP 3.2 configuration

```python
from fastmcp import FastMCP

mcp = FastMCP(
    name="kernel-lore",
    instructions=INSTRUCTIONS,  # see server.py
)

# stdio for local
mcp.run(transport="stdio")

# streamable HTTP for hosted
mcp.run(transport="http", host="0.0.0.0", port=8080)
```

Default bind is `127.0.0.1`. **Always pass `host="0.0.0.0"` in the
hosted path.** This has bitten other FastMCP deploys.

## Auth (v1)

Anonymous + per-IP rate limit. Fine for a public read-only archive.

- nginx in front handles `limit_req_zone` at 60/min/ip.
- Optional `Authorization: Bearer <token>` lifts the IP ceiling for
  known partners (handled in a thin FastAPI middleware — not
  strictly MCP).
- **No user accounts, no session state, no per-user data.** All
  responses deterministic given (query, index snapshot).

## Auth (v2, conditional)

If we want first-class claude.ai Connector listing, layer OAuth
2.1 + PKCE per the MCP June 2025 auth spec:

- Client-ID Metadata Documents (CIMD) for dynamic registration.
- Split MCP server (us) from authorization server (WorkOS / Stytch
  / Scalekit — don't roll our own).
- Tokens are still read-only scopes; we never write.

Skip for v1.

## CORS

- Allow-origin `*` on GET endpoints. Allow-headers: standard MCP.
- Allow-methods: GET, POST (MCP uses POST for `tools/call`), OPTIONS.
- No cookies — strictly Bearer or anonymous.

## TLS

Terminate at ALB or nginx. Cert via Let's Encrypt. HSTS with
`includeSubDomains` — we only run one subdomain.

## Client config recipes

See [`client-config.md`](./client-config.md) for copy-paste
snippets for Claude Code, Cursor, Codex, Zed.
