# Naming Conventions

Rust counterpart to [`../python/naming.md`](../python/naming.md).

Clear names are the cheapest form of documentation. Rust also layers
in type-level naming conventions (PascalCase types, snake_case
functions, SCREAMING_SNAKE constants) that the compiler informally
enforces via `non_snake_case` / `non_camel_case_types` lints.

---

## Principles (mirrored from Python)

1. **Clarity over brevity.** A local loop index can be `i`; a module
   symbol cannot.
2. **Consistency within a module.** If the metadata tier calls it
   `message_id`, the trigram tier calls it `message_id` — not `mid`,
   `msg_id`, or `message`.
3. **Abbreviations only when standard.** `id`, `url`, `db`, `io`,
   `api`, `ctx`, `idx`, `cfg` are fine. `mgr`, `impl`, `tmp`, `misc`
   are not.

---

## Crate and module naming

### Crate name

Our crate is `kernel_lore_mcp` (declared in `Cargo.toml`). Crate
names use **snake_case**. Never `kernel-lore-mcp` in Cargo (though
the repo directory uses hyphens — that's fine).

Python imports the native extension as
`kernel_lore_mcp._core`, which is why `[lib].name = "_core"` in
[`Cargo.toml`](../../../Cargo.toml).

### Module names

`snake_case`, short, descriptive. The current tree is a good model:

```
src/
├── error.rs      # good — one concept, named after it
├── state.rs      # good — cross-tier state management
├── schema.rs     # good — shared field defs
├── store.rs      # good — compressed raw store
├── metadata.rs   # good — metadata tier
├── trigram.rs    # good — trigram tier
├── bm25.rs       # good — BM25 tier
├── ingest.rs     # good — ingestion pipeline
├── router.rs     # good — query router
└── lib.rs
```

Bad (don't add these):

- `util.rs`, `helpers.rs`, `misc.rs` — meaningless bucket names.
- `types.rs` — every Rust file defines types; the name is vacuous.
  If a bag-of-types module is genuinely the right split, name it
  after the *concept* (e.g., `cursor.rs` for cursor types).
- `common.rs`, `shared.rs` — same problem.
- `impl.rs` — reserved-ish and meaningless.

### Test module naming

Integration tests in `tests/<snake_case>.rs`. Bench files in
`benches/<snake_case>.rs`. Mirror the source file being tested when
it makes sense:

```
src/router.rs        -> tests/router_grammar.rs
src/trigram.rs       -> tests/trigram_confirm.rs
src/ingest.rs        -> tests/ingest_smoke.rs
```

---

## Function naming

`snake_case`. Verb-first, object-next — same as Python.

```rust
// Good — verb + object
pub fn parse_query(input: &str) -> Result<Query, Error> { ... }
pub fn extract_trailers(body: &[u8]) -> Trailers { ... }
pub fn build_trigram_index(...) -> Result<TrigramIndex, Error> { ... }
pub fn open_reader(path: &Path) -> Result<Reader, Error> { ... }

// Bad — noun, wrong order, or ambiguous
pub fn query(input: &str) -> ... { ... }       // verb OR noun?
pub fn trailer_extract(body: &[u8]) -> ... { ... }
pub fn reader(path: &Path) -> ... { ... }      // "reader" is a noun
```

### Common verb vocabulary (same as Python side)

| Verb | Meaning | Example |
|------|---------|---------|
| `new` | Basic constructor | `Cursor::new(...)` |
| `with_*` | Constructor variant | `Cursor::with_key(...)` |
| `build` | Assemble from parts | `build_trigram_index` |
| `open` / `load` | Read from disk | `open_reader`, `load_dict` |
| `commit` / `finalize` | Persist | `commit_segment` |
| `parse` | External -> internal | `parse_query`, `parse_rfc822` |
| `serialize` / `encode` | Internal -> external | `encode_cursor` |
| `extract` | Pull fields from a blob | `extract_trailers` |
| `walk` / `iter_*` | Lazy traversal | `walk_commits`, `iter_candidates` |
| `confirm` | Post-hoc verify candidates | `confirm_trigram_hits` |
| `dispatch` | Route to a handler | `dispatch_predicate` |

### Predicate functions — return `bool`

Start with `is_`, `has_`, `can_`, `should_`:

```rust
pub fn is_cover_letter(subject: &str) -> bool { ... }
pub fn has_patch(message: &Message) -> bool { ... }
pub fn can_dfa(pattern: &str) -> bool { ... }
```

### Constructors

`new` — infallible, no meaningful configuration.

```rust
impl Cursor {
    pub fn new(mid: &str, score: f32) -> Self { ... }
    pub fn with_date(mid: &str, date_ns: i64) -> Self { ... }
}
```

Constructors that can fail return `Result<Self, Error>` and are named
after the operation they perform:

```rust
impl Index {
    pub fn open(path: &Path) -> Result<Self, Error> { ... }
    pub fn create(path: &Path, schema: &Schema) -> Result<Self, Error> { ... }
}
```

### Private items

Lower-cased, start with `_` or not (your call — but be consistent).
We do NOT use a leading underscore as a "private" marker in Rust
because the visibility modifier (`pub`, `pub(crate)`, private) already
does that. Leading underscore means "intentionally unused" —
```rust
fn _todo_stub() {}   // not yet implemented, don't yell at me
```

---

## Type naming — PascalCase, with conventional suffixes

### Structs and enums

PascalCase, noun or noun phrase:

```rust
pub struct Cursor { ... }
pub struct TrigramIndex { ... }
pub struct SearchHit { ... }
pub enum Query { ... }
pub enum TimeRange { ... }
```

### Suffixes we use intentionally

| Suffix | When | Example |
|--------|------|---------|
| `Error` | On error enums | `Error` (crate-wide; see `src/error.rs`) |
| `Kind` | Sub-classification enum nested inside a struct | `ErrorKind`, `PredicateKind` |
| `Builder` | Staged construction | `IngestPipelineBuilder` |
| `Config` | Settings/config struct passed at construction | `IngestConfig`, `ReaderConfig` |
| `Handle` | Opaque, cheap-to-clone reference | `ShardHandle` |
| `Ref` | Borrowed projection of a larger type | `MessageRef<'a>` |
| `Iter` | Iterator type returned from a method | `CommitIter<'a>` |

### Suffixes we reject

| Suffix | Why |
|--------|-----|
| `Ty` | Looks like a type variable; never clearer than the base name |
| `Obj` | Everything is an "object" — meaningless |
| `Impl` | Implementation detail shouldn't leak into the type name |
| `Data` / `Info` | Vague. Say what the data *is*. `CommitInfo` -> `Commit` or `CommitMetadata` |
| `Manager` | Almost always a sign of unclear responsibility. Split it. |
| `Helper` / `Util` | Same problem. |
| `Stub` / `Dummy` / `Fake` | Stub code should not ship; tests can name them locally. |

### Error types

One enum named `Error` per crate. Variants describe the failure by
noun or short phrase:

```rust
// src/error.rs
pub enum Error {
    QueryParse(String),
    RegexComplexity(String),
    QueryTimeout { limit_ms: u64 },
    InvalidCursor(String),
    Io(std::io::Error),
    State(String),
    Tantivy(tantivy::TantivyError),
    Arrow(arrow::error::ArrowError),
    Parquet(parquet::errors::ParquetError),
    // ...
}
```

Variant *name* is the failure mode (`QueryParse`). Variant *payload*
is what the caller needs. Do NOT name a variant `ErrorQueryParse`
(stutters against the enum name).

### Enum variants

PascalCase, short. Prefer one word when meaning is unambiguous:

```rust
pub enum Predicate {
    Subject(String),      // not SubjectTerm, not PredSubject
    From(String),
    Trailer { name: String, value: String },
    Regex(RegexAst),
}
```

### Traits

PascalCase. Traits describing a capability end in `-able`, `-er`, or
are an adjective/noun:

```rust
pub trait Ingestible { ... }     // "can be ingested"
pub trait Scorer { ... }          // "does scoring"
pub trait FromBytes<'a>: Sized { ... }
```

Avoid trait names like `TantivyTrait`, `HelperTrait` — if you can't
name the capability, the trait probably isn't one.

---

## Constants and statics

`SCREAMING_SNAKE_CASE`:

```rust
// src/trigram.rs
pub const TRIGRAM_CONFIRM_LIMIT: usize = 4096;
```

Include the unit or the domain in the name when applicable:

```rust
pub const QUERY_TIMEOUT_MS: u64 = 5_000;
pub const MAX_REGEX_SIZE_BYTES: usize = 64 * 1024;
pub const INGEST_BATCH_COMMITS: usize = 4_096;
```

Bare numbers (`const LIMIT: usize = 100;`) tell the reader nothing.

---

## Lifetime and generic parameters

### Lifetimes

Short, lowercase, one letter for the common case. Multi-letter when
the role is worth naming:

```rust
pub fn body<'a>(msg: &'a Message) -> &'a [u8] { ... }
pub struct View<'input, 'index> { ... }    // when 'a/'b would confuse
```

Prefer `'_` (elided) when only one lifetime is in play and it can be
inferred.

### Type parameters

Short, uppercase — `T`, `U`, `K`, `V`. Named when bounded:

```rust
pub fn score<S: Scorer>(s: &S, hits: &[Hit]) -> Vec<ScoredHit> { ... }
pub trait Analyzer { type Token; ... }

// With a descriptive name when the role is specific
pub struct Writer<Body: AsRef<[u8]>> { ... }
```

Do NOT write `THit` or `TMessage` — the prefix `T` as a "this is
generic" marker is Pythonic noise; Rust's position and context make
it clear already.

---

## Field and parameter naming

`snake_case` nouns. Booleans use `is_`, `has_`, `can_` prefixes.

```rust
pub struct Hit {
    pub message_id: String,
    pub list: String,
    pub score: f32,
    pub is_exact_match: bool,
    pub has_patch: bool,
    pub tier_provenance: Vec<Tier>,
}
```

Numeric fields with units carry the unit in the name:

```rust
pub struct QueryConfig {
    pub timeout_ms: u64,
    pub max_candidates: usize,
    pub max_regex_size_bytes: usize,
}
```

---

## PyO3-specific naming

See [`ffi.md`](ffi.md) for the full FFI rules. For naming:

- `#[pyclass]` types on the Rust side carry a `Py` prefix:
  ```rust
  #[pyclass(module = "kernel_lore_mcp._core", name = "Hit")]
  pub struct PyHit { inner: Hit }
  ```
  The `name = "Hit"` makes Python see it as `Hit`; the Rust-side
  `PyHit` clarifies "this is the FFI wrapper."
- `#[pyfunction]` functions use the same `snake_case` name on both
  sides unless we explicitly rename with `#[pyo3(name = "...")]`.
- Pure-Rust types that never cross the boundary do not get a `Py`
  prefix. Only boundary wrappers do.

---

## Cross-language consistency

Where Rust and Python both name the same concept, they match:

| Rust | Python |
|------|--------|
| `Cursor` | `Cursor` (pydantic) |
| `Hit` / `PyHit` | `LoreHit` (pydantic wrapper around raw dict return) |
| `Query` | `Query` |
| `TimeRange` | `TimeRange` |
| `Tier::Metadata` | `"metadata"` (string in `tier_provenance`) |

Keep the shared vocabulary synchronized. A mismatch between
`message_id` in Arrow / tantivy / pydantic is the sort of bug that
eats an afternoon.

---

## Checklist

- [ ] Module name is a concept, not a bucket (no `util`, `misc`).
- [ ] Function names start with a verb.
- [ ] Booleans read as questions (`is_*`, `has_*`, `can_*`).
- [ ] Numeric fields include units (`_ms`, `_bytes`, `_count`).
- [ ] Struct / enum names are PascalCase nouns.
- [ ] Error enum is named `Error`; variants describe failure modes.
- [ ] Constants are `SCREAMING_SNAKE_CASE` with units in the name.
- [ ] No banned suffixes: `Ty`, `Obj`, `Impl`, `Stub`, `Manager`.
- [ ] PyO3 wrapper types use `Py` prefix; Python-visible name set via
      `#[pyclass(name = "...")]`.
- [ ] Shared vocabulary matches the Python side (`message_id`,
      `list`, `tier_provenance`, ...).

---

## Cross-references

- [`index.md`](index.md) — standards index.
- [`../python/naming.md`](../python/naming.md) — Python counterpart.
- [`language.md`](language.md) — language features, derive rules.
- [`ffi.md`](ffi.md) — PyO3 boundary, `Py`-prefixed classes.
