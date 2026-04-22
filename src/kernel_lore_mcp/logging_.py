"""structlog configuration.

Invariant: in **stdio transport** everything structlog emits goes to
stderr. stdout is reserved for MCP JSON-RPC framing. A stray log line
on stdout corrupts the protocol.
"""

from __future__ import annotations

from dataclasses import dataclass
import logging
import os
import sys
from typing import Literal

import structlog


Mode = Literal["local", "hosted"]


@dataclass(frozen=True, slots=True)
class ProfilingThresholds:
    """Slow-path thresholds for structured profiling logs."""

    request_seconds: float
    tool_seconds: float
    queue_wait_seconds: float


def _env_ms(name: str, default_ms: int) -> int:
    raw = os.environ.get(name)
    if raw is None:
        return default_ms
    try:
        value = int(raw)
    except ValueError:
        return default_ms
    return max(0, value)


def profiling_thresholds(mode: Mode) -> ProfilingThresholds:
    """Return mode-aware thresholds for slow-path profiling logs.

    Env overrides:
      * KLMCP_SLOW_REQUEST_MS
      * KLMCP_SLOW_TOOL_MS
      * KLMCP_SLOW_QUEUE_WAIT_MS
    """
    hosted = mode == "hosted"
    return ProfilingThresholds(
        request_seconds=_env_ms(
            "KLMCP_SLOW_REQUEST_MS",
            1000 if hosted else 3000,
        )
        / 1000.0,
        tool_seconds=_env_ms(
            "KLMCP_SLOW_TOOL_MS",
            500 if hosted else 2000,
        )
        / 1000.0,
        queue_wait_seconds=_env_ms(
            "KLMCP_SLOW_QUEUE_WAIT_MS",
            25 if hosted else 100,
        )
        / 1000.0,
    )


def _apply_library_levels(*, mode: Mode, log_level: int) -> None:
    """Keep hosted logs readable by suppressing high-volume libraries."""
    noisy_level = max(log_level, logging.WARNING) if mode == "hosted" else log_level
    for name in (
        "uvicorn.access",
        "httpx",
        "httpcore",
        "fastmcp.server.context.to_client",
    ):
        logging.getLogger(name).setLevel(noisy_level)

    for name in ("uvicorn.error", "fastmcp", "starlette"):
        logging.getLogger(name).setLevel(log_level)


def configure(*, transport: str, mode: Mode = "local", level: str = "INFO") -> None:
    log_level = getattr(logging, level.upper(), logging.INFO)

    # All stdlib logging -> stderr in both modes (MCP stdio owns stdout).
    logging.basicConfig(
        level=log_level,
        stream=sys.stderr,
        format="%(message)s",
        force=True,
    )
    _apply_library_levels(mode=mode, log_level=log_level)

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
