# Data Structures and Data Philosophy

Adapted from `../../../../../273v/kaos-modules/docs/python/data-structures.md`.

Choosing the right data structure is the highest-leverage design
decision in any module. The wrong choice creates friction at every
call site; the right choice makes the code obvious.

See also: [Rust counterpart](../rust/data-structures.md) for the Rust
side (`Arc<T>`, `Cow<'a, str>`, Arrow schemas).

---

## Decision Framework

Start with the simplest structure that works. Move up the stack only
when you gain something concrete — validation at a boundary, named
fields, immutability guarantees.

```
Primitives & builtins
  ↓ need named fields?
dataclass (frozen=True, slots=True)
  ↓ need validation or serialization at a boundary?
Pydantic BaseModel (frozen=True)
```

**Columnar analytics, on-disk Parquet, and the actual index tiers are
Rust-side** — `arrow`, `parquet`, `fst`, `roaring`, `tantivy`. The
Python side never owns those structures; it receives finalized
pydantic models across the PyO3 boundary. Keep it that way.

### Quick Reference

| When you need... | Use | Why |
|---|---|---|
| A simple container for 2–5 values | `tuple` or `NamedTuple` | Zero overhead, immutable, unpacking |
| A lookup table | `dict` or `frozenset` | O(1) access, stdlib |
| A structured internal record | `@dataclass(frozen=True, slots=True)` | Named fields, immutable, fast, zero deps |
| Data crossing the MCP boundary (in or out) | Pydantic `BaseModel` (frozen) | Runtime validation, JSON serialization, `outputSchema` generation |
| Configuration with env-var resolution | `pydantic-settings` | `SecretStr`, env prefix, `.env` parsing |
| Columnar analytics / on-disk metadata | **Rust-side** (`arrow`, `parquet`) | Lives behind `_core`; Python never holds the DataFrame |
| Immutable tree (e.g. thread tree) | Pydantic frozen + `tuple` children | Hashable, round-trippable JSON |

---

## The Core Rule

**Pydantic at every external boundary. Dataclasses for everything
internal. Tuples and NamedTuples for trivial value groups.**

- **External boundary** means: an MCP tool input, an MCP tool output,
  a pydantic-settings environment variable, a `/status` JSON response,
  a cursor payload (before HMAC signing), anything that is serialized
  to JSON for a client.
- **Internal** means: values we hold between the router and the MCP
  tool function; the intermediate result of decoding a cursor; the
  dataclass wrappers around raw `_core` dict returns.

This rule is load-bearing. Bare `dict` from an MCP tool is a
proscription in `CLAUDE.md` — FastMCP derives `outputSchema` from the
pydantic model and emits `structuredContent` accordingly.

---

## Tier 1: Primitives and Builtins

Python's built-in types are fast, well-understood, and sufficient for
most internal data passing. Do not reach for a library when a `tuple`,
`dict`, `list`, or `set` will do.

### Tuples for Immutable Sequences

Use `tuple` when the data is fixed-size, ordered, and should not be
mutated. We use `tuple` extensively for pydantic model children to
enforce immutability (lists inside frozen models are not actually
immutable).

```python
# Fixed collection of hits — immutable by construction
hits: tuple[LoreSearchHit, ...] = ()

# Return multiple values
def split_patch(raw: str) -> tuple[str, str]:
    """Split a message body at the first diff --git line."""
    idx = raw.find("\ndiff --git")
    return raw[:idx], raw[idx + 1:]

# Spans, coordinates, byte ranges
snippet_span: tuple[int, int] = (0, 240)
patch_range: tuple[int, int] = (1024, 4096)
```

### NamedTuple for Lightweight Records

When you need named fields but no methods, validation, or mutability:

```python
from typing import NamedTuple

class ByteSpan(NamedTuple):
    offset: int
    length: int

class GenerationStamp(NamedTuple):
    value: int
    mtime: float
```

Prefer `NamedTuple` over a 2-field dataclass when the values are
positional and "obviously a pair" (coordinate, span, stamp). Prefer a
dataclass when there are 3+ fields or any of them need defaults.

### Dicts for Dynamic Mappings

Use `dict` only for data where keys are not known at development time.

```python
# Good — keys are dynamic (per-list, per-tier)
readers_by_list: dict[str, IndexReader] = {}
tier_timings_ms: dict[str, float] = {}

# Bad — keys are static and known → use a dataclass or model
hit = {"message_id": "...", "score": 0.95, "cite_key": "..."}  # should be typed
```

### Sets and Frozensets for Membership

```python
visited_oids: set[str] = set()
required_trailers: frozenset[str] = frozenset({
    "signed-off-by", "reviewed-by", "acked-by"
})
```

`frozenset` is hashable — usable as dict keys and in other sets.

---

## Tier 2: Dataclasses

Use `@dataclass` when you need named fields with type annotations but
do not need runtime validation or JSON serialization. This is the
workhorse for internal data structures — especially the typed
wrappers around raw `_core` dict returns.

### Always Use `frozen=True` and `slots=True`

Immutable dataclasses are safer (no accidental mutation), hashable
(usable in sets and as dict keys), and `slots=True` gives 20–30%
memory savings with faster attribute access.

```python
from dataclasses import dataclass

@dataclass(frozen=True, slots=True)
class RawHit:
    """Internal wrapper around a _core dict hit. Not exposed over MCP."""
    message_id: str
    score: float
    tier: str
```

### Use `field()` for Defaults and Control

```python
from dataclasses import dataclass, field

@dataclass(frozen=True, slots=True)
class QueryBudget:
    limit: int = 20
    trigram_candidate_cap: int = 4096
    excluded_lists: frozenset[str] = field(default_factory=frozenset)
    _parsed_at_ms: int = field(default=0, repr=False, compare=False)
```

### Validate in `__post_init__`

For light validation (not full pydantic), use `__post_init__`. With
`frozen=True`, use `object.__setattr__` if you need to set computed
fields:

```python
@dataclass(frozen=True, slots=True)
class CursorPayload:
    generation: int
    offset: int
    query_hash: str

    def __post_init__(self) -> None:
        if self.offset < 0:
            raise ValueError(f"offset must be non-negative, got {self.offset}")
        if len(self.query_hash) != 64:
            raise ValueError("query_hash must be 64 hex chars (sha256)")
```

### When NOT to Use Dataclasses

- **At the MCP boundary** (tool inputs, tool outputs, resources): use
  pydantic — you need runtime validation, JSON serialization, and
  `outputSchema` derivation.
- **For configuration**: use `pydantic-settings` — you need env-var
  resolution and `SecretStr`.
- **For the cursor payload's serialized form**: use pydantic — the
  JSON round-trip is the whole point. Use a dataclass only for the
  pre-serialization in-memory form if you prefer.

---

## Tier 3: Pydantic Models

Use pydantic at **boundaries** — where data enters or leaves your
code: MCP tool inputs/outputs, configuration, JSON serialization, and
anything a client sees.

### Immutable Models for Data

```python
from pydantic import BaseModel, Field

class Snippet(BaseModel, frozen=True):
    offset: int = Field(ge=0)
    length: int = Field(gt=0)
    sha256: str = Field(pattern=r"^[0-9a-f]{64}$")
    text: str


class LoreSearchHit(BaseModel, frozen=True):
    message_id: str
    cite_key: str
    from_addr: str
    lore_url: str
    subject: str
    subject_tags: tuple[str, ...] = ()
    is_cover_letter: bool = False
    series_version: int | None = None
    series_index: tuple[int, int] | None = None   # (N, M) from "N/M"
    has_patch: bool
    patch_stats: PatchStats | None = None
    snippet: Snippet
    tier_provenance: tuple[TierProvenance, ...]
    is_exact_match: bool
    cross_posted_to: tuple[str, ...] = ()
```

### ConfigDict for Behavior

```python
from pydantic import BaseModel, ConfigDict

class ToolInput(BaseModel):
    model_config = ConfigDict(
        frozen=True,
        str_strip_whitespace=True,
        extra="forbid",        # reject unknown fields from clients
    )

    query: str = Field(min_length=1, max_length=2048)
    limit: int = Field(ge=1, le=200, default=20)
    cursor: str | None = None
```

`extra="forbid"` is important at the MCP boundary — we want typos in
client requests to fail loudly, not be silently ignored.

### Settings with pydantic-settings

The `Settings` class is where the 6-level resolution hierarchy lives
(defaults → `.env` → env vars → explicit constructor args).

```python
from pathlib import Path

from pydantic import Field, SecretStr
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_prefix="KLMCP_",
        env_file=".env",
        extra="ignore",
    )

    bind: str = "127.0.0.1"
    port: int = Field(default=8787, ge=1, le=65535)
    data_dir: Path
    cursor_key: SecretStr
    default_rt_years: int = 5
```

Always use `SecretStr` for keys and tokens. `SecretStr.__repr__` is
`**********`, and accidentally logging settings does not leak the
value. The value is retrieved via `.get_secret_value()` at the point
of use.

### Discriminated Unions for Polymorphism

For type-safe unions (tier provenance, cursor kinds), use pydantic's
discriminated union with a `Literal` type field.

```python
from typing import Literal

from pydantic import BaseModel


class MetadataProvenance(BaseModel, frozen=True):
    tier: Literal["metadata"] = "metadata"
    fields_matched: tuple[str, ...]


class TrigramProvenance(BaseModel, frozen=True):
    tier: Literal["trigram"] = "trigram"
    confirmed_by_regex: bool


class BM25Provenance(BaseModel, frozen=True):
    tier: Literal["bm25"] = "bm25"
    score: float


type TierProvenance = MetadataProvenance | TrigramProvenance | BM25Provenance
```

With the `tier` Literal, pydantic picks the right model on validation
and `ty` narrows correctly in match statements.

### When NOT to Use Pydantic

- **For internal computation** where fields are correct by
  construction: use dataclasses — pydantic's validation overhead is
  wasted.
- **For hot-loop data** (per-hit, per-token structures inside the
  router): use dataclasses with `slots=True` or plain tuples — pydantic
  model creation is ~10× slower than dataclass creation.
- **For PyO3 wrappers** (the typed dataclass around a raw `_core`
  dict): use dataclasses — they don't pull pydantic into the binding
  layer's import graph.

---

## Tuple-vs-NamedTuple-vs-Dataclass Guidance

A recurring judgment call. Our rules:

| Shape | Choice |
|-------|--------|
| 2 positional values with obvious names (span, pair, stamp) | `NamedTuple` |
| 2 values with no obvious names or temporary | `tuple[A, B]` |
| 3+ fields, all required, no defaults, no methods | `NamedTuple` or `dataclass` — pick `dataclass` if any field needs a non-zero default or validation |
| 3+ fields, at least one needs a default or validation | `@dataclass(frozen=True, slots=True)` |
| Crosses the MCP boundary | Pydantic `BaseModel(frozen=True)` |
| Holds secrets or settings | Pydantic `BaseSettings` |

```python
# Tuple — ad-hoc 2-value return
def split_subject(raw: str) -> tuple[tuple[str, ...], str]:
    """Return (tags, cleaned_subject)."""
    ...

# NamedTuple — named 2-value record with stable identity
from typing import NamedTuple

class Span(NamedTuple):
    offset: int
    length: int

# Dataclass — 4+ fields, internal
@dataclass(frozen=True, slots=True)
class RouterMetrics:
    query_count: int
    reload_count: int
    avg_query_ms: float
    last_generation: int

# Pydantic — MCP boundary, with validation
class LoreMessageResponse(BaseModel, frozen=True):
    message_id: str
    ...
```

---

## Anti-Patterns

### The Premature Pydantic

```python
# Bad — Pydantic for internal counter state
class RouterState(BaseModel):
    query_count: int = 0
    reload_count: int = 0

# Good — plain dataclass
@dataclass(slots=True)
class RouterState:
    query_count: int = 0
    reload_count: int = 0
```

### The Dict-of-Everything

```python
# Bad — untyped dict passed through 5 functions
hit = {"score": 0.95, "text": "...", "mid": "...", "cite": "..."}

# Good — typed pydantic at the boundary, dataclass internally
@dataclass(frozen=True, slots=True)
class RawHit:
    score: float
    snippet_text: str
    message_id: str
    cite_key: str
```

### The Mutable Default

```python
# Bad — shared mutable default
@dataclass
class QueryBudget:
    excluded_lists: list[str] = []   # SHARED across instances!

# Good — frozen, with a default_factory to a frozenset
@dataclass(frozen=True, slots=True)
class QueryBudget:
    excluded_lists: frozenset[str] = field(default_factory=frozenset)
```

### The List-Inside-Frozen-Pydantic

```python
# Bad — frozen model with mutable list; callers can still list.append()
class Page(BaseModel, frozen=True):
    hits: list[LoreSearchHit] = []   # still mutable!

# Good — tuple, genuinely immutable
class Page(BaseModel, frozen=True):
    hits: tuple[LoreSearchHit, ...] = ()
```

`frozen=True` on the pydantic model freezes attribute assignment, not
the elements inside a `list`. Use `tuple` everywhere.

### The Bare `dict` From An MCP Tool

```python
# Bad — bare dict; CLAUDE.md proscription
@mcp.tool(name="lore_message", annotations={"readOnlyHint": True})
async def lore_message(message_id: str) -> dict:
    ...

# Good — pydantic; FastMCP derives outputSchema + emits structuredContent
@mcp.tool(name="lore_message", annotations={"readOnlyHint": True})
async def lore_message(message_id: str) -> LoreMessageResponse:
    ...
```

---

## The Ecosystem Preference

**Prefer stdlib + a small, well-chosen set of dependencies over
adding libraries.**

Before reaching for a library, ask:

1. Can this be done with stdlib in ~100 lines? → Do it.
2. Can this be done inside the existing Rust core in ~500 lines? → Do
   it there — it's probably the right home anyway.
3. Does a high-quality, well-maintained library exist that we cannot
   reasonably replicate? → Only then add the dependency, and log the
   decision in a commit message.

External dependencies create version conflicts, security exposure,
and maintenance burden. 1000 lines of focused, tested, benchmarked
code is better than a 50,000-line library we use for one feature.

---

## Cross-references

- [naming.md](naming.md) — pydantic type alias conventions
- [code-quality.md](code-quality.md) — ty narrowing on `X | None`
- [pyo3-maturin.md](pyo3-maturin.md) — the dataclass wrappers around
  raw `_core` dict returns
- [Rust counterpart](../rust/data-structures.md) — Arrow/Parquet
  schemas, `Arc<T>`, `Cow<'a, str>`
