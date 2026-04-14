# Boundaries — Rust

Rust counterpart to `../../python/design/boundaries.md`.

A boundary is where data changes trust level, where code
changes dependency set, or where one compilation unit talks to
another. Our code has three boundaries that matter and one that
doesn't:

1. **Pure-Rust core vs PyO3 glue.** Biggest one. Every
   `#[pyfunction]` is a thin wrapper around a pure-Rust
   function.
2. **Library (`rlib`) vs binary (`bin`).** `thiserror` in
   library; `anyhow` in binaries only.
3. **Tier interfaces.** `ingest` is a producer;
   `store`/`metadata`/`trigram`/`bm25` are consumers; `router`
   is another consumer.
4. **HTTP / MCP.** Doesn't exist on the Rust side. Lives
   entirely in Python (`fastmcp`). Our Rust crate has no HTTP
   code, no async runtime, no tokio.

---

## Boundary 1 — pure-Rust core vs PyO3 glue

### The thin-wrapper rule

Every `#[pyfunction]` is a thin wrapper. It does four things,
in this order, and nothing else:

1. Convert Python inputs to Rust types (`&str` → `&str`, `PyDict`
   → a typed struct via serde or explicit extraction).
2. Release the GIL with `Python::detach(py, || { ... })`.
3. Call the pure-Rust function.
4. Translate `Result<T, Error>` to `PyResult<T>` via
   `?` (we have `impl From<Error> for PyErr` in `error.rs`).

No business logic in the wrapper. If you're tempted to add a
branch, a loop, or a match on the result, the branch belongs in
the pure function.

Shape:

```rust
// Pure Rust — lives in e.g. router.rs
pub fn search(data_dir: &Path, query: &str, limit: usize)
    -> crate::Result<Vec<Hit>> { ... }

// Glue — lives in lib.rs inside #[pymodule] mod _core
#[pyfunction]
fn search<'py>(
    py: Python<'py>,
    data_dir: &str,
    query: &str,
    limit: usize,
) -> PyResult<Vec<Hit>> {
    let dir = Path::new(data_dir);
    let hits = py.detach(|| router::search(dir, query, limit))?;
    Ok(hits)
}
```

See `../libraries/pyo3.md` for the 0.28-specific details
(`detach`/`attach` names, `Bound<'py, T>` handling, pickle,
stubs).

### Why this matters

- **Testability.** Pure functions get unit tests that run in
  `cargo test` without spinning up a Python interpreter.
- **Rebuildability.** Re-using the core from `src/bin/reindex.rs`
  requires the core to not touch PyO3.
- **GIL discipline.** A fat wrapper that holds the GIL across a
  heavy call serializes every Python caller. `Python::detach`
  is the entire point of having a Rust side.

### What crosses the boundary

| Direction | Allowed types |
|---|---|
| Python → Rust | Primitives, `&str`, `Vec<u8>`, `bytes`, typed `#[pyclass]` inputs. |
| Rust → Python | Primitives, `String`, `Vec<T>` of owned, `#[pyclass]` instances (for complex structs — see `Py` prefix rule in `../libraries/pyo3.md`), `PyResult<T>`. |
| Never | Raw `anyhow::Error`, borrowed references with Python-side lifetimes that outlive the detach, generics with Python-specific bounds. |

`anyhow::Error` crossing the PyO3 boundary is a bug. Our
`Cargo.toml` keeps `anyhow` available for `src/bin/*.rs` only;
PyO3 code sees `crate::Error` exclusively. See `errors.md`.

---

## Boundary 2 — library (`rlib`) vs binary (`bin`)

### The one-line version

- **Library code uses `thiserror`.** Structured, matchable,
  convertible to `PyErr`.
- **Binary code uses `anyhow`.** Formatted for a human, never
  matched on.

### Why

Library errors cross module boundaries, tier boundaries, and
the PyO3 boundary. Every point that consumes them wants to ask
"is this a `QueryParse` or a `QueryTimeout`?" — that requires
a named enum variant. `thiserror` gives us that with a `#[from]`
conversion for each backing error type.

Binary errors are consumed by exactly one audience — a person
reading stderr — and the answer they want is "what went wrong,
with context." `anyhow` provides `with_context` chaining and a
pretty backtrace. Matching on an `anyhow::Error` is possible
but defeats the purpose.

### Layout

```rust
// src/error.rs (library)
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("query parse error: {0}")]
    QueryParse(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    ...
}
pub type Result<T> = std::result::Result<T, Error>;
```

```rust
// src/bin/reindex.rs (binary)
fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    reindex_all(&args).context("rebuilding indices from store")?;
    Ok(())
}
```

The binary calls into the library. When it does, the library
returns `crate::Error`; `anyhow::Error` has a blanket
`From<E: std::error::Error>` so `?` just works. The conversion
is one-way — library functions never accept or return
`anyhow::Error`.

### Cargo enforcement

`Cargo.toml` has both listed but the comment pins the intent:

```toml
anyhow = "1"                   # ONLY for src/bin/*.rs
thiserror = "2"                # library code
```

Clippy doesn't enforce this; code review does. A CI grep for
`anyhow::` outside `src/bin/` is a cheap safety net (TODO).

---

## Boundary 3 — tier interfaces

The three index tiers plus the compressed store form a
publish/subscribe boundary. `ingest` publishes; everyone else
subscribes.

### Per-tier contracts

Each tier module owns:

- Its on-disk layout (see the corresponding
  `../../../indexing/*.md` doc).
- Its write API (used only by `ingest` and `reindex`).
- Its read API (used by `router`).
- Its state-coherence contract (what `state::generation` ticks
  when).

Today the skeletons live as doc comments in
`src/{store,metadata,trigram,bm25}.rs`; this section documents
the contract they implement.

#### `store`

- **Writes:** append-only. One `SegmentWriter` per
  ingest-session per list. Flush on segment roll (1 GB) or
  session end. The segment must be `fsync`'d before
  `index.parquet` references it — crash safety contract.
- **Reads:** random-access by `(segment_id, offset, length)`.
  The metadata tier owns these coordinates; the store is purely
  positional.
- **State tick:** none on its own. Store writes are visible
  once the metadata Parquet referencing them is committed.

#### `metadata`

- **Writes:** one Parquet file per ingest session per list,
  with schema version stamped. `RecordBatch` → Parquet writer.
- **Reads:** predicate pushdown via Parquet page-stats.
  `router` pushes `list:`, `rt:`, `dfn:`, `dfhh:`, trailer
  filters here first.
- **State tick:** the commit that writes a new Parquet file is
  the point where `state::bump_generation()` fires.

#### `trigram`

- **Writes:** segment-per-ingest. Segments are immutable.
  `{trigrams.fst, trigrams.postings, trigrams.docs, meta.json}`.
  See `../../../indexing/trigram-tier.md`.
- **Reads:** open all segments for the queried list, union.
  Intersection of posting bitmaps → candidate docids → confirm
  by decompressing from `store`.
- **State tick:** a new segment rename is observable via the
  shared `generation` counter.

#### `bm25`

- **Writes:** single `tantivy::IndexWriter` system-wide, held
  by the ingest process. `state::writer_lockfile()` is
  `flock`'d for its lifetime. See `../libraries/tantivy.md`.
- **Reads:** `ReloadPolicy::Manual`. Every query-entry
  `stat()`s `state::generation`; if advanced, `reader.reload()`.
- **State tick:** `IndexWriter::commit()` → `bump_generation()`.

### Cross-tier invariants

1. **`message_id` is the join key.** Every tier stores it.
   Router merges by it. Never ordinal position.
2. **`body_sha256` is the data-integrity check.** Computed at
   store write, recorded in metadata, compared at reindex.
3. **Schema version is global.** `schema::SCHEMA_VERSION` is
   stamped into metadata Parquet and into the BM25 analyzer
   fingerprint sidecar. A mismatch is a loud error, not silent
   corruption. See `bm25.rs`.
4. **The store is source of truth.** Nuke all three index tiers
   and `reindex` rebuilds from the store. See
   `../../../indexing/compressed-store.md`. If a piece of data
   lives only in a derived tier and not in the store, that's a
   bug — fix the ingest path.

---

## Boundary 4 — HTTP / MCP (not Rust's problem)

Every byte of HTTP, SSE-alternative (Streamable HTTP),
authentication, and MCP framing lives on the Python side
(`src/kernel_lore_mcp/`). Rust exposes a set of
`#[pyfunction]` that take query strings and return result
models. FastMCP routes MCP calls to those.

**Do not** add tokio, reqwest, axum, hyper, warp, or any HTTP
crate to `Cargo.toml`. Any feature that looks like "we need
HTTP in Rust" is either:

- A fetch that belongs in Python (probably `httpx`), or
- A subprocess call to `grokmirror` (already the case).

See `../../../mcp/` for the Python side.

---

## Data-typing at the boundaries

Same principle as the Python guide: validate once at the edge,
then operate on typed data. Concrete applications:

- **Query strings** arrive from Python as `&str`. They are
  parsed into a `Query` AST in `router.rs`. The AST, not the
  string, is what every internal function takes.
- **Cursors** arrive as base64-encoded HMAC-signed blobs. They
  are parsed into a `Cursor` struct at the entry point; bad
  HMAC returns `Error::InvalidCursor`. Nothing internal sees
  the raw bytes.
- **Paths** from Python arrive as `&str` and are converted to
  `Path` / `PathBuf` once, at the entry point.
- **Hits going out** are `Vec<Hit>` where `Hit` is a plain
  `#[pyclass]` (or `pythonize`-friendly struct). Internal
  functions operate on `Hit`; the PyO3 layer only wraps.

---

## Decision table

| Question | Answer |
|---|---|
| Where do I put business logic that a `#[pyfunction]` will call? | Pure-Rust function in the appropriate tier/orchestrator module. The pyfunction only wraps. |
| Can I use `anyhow::Result` inside a library function? | No. `crate::Result<T, crate::Error>`. |
| Can a `#[pyfunction]` return `anyhow::Error`? | No. Library returns `crate::Error`; `From<Error> for PyErr` does the translation. |
| Can `metadata.rs` call into `trigram.rs` directly? | No — sibling rule (`modules.md`). Router composes them. |
| Do I need tokio for this? | No. If you think you do, the task belongs in Python. |
| Where does HTTP live? | Not in this crate. Python side. |

See also:
- `modules.md` — the module tree these boundaries run through.
- `concurrency.md` — why no tokio; rayon-only discipline.
- `errors.md` — thiserror + anyhow split in full.
- `../ffi.md` — the authoritative PyO3 cost-model document.
