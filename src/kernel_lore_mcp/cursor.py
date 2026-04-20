"""Pagination-cursor helpers.

MCP tool responses that paginate emit a `next_cursor` string the
caller can pass back on the next invocation to pick up after the
last returned row. Cursors are opaque (HMAC-signed) so a client
can't hand-craft an offset that skips records, tampers with the
score bound, or targets a different query's result set.

Wire format + signing are implemented Rust-side in
``kernel_lore_mcp._core.sign_cursor`` /
``kernel_lore_mcp._core.verify_cursor`` — same code path the
server uses internally. This module wraps them with:

  * stable key resolution (env → data_dir state file → generated),
  * query-scoped hashes so a cursor for query A can't be replayed
    against query B,
  * Python-level error types mapped to the MCP `invalid_argument`
    shape used by the rest of the tool surface.
"""

from __future__ import annotations

import base64
import hashlib
import os
import secrets
from pathlib import Path

from kernel_lore_mcp import _core
from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import invalid_argument


_CURSOR_KEY_FILE = "cursor.key"
_MIN_KEY_BYTES = 32


def _key_path() -> Path:
    """Location of the auto-generated cursor key file.

    Kept under the data_dir `state/` subtree so it lives with the
    rest of the mutable server state (writer.lock, generation
    counter, manifest cache). Absent in a fresh deployment; created
    on first use when `KLMCP_CURSOR_KEY` is unset.
    """
    return get_settings().data_dir / "state" / _CURSOR_KEY_FILE


def _auto_generate_key() -> bytes:
    """Generate a fresh 256-bit key and persist it under data_dir.

    stdio-mode deployments run anonymously on one laptop and don't
    want to deal with `openssl rand -hex 32` on first launch; http-
    mode production is expected to set `KLMCP_CURSOR_KEY` explicitly
    in the environment file. This fallback keeps the dev experience
    friction-free without compromising the hosted-instance posture.
    """
    path = _key_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    key = secrets.token_bytes(_MIN_KEY_BYTES)
    # Hex-encoded on disk so the file is inspectable; tempfile +
    # rename for atomic write across signal/crash boundaries.
    tmp = path.with_suffix(".key.tmp")
    tmp.write_text(key.hex())
    os.replace(tmp, path)
    os.chmod(path, 0o600)
    return key


def _load_key_from_disk() -> bytes | None:
    path = _key_path()
    try:
        text = path.read_text().strip()
    except FileNotFoundError:
        return None
    try:
        return bytes.fromhex(text)
    except ValueError:
        return None


def cursor_secret() -> bytes:
    """Resolve the cursor-signing secret.

    Resolution order (matches docs/mcp/transport-auth.md):
      1. `KLMCP_CURSOR_KEY` env var (hex-encoded). Production.
      2. `Settings.cursor_signing_key` from pydantic-settings (str).
      3. `<data_dir>/state/cursor.key` on disk. Auto-generated if
         step 1 and 2 fail, so local stdio dev "just works."

    Raises ValueError when a provided value isn't valid hex — better
    than running with a silently broken signer.
    """
    env = os.environ.get("KLMCP_CURSOR_KEY")
    if env is None:
        settings_key = get_settings().cursor_signing_key
        if settings_key is not None:
            env = settings_key
    if env is not None:
        try:
            key = bytes.fromhex(env)
        except ValueError as e:
            raise ValueError(
                "KLMCP_CURSOR_KEY must be hex-encoded; "
                f"decode failed: {e}"
            ) from e
        if len(key) < _MIN_KEY_BYTES:
            raise ValueError(
                f"KLMCP_CURSOR_KEY must be at least {_MIN_KEY_BYTES} "
                f"bytes; got {len(key)}"
            )
        return key
    disk = _load_key_from_disk()
    if disk is not None and len(disk) >= _MIN_KEY_BYTES:
        return disk
    return _auto_generate_key()


def query_hash(*parts: object) -> int:
    """Stable 64-bit hash over the query parameters that define a
    pagination context. Two calls with the same parts produce the
    same hash; changing any part invalidates every cursor minted
    against the previous query.

    Uses BLAKE2b truncated to 8 bytes — not a cryptographic property
    here, just determinism across processes and Python versions
    (built-in `hash()` is randomized per interpreter run).
    """
    raw = "\x1f".join(str(p) for p in parts).encode("utf-8")
    digest = hashlib.blake2b(raw, digest_size=8).digest()
    return int.from_bytes(digest, "big")


def mint_cursor(
    *,
    q_hash: int,
    last_score: float,
    last_mid: str,
) -> str:
    """Produce an opaque `next_cursor` string for a paginated tool
    response. `last_score` is the RRF / BM25 / sort score of the
    last row returned; `last_mid` is its message-id. On replay,
    callers pass this string as `cursor` and we verify + decode it
    to resume.
    """
    secret = cursor_secret()
    return _core.sign_cursor(secret, q_hash, float(last_score), last_mid)


def decode_cursor(
    token: str | None,
    *,
    expected_q_hash: int,
    arg_name: str = "cursor",
) -> tuple[float, str] | None:
    """Verify + unpack a cursor. Returns `(last_score, last_mid)` on
    success, `None` when `token` is `None` (caller's cue to start
    from the beginning).

    Raises the same `invalid_argument` ToolError the rest of the
    surface uses so clients see a consistent error shape:

      * Bad base64 / hmac mismatch / malformed payload → tampered.
      * Hash mismatch → cursor was minted against a different query
        and can't be replayed.
    """
    if token is None:
        return None
    secret = cursor_secret()
    try:
        q_hash, last_score, last_mid = _core.verify_cursor(secret, token)
    except Exception as e:  # noqa: BLE001
        raise invalid_argument(
            name=arg_name,
            reason=f"cursor could not be verified: {e}",
            value=_preview(token),
            example="(obtained from a prior tool response's next_cursor)",
        ) from e
    if q_hash != expected_q_hash:
        raise invalid_argument(
            name=arg_name,
            reason=(
                "cursor was minted against a different query; mint a "
                "fresh one by re-running with the new parameters"
            ),
            value=_preview(token),
            example="(obtained from a prior tool response's next_cursor)",
        )
    return last_score, last_mid


def _preview(token: str) -> str:
    """Truncate a cursor string for error messages — the full opaque
    blob is too noisy to echo back verbatim and callers can still
    identify their failing request by its prefix."""
    if len(token) <= 24:
        return token
    return token[:20] + "..."


__all__ = [
    "cursor_secret",
    "query_hash",
    "mint_cursor",
    "decode_cursor",
]
