# Research — MCP Python SDK (April 14 2026)

## Decision

Use **standalone `fastmcp` 3.2.4** (PrefectHQ / jlowin), Streamable
HTTP transport, anonymous + rate-limited auth for v1.

## Why (short version)

- FastMCP 3.2 is the de facto standard (~70% of MCP servers per
  public surveys); the low-level `mcp` SDK is re-implementing
  ground FastMCP already covers, and Anthropic previewed an
  official SDK v2 at MCP Dev Summit 2026 — not a place to build
  against yet.
- SSE transport formally deprecated April 1 2026; vendors are
  sunsetting SSE endpoints through June 2026. Streamable HTTP is
  what Claude Code, Claude.ai Connectors, Cursor, Codex, and Zed
  all speak.
- Auth: MCP Nov 2025 spec mandates OAuth 2.1 + PKCE for
  user-scoped servers. Read-only archive with no user-scoped data
  is fine anonymous + per-IP rate limit. Revisit if we want
  first-class claude.ai Connector listing.

## Known gotchas encoded into the scaffold

- `mcp.run(transport="http")` binds 127.0.0.1 by default — our
  `__main__.py` forces 0.0.0.0 for the http path.
- Some official-SDK versions have a strict-Accept-header bug
  breaking against claude.ai's proxy (issue #2349). FastMCP avoids
  this. Stay on FastMCP.
- Python 3.14 free-threaded (`python3.14t`) is stable but not all
  transitive deps are certified; ship GIL build by default, keep
  free-threaded as a flag.
- Tool names snake_case, <64 chars; Cursor truncates otherwise.
- Use MCP `outputSchema` (spec 2025-06-18) so clients validate
  structured results.

## Pagination

Opaque cursor strings only, per MCP spec and microsoft/mcp-for-beginners
pagination guide. No offset/limit for LLM callers.

## Sources

- [jlowin/fastmcp](https://github.com/jlowin/fastmcp)
- [modelcontextprotocol/python-sdk](https://github.com/modelcontextprotocol/python-sdk)
- [FastMCP on PyPI](https://pypi.org/project/fastmcp/)
- [MCP Transports spec 2025-03-26](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports)
- [MCP Tools spec 2025-06-18](https://modelcontextprotocol.io/specification/2025-06-18/server/tools)
- [MCP Authorization tutorial](https://modelcontextprotocol.io/docs/tutorials/security/authorization)
- [Why MCP Deprecated SSE](https://blog.fka.dev/blog/2025-06-06-why-mcp-deprecated-sse-and-go-with-streamable-http/)
- [python-sdk issue #2349](https://github.com/modelcontextprotocol/python-sdk/issues/2349)
