"""Entry point for the ``kernel-lore-mcp`` console script.

Defaults:
  * `--transport stdio` (safe for local dev, Claude Code local config).
  * HTTP mode binds `127.0.0.1` unless `KLMCP_BIND` is set or
    `--host 0.0.0.0` is passed explicitly.

Stdio note: in stdio mode **all** logging must go to stderr, never
stdout. stdout carries MCP JSON-RPC frames; any extra byte corrupts
the stream. `logging_.configure()` handles this.
"""

from __future__ import annotations

import argparse
import os
import sys

from kernel_lore_mcp.logging_ import configure as configure_logging
from kernel_lore_mcp.server import build_server


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="kernel-lore-mcp")
    parser.add_argument(
        "--transport",
        choices=("stdio", "http"),
        default="stdio",
        help="MCP transport (stdio for local dev, http for hosted).",
    )
    parser.add_argument(
        "--host",
        default=None,
        help="HTTP bind host. Default 127.0.0.1 unless KLMCP_BIND is set.",
    )
    parser.add_argument("--port", type=int, default=8080, help="HTTP bind port.")
    parser.add_argument(
        "--uds",
        default=None,
        help="Path to bind a Unix domain socket instead of TCP (http mode).",
    )
    parser.add_argument(
        "--log-level",
        default=os.environ.get("KLMCP_LOG_LEVEL", "INFO"),
        help="Log level (DEBUG/INFO/WARN/ERROR). Env: KLMCP_LOG_LEVEL.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    configure_logging(transport=args.transport, level=args.log_level)

    mcp = build_server()

    if args.transport == "stdio":
        mcp.run(transport="stdio")
        return 0

    host = args.host or os.environ.get("KLMCP_BIND", "127.0.0.1")
    if args.uds:
        mcp.run(transport="http", uds=args.uds)
    else:
        mcp.run(transport="http", host=host, port=args.port)
    return 0


if __name__ == "__main__":
    sys.exit(main())
