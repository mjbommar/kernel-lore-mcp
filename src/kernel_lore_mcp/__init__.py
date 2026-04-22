"""kernel-lore-mcp — MCP server over lore.kernel.org.

The public Python API is deliberately small. The heavy lifting lives
in the Rust extension module `kernel_lore_mcp._core` (built by
maturin). This package is the MCP surface.

`_core` is imported lazily so that tooling (pytest collection, ruff,
mypy on source) does not require a built wheel.
"""

from __future__ import annotations

import importlib
from typing import Any

__all__ = ["__version__", "native_version"]

__version__ = "0.3.3"


def __getattr__(name: str) -> Any:
    # Lazy _core import so tooling (pytest collection, ruff, ty) does
    # not hard-require a built wheel. `importlib.import_module` avoids
    # the recursion hazard that `from kernel_lore_mcp import _core`
    # would introduce inside __getattr__.
    if name == "_core":
        return importlib.import_module("kernel_lore_mcp._core")
    raise AttributeError(f"module 'kernel_lore_mcp' has no attribute {name!r}")


def native_version() -> str:
    """Return the version baked into the compiled Rust extension."""
    core = importlib.import_module("kernel_lore_mcp._core")
    return core.version()
