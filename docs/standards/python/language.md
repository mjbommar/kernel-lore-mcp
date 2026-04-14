# Python Language Features — kernel-lore-mcp

Adapted from `../../../../../273v/kaos-modules/docs/python/language.md`.

The project targets **Python 3.12 as the floor** (abi3-py312 for the PyO3
extension), with **3.14 preferred at runtime**. 3.13 works; we do not
block on it. This guide lists the language and typing features we rely
on and the small set of forward-looking items we track but do not
deploy.

See also: [Rust counterpart](../rust/language.md).

---

## Version Strategy

| Version | Role | Notes |
|---------|------|-------|
| **3.12** | Minimum supported | abi3 floor pinned in `pyproject.toml` (`requires-python = ">=3.12"`) and maturin (`abi3-py312`). Required for the `type` statement and PEP 695 generics. |
| **3.13** | Compatible | Runs cleanly. `TypeIs`, `@override`, `@deprecated` available here too. |
| **3.14** | Preferred runtime | GA October 2025. Deferred annotations (PEP 649), `annotationlib`, incremental GC. |
| **3.14t** (free-threaded) | Experimental | Requires `maturin build --no-default-features` — abi3 is incompatible until PEP 803 "abi3t" lands. Tracked, not deployed. |

`pyproject.toml` uses `target-version = "py312"` for ruff. Do not raise
the floor without also bumping the PyO3 `abi3-py3XX` feature in
`Cargo.toml` and the maturin build matrix.

---

## What We Use from 3.12

### PEP 695 `type` Statement

The primary reason for the 3.12 floor. Use it for every type alias.

```python
# Good
type MessageId = str
type ListName = str
type LoreUrl = str
type SearchHit = LoreSearchHit | ThreadSummary

# Bad — legacy TypeAlias (still works, but not our style)
from typing import TypeAlias
MessageId: TypeAlias = str
```

### PEP 695 Generic Syntax

```python
# Good — 3.12 generic class / function syntax
class Cursor[T]:
    value: T
    offset: int

def first[T](items: list[T]) -> T | None:
    return items[0] if items else None

# Use TypeVar only when you need bounds/constraints 695 cannot express
```

### f-string Grammar Relaxation

Multi-line f-strings with embedded quotes, backslashes, and comments
are valid. Useful in our structlog bindings and query-grammar error
messages.

```python
log.error(
    f"regex rejected: pattern={pattern!r} "
    f"reason={reason!r} dfa_states={states}"
)
```

### Per-Interpreter GIL (PEP 684)

Available, not used. Our parallelism story is Rust + rayon behind
`Python::detach()`. Subinterpreters would just fragment the tantivy
reader and the compressed-store file handle.

---

## What We Use from 3.13 (Available Everywhere We Ship)

### `TypeIs` (PEP 742)

Narrows types on both branches — better than `TypeGuard` for router
dispatch.

```python
from typing import TypeIs

from kernel_lore_mcp.models import LoreSearchHit, LoreThreadNode

def is_search_hit(item: LoreSearchHit | LoreThreadNode) -> TypeIs[LoreSearchHit]:
    return hasattr(item, "tier_provenance")

def render(item: LoreSearchHit | LoreThreadNode) -> str:
    if is_search_hit(item):
        # ty knows: item is LoreSearchHit here
        return f"{item.cite_key}: {item.subject}"
    # ty knows: item is LoreThreadNode here
    return f"{item.message_id} depth={item.depth}"
```

### TypeVar Defaults (PEP 696)

```python
class Page[T = LoreSearchHit]:
    items: tuple[T, ...]
    next_cursor: str | None
```

### `@override` (PEP 698)

Required on every method override. Catches rename drift when we
restructure `tools/` or `routes/`.

```python
from typing import override

from fastmcp.server import FastMCP

class KernelLoreServer(FastMCP):
    @override
    async def run_async(self, *args, **kwargs) -> None:
        ...
```

### `@deprecated` (PEP 702)

```python
from warnings import deprecated

@deprecated("Use lore_search with tier='bm25' instead")
def legacy_prose_search(query: str) -> list[LoreSearchHit]: ...
```

### ReadOnly TypedDict (PEP 705)

Only used in the `_core.pyi` stubs where a dict crosses the PyO3
boundary and we want to mark identity fields as immutable. Prefer
pydantic or dataclass for everything else.

---

## What We Use from 3.14 (When Running on 3.14)

### Deferred Annotations (PEP 649 + PEP 749)

Forward references "just work" — no `from __future__ import annotations`,
no string quotes.

```python
# kernel_lore_mcp/router/tree.py — fine on 3.14 without quotes
class ThreadNode:
    parent: ThreadNode | None = None
    children: tuple[ThreadNode, ...] = ()
    message_id: str = ""
```

Accessing annotations at runtime (e.g. in our pydantic model
derivation): prefer `annotationlib.get_annotations()` on 3.14,
`typing.get_type_hints()` elsewhere.

```python
try:
    import annotationlib

    def read_annotations(obj: object) -> dict[str, object]:
        return annotationlib.get_annotations(
            obj, format=annotationlib.Format.VALUE
        )
except ImportError:
    from typing import get_type_hints as read_annotations  # type: ignore[assignment]
```

### Incremental GC

Transparent. Matters for our ingestion process because it shortens
max GC pause; relevant when streaming ~350 GB of lore through
`gix::ThreadSafeRepository`.

### Template Strings (PEP 750) — Not Used

We do not build SQL or HTML. Noted for completeness; skip.

### Subinterpreters (PEP 734) — Not Used

Parallelism lives in Rust. Skip.

---

## Free-Threaded Python (PEP 779)

**Tracked, not deployed.**

- abi3 and free-threaded are mutually exclusive until PEP 803 lands.
- Free-threaded build would require `maturin build --no-default-features`
  and a parallel wheel matrix.
- Our Rust core is already `Send + Sync` where it needs to be
  (`tantivy::IndexReader`, `gix::ThreadSafeRepository`), so once PEP
  803 stabilizes we can add a free-threaded wheel without touching
  algorithm code.
- Do not set `PYTHON_GIL=0` in production.

---

## Typing Style

### Union Syntax — Always `|`

```python
# Good
def lookup(message_id: str) -> LoreMessage | None: ...
def fetch(url: str, timeout: float = 30.0) -> bytes | None: ...

# Bad — legacy typing module unions
from typing import Optional, Union
def lookup(message_id: str) -> Optional[LoreMessage]: ...
def fetch(url: str, timeout: float = 30.0) -> Union[bytes, None]: ...
```

### Generic Syntax — PEP 695 First

```python
# Good
type HitPage = tuple[LoreSearchHit, ...]

class Paginator[T]:
    items: tuple[T, ...]
    cursor: str | None

# Bad — legacy TypeVar/Generic
from typing import TypeVar, Generic
T = TypeVar("T")
class Paginator(Generic[T]): ...
```

Exception: use `TypeVar` when you need bounds or constraints PEP 695
cannot express.

### `tuple[X, ...]` vs PEP 646 Variadics

- Use `tuple[Hit, ...]` for homogeneous variable-length sequences
  (every search result list).
- Use `tuple[str, int]` (fixed-size) for heterogeneous records —
  prefer NamedTuple or dataclass if the positions are load-bearing.
- Do **not** reach for `TypeVarTuple` / `Unpack` (PEP 646) unless
  you are writing a generic shape-preserving transform. We do not
  have one; the router returns `tuple[LoreSearchHit, ...]`, not a
  variadic tuple shape.

### Match Statement

Use for router dispatch and for decoding tier provenance enums.
Exhaustiveness is verified by `ty`.

```python
from kernel_lore_mcp.models import TierProvenance

def explain(tier: TierProvenance) -> str:
    match tier:
        case TierProvenance.METADATA:
            return "Arrow/Parquet metadata tier"
        case TierProvenance.TRIGRAM:
            return "fst+roaring trigram tier"
        case TierProvenance.BM25:
            return "tantivy BM25 tier"
```

Avoid match on raw strings when an Enum is available — ty cannot
check exhaustiveness on open string sets.

### Exception Groups (PEP 654)

Use `except*` when fanning out parallel work with `asyncio.TaskGroup`
or `anyio.create_task_group`. Our `ingest` pipeline uses rayon in
Rust, so Python-side fan-out is rare — but when we aggregate
per-list reload errors in `state.py`, exception groups are the
correct shape.

```python
import asyncio

async def reload_all(lists: tuple[str, ...]) -> None:
    try:
        async with asyncio.TaskGroup() as tg:
            for name in lists:
                tg.create_task(reload_one(name))
    except* FileNotFoundError as eg:
        log.error("generation file missing", errors=eg.exceptions)
    except* PermissionError as eg:
        log.error("reader denied", errors=eg.exceptions)
```

---

## Compatibility Notes

### Writing Code That Runs on 3.12, 3.13, and 3.14

- Don't use `annotationlib` unconditionally — guard with try/except
  (see above).
- Forward references: keep them workable without deferred
  evaluation. On 3.12/3.13, prefer quoted strings or tight module
  ordering over `from __future__ import annotations` so `ty` can see
  the real types at check time.
- `typing.get_type_hints()` is the portable way to resolve
  annotations. It eagerly evaluates on 3.12 and handles deferred
  evaluation on 3.14.

### Things That Changed 3.12 → 3.14 That Can Bite Us

| Change | Where it shows up |
|--------|-------------------|
| `multiprocessing` default start method → `forkserver` on Linux (3.14) | We do not use `multiprocessing` — rayon handles parallelism — but be explicit if you add it. |
| `ast.Num`/`ast.Str`/`ast.Bytes` removed | No AST handling in this codebase; ignore. |
| `pkgutil.find_loader()` removed | Use `importlib.util.find_spec()`. |
| `int()` no longer calls `__trunc__` | Implement `__int__`/`__index__` if you add a numeric wrapper around a Rust type. |

---

## Import Conventions

Ruff's `I` rule enforces import ordering. Write them once, let ruff
maintain.

```python
import asyncio
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import Literal

import httpx
import structlog
from fastmcp import FastMCP
from pydantic import BaseModel, Field

from kernel_lore_mcp._core import Router  # Rust extension
from kernel_lore_mcp.models import LoreSearchHit
from kernel_lore_mcp.tools.search import lore_search
```

stdlib → third-party → first-party, blank line between groups. No
wildcard imports. No unused imports (`F401` is on).

---

## What We Do Not Use

- **`from __future__ import annotations`** in new files. We are on
  3.12+, and on 3.14 it is redundant. Keep existing occurrences if
  removing them would break module-load ordering; otherwise drop.
- **JIT (`PYTHON_JIT=1`)** in production. Performance lives in Rust.
- **Template strings (PEP 750)**. No use case here.
- **Subinterpreters (PEP 734)**. Rust owns parallelism.
- **`typing.TypeAlias`** in new code — `type` statement supersedes it.
- **`typing.Optional`, `typing.Union`, `typing.List`, `typing.Dict`,
  `typing.Tuple`** in new code — use `X | None`, `X | Y`, `list[X]`,
  `dict[K, V]`, `tuple[X, ...]`.

---

## Cross-references

- [code-quality.md](code-quality.md) — ruff target-version, ty settings
- [pyo3-maturin.md](pyo3-maturin.md) — abi3-py312 floor and free-threaded notes
- [Rust counterpart](../rust/language.md) — edition 2024, MSRV
