# Errors — Rust

Rust counterpart to `../../python/design/errors.md`.

The Python guide's three-part rule (what went wrong, how to
fix, alternative) applies verbatim to the *messages* we emit.
What's Rust-specific is the type system: one error enum per
crate, `thiserror` for derivation, strict separation from
`anyhow`, and a single `impl From<Error> for PyErr` that
every Python-facing call relies on.

---

## One `Error` per crate

We have one crate (`kernel_lore_mcp`) and therefore one
`crate::Error`. It lives in `src/error.rs`. Every library
function returns `crate::Result<T>` — an alias for
`std::result::Result<T, crate::Error>`.

Current shape (abbreviated; authoritative version in the code):

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("query parse error: {0}")]
    QueryParse(String),

    #[error("regex too complex for DFA-only engine: {0}")]
    RegexComplexity(String),

    #[error("query exceeded {limit_ms} ms wall-clock limit")]
    QueryTimeout { limit_ms: u64 },

    #[error("invalid cursor: {0}")]
    InvalidCursor(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("gix error: {0}")]
    Gix(String),

    #[error("mail parse error: {0}")]
    MailParse(String),

    #[error("tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),

    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    #[error("state inconsistency: {0}")]
    State(String),
}
```

### Design rationale

- **One enum, not one-per-module.** 11 modules with 11 error
  types become 100+ conversions. Kernel-lore-mcp is a single
  crate; a single enum is right-sized.
- **Named variants, not `Generic(String)`.** Each variant
  classifies a failure mode. Callers pattern-match; wrappers
  map to different Python exception types
  (see `From<Error> for PyErr` below).
- **`#[from]` for leaf errors only.** `io::Error`,
  `TantivyError`, `ArrowError`, `ParquetError`. Everything else
  is constructed explicitly so the error message can carry
  context.
- **`Gix(String)` / `MailParse(String)` not `#[from]`.** The
  upstream error types are fine but not `pub`-ly structured
  enough to match on; we stringify at the boundary with
  context.

---

## Message content — the three-part rule

Same rule as Python. Every error message has:

1. **What went wrong** (concrete, not "operation failed").
2. **How to fix it** (when there's a fix).
3. **Alternative approach** (when there's one).

Good example:

```rust
return Err(Error::RegexComplexity(format!(
    "pattern {pat!r} contains backreference \\{n}; \
     DFA-only engine rejects backrefs. \
     Rewrite without backrefs, or use substring predicates \
     (dfa:, dfb:) instead of /regex/."
)));
```

Bad example (what we don't write):

```rust
return Err(Error::RegexComplexity("bad regex".into()));
```

The Python side will surface this via FastMCP to an LLM client.
The LLM can only self-correct if the message tells it how.

### When part 3 doesn't apply

Fall through to additional diagnostic detail, not silence:

```rust
return Err(Error::QueryTimeout { limit_ms: 5000 });
// Display renders: "query exceeded 5000 ms wall-clock limit"
```

The LLM reading this knows to narrow the query (`list:`, `rt:`);
even without spelled-out advice in the variant, the error kind
itself is actionable.

---

## `#[from]` conversions

Every `#[from]`-annotated variant provides a `From<E>` impl,
letting `?` lift foreign errors without ceremony:

```rust
fn open_parquet(path: &Path) -> crate::Result<ParquetReader> {
    let f = File::open(path)?;     // io::Error -> Error::Io
    let r = ParquetReader::new(f)?; // ParquetError -> Error::Parquet
    Ok(r)
}
```

Rules for `#[from]`:

- **Use it for leaf errors** — `io::Error`, tantivy, arrow,
  parquet. These already have good messages; we just want the
  kind.
- **Don't use it for cross-module errors.** If another
  internal module returns `crate::Error`, `?` already works;
  no `#[from]` needed.
- **Don't use it when the source needs context.** Prefer:

  ```rust
  .map_err(|e| Error::State(format!(
      "read generation file at {}: {}", path.display(), e
  )))?
  ```

  over a blind `?` that drops the "which file, which step"
  context.

### anyhow-style context is available without anyhow

We avoid `anyhow` in library code, but the context-adding
pattern is:

```rust
.map_err(|e| Error::Gix(format!("open {}: {}", path.display(), e)))?
```

This is `anyhow::Context::with_context` by hand. More typing,
but matchable downstream (variant is `Gix`, not `anyhow`
opaque).

---

## `impl From<Error> for PyErr`

The single bridge. Every `#[pyfunction]` returning
`PyResult<T>` relies on this impl — `?` on
`crate::Result<T>` converts through it.

Current impl (in `src/error.rs`):

```rust
impl From<Error> for PyErr {
    fn from(e: Error) -> Self {
        match e {
            Error::QueryParse(_)
            | Error::RegexComplexity(_)
            | Error::InvalidCursor(_) => PyValueError::new_err(e.to_string()),
            _ => PyRuntimeError::new_err(e.to_string()),
        }
    }
}
```

Mapping guidelines (expand as we grow):

| Rust variant | Python exception | Reason |
|---|---|---|
| `QueryParse` | `ValueError` | User input was malformed. |
| `RegexComplexity` | `ValueError` | User regex rejected by policy. |
| `InvalidCursor` | `ValueError` | Cursor is HMAC-tampered or parsed wrong. |
| `QueryTimeout` | `TimeoutError` (future) — today `RuntimeError` | Wall-clock cap; user can narrow. |
| `Io`, `Parquet`, `Arrow`, `Tantivy`, `Gix`, `MailParse`, `State` | `RuntimeError` | Internal / environmental. |

Rule: **user errors → `ValueError`-family; system errors →
`RuntimeError`-family.** FastMCP's error handling treats these
differently (422 vs 500); the MCP client sees "I asked for
something bad" vs "the server broke" distinctly.

When adding a new variant, update the match. `#[non_exhaustive]`
would force this but cost us the compile-error-on-miss property
the match currently has — prefer letting the match fail to
build.

---

## `anyhow` — binaries only

Bright line: **`anyhow::Error` never crosses a PyO3 boundary.**

### Allowed: `src/bin/*.rs`

```rust
// src/bin/reindex.rs
use anyhow::{Context as _, Result};

fn main() -> Result<()> {
    let args = parse_args().context("parsing reindex arguments")?;
    reindex_all(&args).context("rebuilding indices from store")?;
    Ok(())
}
```

The binary formats the error for a human (stderr) and exits.
No one pattern-matches on it.

### Not allowed: `src/*.rs` (everything else)

No `anyhow::Error` return types. No `anyhow::anyhow!` macro.
No `anyhow::bail!`.

If a library function wants "just stringify this failure", that
becomes an `Error` variant that carries the context:

```rust
// BAD — library function
fn load_dict(path: &Path) -> anyhow::Result<Dict> { ... }

// GOOD
fn load_dict(path: &Path) -> crate::Result<Dict> {
    read_bytes(path)
        .map_err(|e| Error::State(format!(
            "load zstd dict at {}: {}", path.display(), e
        )))?;
    ...
}
```

### Why the bright line

- **Matchable downstream.** A caller can distinguish
  `Error::State` from `Error::RegexComplexity`. `anyhow::Error`
  flattens everything to a string.
- **`From<Error> for PyErr` is total and compile-checked.**
  `From<anyhow::Error> for PyErr` would have to choose one
  Python exception for every possible underlying error —
  wrong every time.
- **Test behavior.** `cargo test` can assert on variant
  (`matches!(err, Error::QueryParse(_))`). With `anyhow` you'd
  assert on the message — brittle.

### Cargo hygiene

`Cargo.toml` has:

```toml
anyhow = "1"    # ONLY for src/bin/*.rs
thiserror = "2"
```

No per-target feature gating; `anyhow` is a regular
dependency. Enforcement is by code review (and eventually a CI
grep for `anyhow::` outside `src/bin/`).

---

## Result-propagation discipline

Use `?` everywhere. Don't write:

```rust
// BAD
match some_op() {
    Ok(v) => v,
    Err(e) => return Err(e),
}

// BAD
match some_op() {
    Ok(v) => v,
    Err(e) => panic!("unexpected: {e}"),
}
```

The second is especially bad: it turns a recoverable error into
a process crash. Panics are for bugs, not failures.

### When to `.unwrap()` / `.expect()`

Acceptable in:

- Test code (`#[cfg(test)]`).
- One-time init where failure is a build-configuration bug
  (e.g., `Regex::new(KNOWN_GOOD_PATTERN).unwrap()` for a
  compile-time-known pattern).

Unacceptable in:

- Any path reachable from a `#[pyfunction]`.
- Any path reachable from the ingest shard walk.
- Any code that handles user input.

Use `expect` over `unwrap` when you unwrap — the message
documents the invariant:

```rust
let sha = trailers
    .iter()
    .find(|t| t.key == "Fixes")
    .expect("caller confirmed fixes-trailer present");
```

---

## Panics — when and why

Panics indicate bugs. We don't swallow them, we don't `catch`
them, we fix them.

PyO3's `#[pyfunction]` catches panics and converts them to a
`PyRuntimeError` — the Python side sees "rust panic: {msg}".
That's a safety net; it's not a substitute for returning
`Result`. If you're relying on that catch, restructure the code.

Situations where panics are correct:

- Invariant violation that means the data structure is
  corrupt (`state::generation` went backward).
- Index out of bounds in a tight loop where we've already
  proven the bound.
- Allocation failure (happens as a panic by default in Rust).

For the corruption case, panic with a loud message and exit;
don't try to recover a corrupt index:

```rust
panic!(
    "state::generation regressed: prev={prev} new={new}; \
     index is corrupt, rebuild with `reindex --from-store`"
);
```

---

## Summary

| Decision | Answer |
|---|---|
| How many error types per crate? | One. `crate::Error`. |
| `thiserror` or `anyhow` for libraries? | `thiserror`. |
| `anyhow` anywhere? | `src/bin/*.rs` only. |
| How does Rust error become Python exception? | Single `impl From<Error> for PyErr` in `error.rs`. |
| Use `#[from]` for every error? | No — leaf errors only. Cross-module Rust errors use `?` directly; upstream errors that need context are mapped explicitly. |
| `.unwrap()` in library code? | Only for compile-time-known invariants, with an `.expect()` message. |
| Panic in a `#[pyfunction]` path? | No. PyO3 catches, but the point is to return `Result`. |

See also:
- `../../python/design/errors.md` — the three-part message rule
  we mirror.
- `boundaries.md` — where `From<Error> for PyErr` sits in the
  layering.
- `../ffi.md` — overall PyO3 cost model.
