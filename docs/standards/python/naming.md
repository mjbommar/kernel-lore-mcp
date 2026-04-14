# Naming Conventions

Adapted from `../../../../../273v/kaos-modules/docs/python/naming.md`.

Clear, consistent naming is the cheapest form of documentation. A
well-chosen name eliminates the need for a comment; a poorly-chosen
name creates ambiguity that propagates through every call site.

See also: [Rust counterpart](../rust/naming.md) for Rust-side identifier
rules and `Py*` prefix conventions on `#[pyclass]` types.

---

## General Principles

### Clarity Over Brevity

A name should be as long as it needs to be and no longer. One-letter
names are acceptable only in tight scopes (loop indices, lambda params,
generic type variables). Everywhere else, the name must communicate
intent without requiring the reader to look up its definition.

```python
# Good — intent is clear
hit_count = len(hits)
cite_key = hit.cite_key
reload_delay_seconds = 2.0

# Bad — saves keystrokes, costs understanding
hc = len(hits)
ck = hit.cite_key
rd = 2.0
```

### Consistency Within a Module

Same concept, same name. If you call it `message_id` in one function,
do not call it `mid` in the next. If we standardize on `cite_key` do
not introduce `citation_key` elsewhere.

```python
# Consistent
def fetch_message(message_id: str) -> LoreMessage | None: ...
def fetch_thread(message_id: str) -> ThreadTree: ...

# Inconsistent
def fetch_message(message_id: str) -> LoreMessage | None: ...
def fetch_thread(mid: str) -> ThreadTree: ...   # why the rename?
```

### Abbreviation Rules

**Acceptable abbreviations** (universally understood in our domain):

- `id`, `url`, `uri`
- `html`, `json`, `csv`, `pdf`, `sql`
- `io`, `db`, `api`
- `ctx` (context — when the full word is used dozens of times)
- `idx` (tight loops only)
- `config`, `params`
- **Domain-specific but universal in kernel lore**: `cve`, `cid`
  (commit id), `oid` (git object id), `mbox`, `mid` (message id — only
  when abbreviating a compound like `midx`, otherwise spell
  `message_id`), `lkml`, `lore`, `vger`

**Unacceptable abbreviations** (ambiguous or unnecessary):

- `doc`, `mgr`, `impl`, `proc`, `info`
- `tmp`, `temp` (what is temporary about it?)
- `misc`, `util`, `helper` (meaningless categories)
- `mid` as a standalone variable — spell `message_id`.

---

## Variables and Parameters

### Nouns for Data, Verbs for Actions

```python
# Variables — nouns
hit_count = len(hits)
thread_root = tree.root
active_lists: dict[str, ListHandle] = {}

# Functions — verbs
def count_hits(page: LoreSearchPage) -> int: ...
def build_thread(message_id: str) -> ThreadTree: ...
def open_list(name: str) -> ListHandle: ...
```

### Singular vs. Plural

```python
hit = hits[0]
hits = page.hits

trailer = message.trailers.signed_off_by[0]
signed_off_by = message.trailers.signed_off_by

# Mappings — plural key + singular value
lists_by_name: dict[str, ListHandle] = {}
readers_by_generation: dict[int, IndexReader] = {}
```

### Boolean Variables

Read as assertions. Use `is_`, `has_`, `can_`, `should_`, `allows_`.

```python
is_cover_letter = message.is_cover_letter
has_patch = message.has_patch
can_reload = router.generation_changed()
should_apply_default_rt = "rt:" not in parsed_query.operators
```

### Numeric Variables with Units

```python
# Good — units are explicit
reload_timeout_seconds = 30.0
max_regex_dfa_states = 10_000
candidate_cap = 4096
body_length_bytes = len(message.body)

# Bad
timeout = 30.0              # seconds? ms?
max_states = 10_000         # states of what?
cap = 4096                  # what unit?
```

### No Meaningless Prefixes

```python
# Good
cursor_key = b"..."
settings = Settings()

# Bad
my_cursor_key = b"..."
the_settings = Settings()
str_cursor_key = b"..."
```

---

## Functions and Methods

### Verb + Object

```python
# Good
def parse_query(text: str) -> ParsedQuery: ...
def reload_reader(reader: IndexReader) -> None: ...
def sign_cursor(payload: CursorPayload, key: bytes) -> str: ...
def decode_cursor(token: str, key: bytes) -> CursorPayload: ...

# Bad
def query_parse(text: str) -> ParsedQuery: ...       # wrong order
def cursor_signature(payload, key) -> str: ...       # noun phrase
def do_reload(reader) -> None: ...                   # "do" is a filler
```

### Common Verbs

| Verb | Meaning | Example |
|------|---------|---------|
| `build` | Assemble from parts | `build_server()`, `build_thread()` |
| `open` | Attach to existing resource | `open_list()`, `open_router()` |
| `load` | Read from storage | `load_generation()`, `load_schema()` |
| `store` | Write to storage | `store_message()` |
| `parse` | Convert from external format | `parse_query()`, `parse_mbox()` |
| `encode` / `decode` | Symmetric serialize | `encode_cursor()` / `decode_cursor()` |
| `register` | Add to a registry | `register_tool()` |
| `resolve` | Determine final value | `resolve_settings()` |
| `validate` | Check correctness | `validate_cursor()` |
| `extract` | Pull data from a source | `extract_trailers()`, `extract_touched_files()` |
| `search` | Query with ranking | `search_bm25()`, `search_trigram()` |
| `iter` | Yield lazily | `iter_hits()`, `iter_thread_nodes()` |

### Predicate Functions

```python
def is_regex_query(term: str) -> bool: ...
def has_patch(message: LoreMessage) -> bool: ...
def can_serve(reader: IndexReader) -> bool: ...
```

### Private Functions

Single-underscore prefix for module-internal helpers.

```python
def _normalize_subject(raw: str) -> str: ...
def _build_default_rt_filter() -> DateFilter: ...
```

---

## Classes

### Noun Phrase + PascalCase

```python
# Good
class LoreSearchHit: ...
class ThreadTree: ...
class CursorKey: ...
class RouterSettings: ...

# Bad
class ParseLoreMessage: ...   # sounds like a function
class ActiveRouter: ...       # state, not identity
```

### Base Classes and Mixins

Prefix abstract bases with `Base` when "the thing" alone is too generic.

```python
class BaseTool: ...         # abstract
class BaseResource: ...

# No prefix needed when the name is already abstract enough
class Tier: ...
```

### Settings / Configuration Classes

Suffix with `Settings`.

```python
from pydantic_settings import BaseSettings, SettingsConfigDict

class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_prefix="KLMCP_", env_file=".env")
    bind: str = "127.0.0.1"
    port: int = 8787
    data_dir: Path
    cursor_key: SecretStr
```

For per-subsystem settings:

```python
class IngestSettings(BaseSettings): ...
class RouterSettings(BaseSettings): ...
```

---

## Modules and Files

### Module Naming

Lowercase with underscores, short but descriptive.

```
src/kernel_lore_mcp/server.py            # FastMCP assembly
src/kernel_lore_mcp/config.py            # pydantic-settings
src/kernel_lore_mcp/models.py            # pydantic response models
src/kernel_lore_mcp/logging_.py          # underscore suffix avoids stdlib clash
src/kernel_lore_mcp/tools/search.py      # one file per MCP tool
src/kernel_lore_mcp/tools/thread.py
src/kernel_lore_mcp/tools/activity.py
src/kernel_lore_mcp/routes/status.py
src/kernel_lore_mcp/routes/metrics.py
```

```
# Bad
src/kernel_lore_mcp/server_implementation_details.py   # too long
src/kernel_lore_mcp/srv.py                             # too abbreviated
src/kernel_lore_mcp/helpers.py                         # meaningless
src/kernel_lore_mcp/logging.py                         # shadows stdlib
```

### Test File Naming

Mirror the source structure with `test_` prefix.

```
src/kernel_lore_mcp/router/query.py       → tests/python/unit/test_query.py
src/kernel_lore_mcp/tools/search.py       → tests/python/integration/test_lore_search.py
src/kernel_lore_mcp/cursor.py             → tests/python/unit/test_cursor.py
```

---

## Constants and Enums

### Constants — UPPER_SNAKE_CASE

```python
DEFAULT_LIMIT = 20
MAX_LIMIT = 200
CURSOR_LIFETIME_SECONDS = 900
MAX_REGEX_DFA_STATES = 10_000
TRIGRAM_CANDIDATE_CAP = 4096
DEFAULT_RT_YEARS = 5
```

### Enums

Enum class names are PascalCase nouns. Members are UPPER_SNAKE_CASE.

```python
from enum import Enum

class Tier(str, Enum):
    METADATA = "metadata"
    TRIGRAM = "trigram"
    BM25 = "bm25"

class CursorKind(str, Enum):
    PAGE = "page"
    THREAD = "thread"
```

---

## Type Aliases for Pydantic Models

We use PEP 695 `type` statements for aliases that show up in tool
signatures and response schemas. The alias name stays singular; any
collection wraps the alias.

```python
# Good — clear singular alias, explicit collection at use sites
type Hit = LoreSearchHit
type ThreadNode = LoreThreadNode
type TierProvenance = MetadataProvenance | TrigramProvenance | BM25Provenance

async def lore_search(query: str, *, limit: int = 20) -> tuple[Hit, ...]:
    ...

async def lore_thread(message_id: str) -> tuple[ThreadNode, ...]:
    ...

# Bad — plural alias hides the collection shape
type Hits = list[LoreSearchHit]                 # reader can't tell it's a list
async def lore_search(...) -> Hits: ...         # what's the element type?
```

For discriminated unions, the alias sits alongside the Literal-tagged
member models:

```python
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

---

## Python-Side Rust Module Naming

The Rust extension is named **`_core`**, not `_native`, `_rust`, or
`_ext`. This is fixed in `pyproject.toml`:

```toml
[tool.maturin]
module-name = "kernel_lore_mcp._core"
```

Rules:
- The import is `from kernel_lore_mcp import _core` or
  `from kernel_lore_mcp._core import Router`.
- Type stubs live at `src/kernel_lore_mcp/_core.pyi`.
- `py.typed` sits beside it.
- The Rust-side `#[pymodule]` entry point is named `_core` to match.
- **Do not** write `_native`, `_rust`, or `_ext`. One name, everywhere.

### `#[pyclass]` Types — `Py*` Prefix on Rust, Clean Name on Python

Inside the Rust crate we use the `Py*` convention so generic and
bound versions are distinguishable:

```rust
#[pyclass(module = "kernel_lore_mcp._core", name = "Router")]
pub struct PyRouter { ... }

#[pyclass(module = "kernel_lore_mcp._core", name = "TrigramIndex")]
pub struct PyTrigramIndex { ... }
```

But the `name` attribute strips the prefix so Python sees:

```python
from kernel_lore_mcp._core import Router, TrigramIndex
```

Never let the `Py*` prefix leak into the Python surface.

---

## MCP Tool Naming

MCP tool names follow a specific shape. The MCP spec caps names at 64
characters and we enforce `snake_case` for Python-idiomatic tool
identifiers that also register cleanly as FastMCP attribute names.

### Rules

1. **snake_case only**. Letters, digits, underscores. No hyphens, no
   camelCase.
2. **Under 64 characters.** MCP spec requirement; ours fit easily.
3. **Noun or `verb_object` — not just a verb.** The tool name must
   make sense when prefixed mentally by "use the tool to …".
4. **Module prefix `lore_`** for every tool that queries lore data.
   Matches the public brand, distinguishes from any future non-lore
   tools (e.g. an `admin_` surface).
5. **`readOnlyHint: true`** annotation on every v1 tool. No tool in
   v1 writes anything; this is load-bearing for clients deciding
   whether to auto-approve.
6. **Stable across versions.** Changing a tool name is a breaking
   change. Add new names, deprecate old ones via `@deprecated`.

### The v1 Tool Set

| Name | Purpose |
|------|---------|
| `lore_search` | Query across all tiers |
| `lore_thread` | Resolve a thread tree by message id |
| `lore_patch` | Fetch parsed patch metadata for a message |
| `lore_patch_diff` | Fetch the raw diff body from the compressed store |
| `lore_activity` | Aggregate by file/author/series over a window |
| `lore_message` | Fetch a single message by id or cite_key |
| `lore_series_versions` | List sibling versions of a patch series |

### Registration Example

```python
# src/kernel_lore_mcp/tools/search.py
from typing import Annotated

from fastmcp import FastMCP
from pydantic import Field

from kernel_lore_mcp.models import LoreSearchResponse


def register(mcp: FastMCP) -> None:
    @mcp.tool(
        name="lore_search",
        description="Search lore.kernel.org across all tiers.",
        annotations={"readOnlyHint": True},
    )
    async def lore_search(
        query: Annotated[str, Field(description="lei-compatible query string")],
        limit: Annotated[int, Field(ge=1, le=200)] = 20,
        cursor: Annotated[str | None, Field(description="opaque HMAC-signed cursor")] = None,
    ) -> LoreSearchResponse:
        ...
```

Notes:
- The tool name is the string `"lore_search"`, which is also the
  Python function identifier. Keep them identical to avoid confusion.
- `annotations={"readOnlyHint": True}` is non-optional for our v1
  surface — a test enforces this for every registered tool.
- Tool registration is **explicit** — we do not rely on
  side-effect imports (see `CLAUDE.md` proscription).

### Bad Tool Names

```python
# Bad — too short, no module prefix
name="search"

# Bad — camelCase
name="loreSearch"

# Bad — hyphens (not snake_case)
name="lore-search"

# Bad — verb phrase without object
name="lore_run"

# Bad — leaks implementation detail
name="lore_tantivy_bm25_search"
```

---

## Summary Checklist

- [ ] Variable names are nouns; function names start with verbs
- [ ] Singular for one item, plural for collections
- [ ] Booleans start with `is_`, `has_`, `can_`, `should_`
- [ ] Numeric names include units (`_seconds`, `_bytes`, `_states`)
- [ ] Abbreviations only from the accepted list
- [ ] Classes are PascalCase noun phrases
- [ ] Constants are UPPER_SNAKE_CASE
- [ ] Type aliases use `type` statement (PEP 695), singular name
- [ ] Rust extension imports as `kernel_lore_mcp._core` — never
      `_native`, `_rust`, or `_ext`
- [ ] `#[pyclass]` uses `Py*` prefix in Rust, clean name in Python
- [ ] MCP tools are `snake_case`, `lore_`-prefixed, under 64 chars,
      `readOnlyHint: true`
- [ ] Test files mirror source with `test_` prefix

---

## Cross-references

- [code-quality.md](code-quality.md) — ruff enforces import ordering
- [pyo3-maturin.md](pyo3-maturin.md) — `_core` module, `Py*` prefix,
  stub files
- [data-structures.md](data-structures.md) — when to reach for pydantic
  vs dataclass vs tuple
- [Rust counterpart](../rust/naming.md) — Rust-side identifier rules
