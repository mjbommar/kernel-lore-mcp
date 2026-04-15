"""Runtime configuration via environment variables or .env.

Env prefix: `KLMCP_`.

`get_settings()` returns the process-wide singleton. `build_server`
calls `set_settings(s)` once at startup; all tools call
`get_settings()` instead of constructing `Settings()` from env.
"""

from __future__ import annotations

from pathlib import Path

from pydantic import Field
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_prefix="KLMCP_",
        env_file=".env",
        extra="ignore",
    )

    data_dir: Path = Field(
        default=Path("./data"),
        description=(
            "Root dir for compressed raw store + index tiers + state. "
            "Override via KLMCP_DATA_DIR env var."
        ),
    )
    lore_mirror_dir: Path = Field(
        default=Path("./data/lore-mirror"),
        description="grokmirror-managed public-inbox git shards.",
    )

    bind: str = Field(
        default="127.0.0.1",
        description="HTTP bind host. Override to 0.0.0.0 for public deploy.",
    )
    port: int = Field(default=8080, ge=1, le=65535)

    rate_limit_per_ip_per_minute: int = Field(
        default=60,
        description=(
            "Per-IP cap. Same limit for every caller — no auth tier, "
            "ever; see CLAUDE.md §Non-negotiable product constraints."
        ),
    )

    grokmirror_interval_seconds: int = Field(
        default=300,
        ge=60,
        le=3600,
        description=(
            "Seconds between grokmirror pulls. Default 300 per "
            "docs/ops/update-frequency.md. Floor 60, ceiling 3600 — "
            "tighter than 60s risks kernel.org infra politeness; "
            "looser than 1h breaks the freshness promise."
        ),
    )
    ingest_debounce_seconds: int = Field(
        default=30,
        ge=0,
        le=600,
        description=(
            "Minimum gap between consecutive ingest runs regardless "
            "of grok-pull trigger rate. Prevents overlapping writers."
        ),
    )

    cursor_signing_key: str | None = Field(
        default=None,
        description="HMAC key for opaque cursor signing. Required in http mode.",
    )

    freshness_cache_ttl_seconds: int = Field(default=30, ge=1)

    query_wall_clock_ms: int = Field(
        default=5000,
        description="Per-query hard wall-clock cap across all tiers.",
    )
    thread_response_max_bytes: int = Field(
        default=5 * 1024 * 1024,
        description="Per-response byte cap (lore_thread / lore_patch).",
    )


# Process-wide singleton. Set once by build_server(); read by tools
# via get_settings(). Avoids re-parsing env on every request and
# makes build_server(settings=...) meaningful.
_singleton: Settings | None = None


def set_settings(s: Settings) -> None:
    global _singleton
    _singleton = s


def get_settings() -> Settings:
    if _singleton is not None:
        return _singleton
    return Settings()
