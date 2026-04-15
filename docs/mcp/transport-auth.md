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

## Auth — permanently anonymous read-only

**There is no authentication. There will never be authentication.**
This is a product constraint, not a posture choice (see CLAUDE.md §
"Non-negotiable product constraints"):

- **No API keys.** Not as a query string, not as a header, not lifted
  by an env var. Don't add one to "temporarily gate" a new tool
  either — build the tool safely for anonymous use or don't ship it.
- **No OAuth 2.1, no PKCE, no DCR.** Do not layer it in a v2 "if we
  ever want claude.ai Connectors." The claude.ai Connector story is
  solved for us by the fact that any developer can point their agent
  at a self-hosted instance — no gate at all is better than even the
  friction of a login flow for a public read-only mirror.
- **No Bearer "partner token" lift.** Do not add a rate-limit bypass
  keyed on a shared secret; that's an API key with different naming.
  The per-IP limit is the only lever.
- **No user accounts, no session state, no per-user data.** All
  responses deterministic given (query, index snapshot).
- **Upstream credentials (KCIDB BigQuery, GitHub API for ingestion,
  LWN feed, etc.) live in the server's deployment env and never
  touch the caller.** The caller's MCP request never carries a
  secret, inbound or outbound.

### Rate limiting

- nginx in front handles `limit_req_zone` at 60/min/ip, `burst=30`
  `nodelay`. Same limit every caller, everywhere.
- IPv6 handled via `$binary_remote_addr` truncation (/64) so we
  don't trivially allow /128 sweeps.
- 429 responses carry a `Retry-After` header so agents back off.
- The rate limit is generous by design — fanout-to-one means every
  agent pointed at us is one fewer scraping lore directly, so a
  mild per-IP limit is the right safety valve, not a paywall.

## CORS

- Allow-origin `*` on GET endpoints. Allow-headers: standard MCP.
- Allow-methods: GET, POST (MCP uses POST for `tools/call`), OPTIONS.
- No cookies — strictly anonymous, every request.

## TLS

Terminate at ALB or nginx. Cert via Let's Encrypt. HSTS with
`includeSubDomains` — we only run one subdomain.

## Client config recipes

See [`client-config.md`](./client-config.md) for copy-paste
snippets for Claude Code, Cursor, Codex, Zed.
