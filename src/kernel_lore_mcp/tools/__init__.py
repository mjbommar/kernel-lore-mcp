"""MCP tool implementations.

One file per tool. Tools are **not** registered via import
side-effects; `server.build_server()` imports each tool function and
calls `mcp.tool(fn, annotations=...)` explicitly.
"""
