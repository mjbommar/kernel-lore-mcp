"""Type stubs for the Rust extension module.

Keep in lockstep with `src/lib.rs`.
"""

from __future__ import annotations

def version() -> str:
    """Return the version string baked into the Rust extension at build time."""
    ...
