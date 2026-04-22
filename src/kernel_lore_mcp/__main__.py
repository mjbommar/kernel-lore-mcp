"""Entry point for the ``kernel-lore-mcp`` console script.

Two subcommands:

  * ``serve`` (default) — run the MCP server. stdio for local dev,
    streamable HTTP for hosted deployments. In stdio mode ALL logging
    goes to stderr; stdout carries MCP JSON-RPC frames and any extra
    byte corrupts the stream.

  * ``status`` — read the generation file + mtime from a data_dir
    and print one-line JSON. Zero dependencies on the HTTP surface;
    use when you want to answer "is my index fresh?" without booting
    a server. Output shape matches the /status route.

Bare invocation with no subcommand behaves as ``serve`` for backwards
compatibility with the pre-subcommand scripts.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import UTC, datetime
from pathlib import Path

import structlog

from kernel_lore_mcp import __version__
from kernel_lore_mcp.health import read_sync_state, writer_lock_present
from kernel_lore_mcp.logging_ import configure as configure_logging
from kernel_lore_mcp.logging_ import profiling_thresholds


def _add_serve_args(parser: argparse.ArgumentParser) -> None:
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
    parser.add_argument(
        "--mode",
        choices=("local", "hosted"),
        default=None,
        help=(
            "Deployment profile. Defaults to KLMCP_MODE or the config default. "
            "`hosted` enables the public-safe runtime posture."
        ),
    )


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="kernel-lore-mcp")
    sub = parser.add_subparsers(dest="cmd")

    serve = sub.add_parser("serve", help="Run the MCP server (default).")
    _add_serve_args(serve)

    status = sub.add_parser(
        "status",
        help=("Print generation + freshness for a data_dir as JSON. No HTTP server required."),
    )
    status.add_argument(
        "--data-dir",
        default=None,
        help=(
            "Data directory to probe. Default: KLMCP_DATA_DIR env or "
            "the pydantic-settings default (./data)."
        ),
    )

    # Back-compat: `kernel-lore-mcp --transport stdio` with no
    # subcommand keeps working.
    _add_serve_args(parser)
    return parser


def _run_serve(args: argparse.Namespace) -> int:
    from kernel_lore_mcp.config import Settings
    from kernel_lore_mcp.server import build_server

    settings_kwargs: dict[str, object] = {}
    if args.mode is not None:
        settings_kwargs["mode"] = args.mode
    settings = Settings(**settings_kwargs)
    configure_logging(
        transport=args.transport,
        mode=settings.mode,
        level=args.log_level,
    )
    log = structlog.get_logger(__name__)
    thresholds = profiling_thresholds(settings.mode)
    mcp = build_server(settings)
    if args.transport == "stdio":
        log.info(
            "server starting",
            version=__version__,
            mode=settings.mode,
            transport="stdio",
            data_dir=str(settings.data_dir),
            log_level=args.log_level.upper(),
            slow_request_ms=int(thresholds.request_seconds * 1000),
            slow_tool_ms=int(thresholds.tool_seconds * 1000),
            slow_queue_wait_ms=int(thresholds.queue_wait_seconds * 1000),
        )
        mcp.run(transport="stdio")
        return 0

    # CLI args override settings; settings override env defaults.
    host = args.host or settings.bind
    port = args.port if args.port != 8080 else settings.port
    log.info(
        "server starting",
        version=__version__,
        mode=settings.mode,
        transport="http",
        data_dir=str(settings.data_dir),
        bind=host,
        port=port,
        uds=args.uds,
        log_level=args.log_level.upper(),
        slow_request_ms=int(thresholds.request_seconds * 1000),
        slow_tool_ms=int(thresholds.tool_seconds * 1000),
        slow_queue_wait_ms=int(thresholds.queue_wait_seconds * 1000),
    )
    if args.uds:
        mcp.run(transport="http", uds=args.uds)
    else:
        mcp.run(transport="http", host=host, port=port)
    return 0


def _run_status(args: argparse.Namespace) -> int:
    """Read state/generation directly — no server, no HTTP."""
    from kernel_lore_mcp.config import Settings

    data_dir = Path(args.data_dir) if args.data_dir else Settings().data_dir
    gen_file = data_dir / "state" / "generation"
    sync = read_sync_state(data_dir)
    writer_active = writer_lock_present(data_dir)

    if not gen_file.exists():
        out = {
            "service": "kernel-lore-mcp",
            "data_dir": str(data_dir),
            "generation": 0,
            "last_ingest_utc": None,
            "last_ingest_age_seconds": None,
            "configured_interval_seconds": Settings().grokmirror_interval_seconds,
            "freshness_ok": None,
            "writer_lock_present": writer_active,
            "sync_active": bool(sync and sync.get("active")),
            "sync": sync,
            "note": "no ingest has run against this data_dir yet",
        }
        json.dump(out, sys.stdout)
        sys.stdout.write("\n")
        return 0

    try:
        generation = int(gen_file.read_text().strip())
    except ValueError:
        generation = 0
    mtime = datetime.fromtimestamp(gen_file.stat().st_mtime, tz=UTC)
    now = datetime.now(tz=UTC)
    age = max(0, int((now - mtime).total_seconds()))
    interval = Settings().grokmirror_interval_seconds
    out = {
        "service": "kernel-lore-mcp",
        "data_dir": str(data_dir),
        "generation": generation,
        "last_ingest_utc": mtime.isoformat(),
        "last_ingest_age_seconds": age,
        "configured_interval_seconds": interval,
        "freshness_ok": age < 3 * interval,
        "writer_lock_present": writer_active,
        "sync_active": bool(sync and sync.get("active")),
        "sync": sync,
    }
    json.dump(out, sys.stdout)
    sys.stdout.write("\n")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = _build_parser()
    args = parser.parse_args(argv)

    if args.cmd == "status":
        return _run_status(args)

    # Default: serve. Accept both `serve ...` and the bare `--transport`
    # form for back-compat with the pre-subcommand scripts.
    return _run_serve(args)


if __name__ == "__main__":
    sys.exit(main())
