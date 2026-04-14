"""structlog configuration.

Invariant: in **stdio transport** everything structlog emits goes to
stderr. stdout is reserved for MCP JSON-RPC framing. A stray log line
on stdout corrupts the protocol.
"""

from __future__ import annotations

import logging
import sys

import structlog


def configure(*, transport: str, level: str = "INFO") -> None:
    log_level = getattr(logging, level.upper(), logging.INFO)

    # All stdlib logging -> stderr in both modes (MCP stdio owns stdout).
    logging.basicConfig(
        level=log_level,
        stream=sys.stderr,
        format="%(message)s",
    )

    processors: list[structlog.types.Processor] = [
        structlog.contextvars.merge_contextvars,
        structlog.processors.add_log_level,
        structlog.processors.TimeStamper(fmt="iso", utc=True),
    ]

    renderer: structlog.types.Processor
    if transport == "stdio":
        renderer = structlog.processors.JSONRenderer()
    else:
        renderer = structlog.processors.JSONRenderer()
    processors.append(renderer)

    structlog.configure(
        processors=processors,
        wrapper_class=structlog.make_filtering_bound_logger(log_level),
        logger_factory=structlog.PrintLoggerFactory(file=sys.stderr),
        cache_logger_on_first_use=True,
    )
