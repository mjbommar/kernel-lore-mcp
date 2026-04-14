"""Runtime configuration via environment variables or .env.

Env prefix: `KLMCP_`.
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
        description="Anonymous-tier cap. Bearer tokens lift this.",
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
