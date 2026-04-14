# Pydantic v2

> Adapted from KAOS `docs/python/libraries/pydantic.md`. Pared to the
> surface we actually use: MCP request/response models, settings,
> cursor validators.
>
> See also: [`../index.md`](../index.md),
> [`../design/boundaries.md`](../design/boundaries.md),
> [`fastmcp.md`](fastmcp.md).

Pydantic v2 is the validation and serialization layer at every
boundary in `kernel-lore-mcp`. The MCP wire uses it. The settings
layer uses it. The response shapes exposed to the LLM are all pydantic
`BaseModel`.

---

## 1. Where pydantic shows up

| Boundary | File | What |
|---|---|---|
| MCP tool input | `models.py` → `SearchRequest`, `ThreadRequest`, etc. | FastMCP validates wire JSON against the model. |
| MCP tool output | `models.py` → `SearchResponse`, `SearchHit`, `Freshness`, etc. | FastMCP auto-derives `outputSchema` + emits `structuredContent`. |
| Settings | `config.py` → `Settings(BaseSettings)` | Env var resolution, `SecretStr` for `KLMCP_CURSOR_KEY`. |
| Resources | `resources/*.py` → `BlindSpotsCoverage`, etc. | Same shape as tool outputs. |

No pydantic in hot loops. No pydantic on the Rust side of `_core`.

---

## 2. Model configuration

Every model declares `model_config`. The default (no `ConfigDict`) is
almost never what we want.

### Two profiles that cover everything we need

```python
from pydantic import BaseModel, ConfigDict

# Wire-facing — MCP request/response, resource payloads
class WireModel(BaseModel):
    model_config = ConfigDict(extra="forbid", populate_by_name=True)

# Immutable data — snippets, cursor state, anything we hash
class ImmutableModel(BaseModel):
    model_config = ConfigDict(frozen=True, extra="forbid")
```

### ConfigDict options we use

| Option | When |
|---|---|
| `extra="forbid"` | Every wire-facing model. Reject unknown fields so clients get a clear error. |
| `frozen=True` | Snippets (hashed), cursor state, any model we pass as a dict key. |
| `extra="ignore"` | Settings only. New env vars must not crash old deployments. |
| `populate_by_name=True` | When field aliases exist (rare in this project). |
| `str_strip_whitespace=True` | Tool input strings (query text). |

### Good vs. bad

```python
# Bad — default config; extra fields accepted silently
class SearchRequest(BaseModel):
    q: str
    max_results: int = 20

# Good — strict
class SearchRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    q: str = Field(min_length=1, max_length=10_000)
    max_results: int = Field(default=20, ge=1, le=200)
```

---

## 3. Field constraints

Use `Field(...)` to encode constraints declaratively. These flow
into JSON Schema → MCP `inputSchema` → the LLM tool catalog.

```python
from pydantic import BaseModel, ConfigDict, Field

class SearchRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    q: str = Field(
        min_length=1,
        max_length=10_000,
        description=(
            "lei-compatible query. Supports s:, f:, d:, list:, rt:, "
            "b:, nq:, dfpre:, dfpost:, dfa:, dfb:, dfctx:, "
            "/<regex>/, tag:, trailer:<name>:<value>. Full grammar "
            "at docs/mcp/query-routing.md."
        ),
    )
    max_results: int = Field(
        default=20, ge=1, le=200,
        description="Max hits per page. Tighter values cost less.",
    )
    cursor: str | None = Field(
        default=None,
        description="Opaque HMAC-signed cursor from a previous response.",
    )
```

### Constraint reference

| Kind | Options |
|---|---|
| Numeric | `ge`, `le`, `gt`, `lt`, `multiple_of` |
| String | `min_length`, `max_length`, `pattern` |
| Collection | `min_length`, `max_length` |
| Everywhere | `description` (flows to JSON Schema) |

### Spend time on descriptions

The LLM's only view of a tool parameter is the `description` field
on the schema. A good description closes the gap between "it accepts
a string" and "here's how to compose a useful query." Look at
`models.py` — every field of `SearchRequest` / `SearchResponse` has
a description aimed at an LLM reader.

---

## 4. Validators

### `@field_validator` — single-field

```python
from pydantic import BaseModel, ConfigDict, Field, field_validator

class SearchRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    cursor: str | None = None

    @field_validator("cursor")
    @classmethod
    def _non_empty_cursor(cls, v: str | None) -> str | None:
        if v is not None and not v.strip():
            raise ValueError("cursor must be None or a non-empty string")
        return v
```

Rules:

- `@classmethod` is required on Pydantic v2 validators.
- Raise `ValueError` (pydantic wraps it into `ValidationError`).
- Return the possibly-transformed value.

### Cursor parsing — the typical case

The cursor is an opaque, HMAC-signed bytes blob, base64-url-encoded.
The actual HMAC verification lives in a helper; the pydantic
validator only enforces "looks like base64url and is non-empty":

```python
import re
from pydantic import field_validator

_B64URL_RE = re.compile(r"^[A-Za-z0-9_\-]+$")

class SearchRequest(BaseModel):
    cursor: str | None = None

    @field_validator("cursor")
    @classmethod
    def _cursor_shape(cls, v: str | None) -> str | None:
        if v is None:
            return None
        if not _B64URL_RE.match(v):
            raise ValueError("cursor must be base64url-encoded")
        return v
```

The actual HMAC check happens in the handler (it needs
`settings.cursor_signing_key`). A bad signature is not a pydantic
concern; it's a `ValueError` raised at the verification step and
caught at the tool boundary → `ToolError` with three-part message.

### `@model_validator(mode="before")` — raw input transform

Runs before per-field parsing. We use it in `Settings` for legacy env
var fallback, not for wire models.

```python
from pydantic import model_validator

class Settings(BaseSettings):
    cursor_signing_key: SecretStr | None = None

    @model_validator(mode="before")
    @classmethod
    def _legacy_env_fallback(cls, values: dict[str, Any]) -> dict[str, Any]:
        if not values.get("cursor_signing_key"):
            legacy = os.environ.get("KLMCP_CURSOR_HMAC_KEY")  # old name
            if legacy:
                values["cursor_signing_key"] = legacy
        return values
```

Key points:

- `mode="before"` + `@classmethod`.
- Return the mutated `values` dict, not `cls(...)`.
- Let pydantic do `str → SecretStr` coercion (don't do it yourself).

### `@model_validator(mode="after")` — cross-field invariants

Runs after all fields are parsed. Use it for constraints that span
multiple fields:

```python
class SearchRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    q: str = Field(min_length=1)
    cursor: str | None = None
    max_results: int = Field(default=20, ge=1, le=200)

    @model_validator(mode="after")
    def _cursor_pin(self) -> SearchRequest:
        # When resuming from a cursor, max_results is embedded in the
        # cursor itself; clients that send both must send matching values.
        # The real check (cursor.max_results == self.max_results) happens
        # after HMAC verification — but the shape rule is:
        # cursor-with-different-q is nonsensical.
        return self
```

---

## 5. Discriminated unions

We don't have a large AST, but discriminated unions may land for:

- Tool variants in a single `lore_*` endpoint (we currently split
  into per-tool files, so this is deferred).
- Response variants (e.g. `PatchResponse` vs `NoPatchResponse`).

Pattern:

```python
from typing import Annotated, Literal
from pydantic import BaseModel, ConfigDict, Field

class PatchHit(BaseModel):
    model_config = ConfigDict(extra="forbid")
    kind: Literal["patch"] = "patch"
    diff: str
    stats: PatchStats

class NoPatchHit(BaseModel):
    model_config = ConfigDict(extra="forbid")
    kind: Literal["no_patch"] = "no_patch"
    reason: str

PatchResult = Annotated[
    PatchHit | NoPatchHit,
    Field(discriminator="kind"),
]
```

Rules:

- Discriminator field is `Literal[...]` on every variant.
- Give it a matching default so callers don't have to spell it.
- Use `Annotated[..., Field(discriminator=...)]` syntax.

---

## 6. Serialization

```python
# Python dict
data = model.model_dump()
data = model.model_dump(exclude_none=True)

# JSON string (faster than json.dumps(model.model_dump()))
s = model.model_dump_json()

# Reconstruct from dict / JSON
obj = SearchResponse.model_validate(raw_dict)
obj = SearchResponse.model_validate_json(raw_str)
```

### Wrapping `_core` return shapes

Rust returns plain dicts. Wrap at the Python edge with
`model_validate`:

```python
raw = await asyncio.to_thread(_core.run_search, ...)
return SearchResponse(
    results=[SearchHit.model_validate(row) for row in raw["hits"]],
    freshness=Freshness.model_validate(raw["freshness"]),
    query_tiers_hit=raw["tiers"],
    default_applied=raw.get("default_applied", []),
)
```

`model_validate` catches drift between `_core.pyi` and the actual
Rust return shape — the most common way the PyO3 boundary breaks
silently.

### `model_construct` for trusted data

If and when we have a measured hot path building many response
objects from trusted Rust data, `Model.model_construct(...)` bypasses
validation. Rule of thumb: don't use it without a benchmark. The
validation cost for a dozen `SearchHit` objects per request is
invisible next to a tantivy query.

---

## 7. SecretStr

`KLMCP_CURSOR_KEY` is the one secret we handle. It's a `SecretStr`.

```python
from pydantic import SecretStr
from pydantic_settings import BaseSettings, SettingsConfigDict

class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_prefix="KLMCP_",
        env_file=".env",
        extra="ignore",
    )

    cursor_signing_key: SecretStr | None = None
    # ... other fields
```

### Access

```python
settings = Settings()

print(settings.cursor_signing_key)        # SecretStr('**********')
print(repr(settings.cursor_signing_key))  # SecretStr('**********')

# Only where we actually need the bytes
if settings.cursor_signing_key is None:
    raise RuntimeError("KLMCP_CURSOR_KEY required for HTTP mode")
raw_key = settings.cursor_signing_key.get_secret_value().encode("utf-8")
```

### Rules

- `SecretStr | None = None` for the optional case.
- Never `str` for a secret, even internally.
- Never log `get_secret_value()`. `structlog` doesn't auto-redact.
- The cursor **contents** (signed payload) are not secret — log them
  if debugging pagination. The **key** is.

---

## 8. Settings

Full pattern, matching `src/kernel_lore_mcp/config.py`:

```python
from __future__ import annotations

from pathlib import Path

from pydantic import Field, SecretStr
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_prefix="KLMCP_",
        env_file=".env",
        extra="ignore",
    )

    data_dir: Path = Field(default=Path("./data"))
    lore_mirror_dir: Path = Field(default=Path("./data/lore-mirror"))

    bind: str = Field(default="127.0.0.1")
    port: int = Field(default=8080, ge=1, le=65535)

    rate_limit_per_ip_per_minute: int = Field(default=60)

    cursor_signing_key: SecretStr | None = Field(default=None)

    freshness_cache_ttl_seconds: int = Field(default=30, ge=1)
    query_wall_clock_ms: int = Field(default=5000)
    thread_response_max_bytes: int = Field(default=5 * 1024 * 1024)
```

### Usage

```python
# In __main__.py — the edge
settings = Settings()    # reads env + .env

# Passed down through build_server(settings)
```

### Resolution priority

1. Explicit overrides to the `Settings(...)` constructor (tests).
2. Environment variables (`KLMCP_*`).
3. `.env` file.
4. Field defaults.

### Rules

- `extra="ignore"` so adding new fields in code doesn't crash older
  deployments.
- Secrets always `SecretStr`.
- Never read `os.environ` outside `config.py`.
- One `Settings` class. If it ever hits ~40 fields, split by concern
  (ingest vs serve) but keep them both under `config.py` until that's
  really justified.

---

## 9. Inheritance

We don't have deep pydantic hierarchies. If we end up with a family
of response models that share fields, the pattern is:

```python
class BaseWire(BaseModel):
    model_config = ConfigDict(extra="forbid", populate_by_name=True)
```

and derive wire-facing responses from `BaseWire`. Prefer
**composition** (a `freshness: Freshness` field) over multiple
inheritance.

---

## 10. Anti-patterns

### Missing `ConfigDict`

Defaults accept extra fields silently. Every wire model needs
`extra="forbid"`.

### Pydantic in hot loops

```python
# Bad — validated once per candidate row
for row in _core.scan(...):
    hit = SearchHit(**row)

# Good — do it once at the edge
return SearchResponse(
    results=[SearchHit.model_validate(r) for r in raw["hits"]],
    ...
)
```

Wrapping is cheap when you do it once. It's not cheap when you do it
in the trigram confirmation loop (which is why that loop lives in
Rust).

### `mode="after"` for env fallback

Requires `object.__setattr__` on frozen models and bypasses type
coercion. Always `mode="before"` + `@classmethod` for env fallback.

### `dict(model)` instead of `model_dump()`

`dict(model)` skips custom serializers and field exclusions. Use
`model.model_dump()`.

### Unconstrained strings on the MCP wire

```python
# Bad
class SearchRequest(BaseModel):
    q: str
    max_results: int

# Good
class SearchRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    q: str = Field(min_length=1, max_length=10_000)
    max_results: int = Field(default=20, ge=1, le=200)
```

### Logging `SecretStr.get_secret_value()`

Defeats the point of SecretStr. If you need to log that a key was
used, log something like `{"cursor_key_present": True}`, not the key.

---

## Quick reference

| Task | Pattern |
|---|---|
| MCP wire model | `BaseModel` + `ConfigDict(extra="forbid")` + rich `Field(description=...)` |
| Immutable data | `ConfigDict(frozen=True)` + `tuple` for collections |
| Settings | `BaseSettings` + `SettingsConfigDict(env_prefix="KLMCP_", extra="ignore")` |
| Secret | `SecretStr \| None = None` + env var + `mode="before"` legacy fallback if needed |
| Cross-field invariant | `@model_validator(mode="after")` |
| Single-field validation | `@field_validator("name")` + `@classmethod` |
| Wrap Rust return | `Model.model_validate(raw_dict)` |
| Serialize for wire | `model.model_dump()` / `model.model_dump_json()` |
