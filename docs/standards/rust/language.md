# Rust Language and Toolchain

Rust counterpart to [`../python/language.md`](../python/language.md).

kernel-lore-mcp is pinned to **Rust stable 1.88** (edition 2024) in
[`rust-toolchain.toml`](../../../rust-toolchain.toml). Any bump is a
project decision, logged in the commit message — not a casual
`rustup update`.

---

## Pins (authoritative: [`../../../CLAUDE.md`](../../../CLAUDE.md))

| Knob | Value | Notes |
|------|-------|-------|
| toolchain | `stable 1.88` | edition 2024 |
| MSRV | `1.88` | `rust-version = "1.88"` in `Cargo.toml` |
| Edition | `2024` | `edition = "2024"` |

MSRV is enforced by `cargo check --locked` in CI. Raising it is a
project-level decision; see [`cargo.md`](cargo.md) for the update
workflow.

---

## Edition 2024 features we rely on

Edition 2024 is the baseline. The features below are the ones we use
intentionally. Other 2024 features are fine if they are idiomatic;
avoid novelty for its own sake.

### `let-else` — early return with name binding

```rust
// src/router.rs (sketch)
fn parse_rt(token: &str) -> Result<TimeRange, Error> {
    let Some(rest) = token.strip_prefix("rt:") else {
        return Err(Error::QueryParse(format!("expected rt:, got {token}")));
    };
    TimeRange::parse(rest)
}
```

Use it when the "failure" branch is a return, a `continue`, or a
panic. Do NOT use `let-else` when the failure branch produces a
fallback value — that's what `unwrap_or` and `match` are for.

### `let chains`

Stable in 1.85. Lets you compose `if let` with `&&`:

```rust
if let Some(tag) = subject_tags.first()
    && tag == "GIT PULL"
    && has_patch
{
    // composite condition, still one branch
}
```

Prefer this over nested `if let` when the condition is genuinely one
guard. Three chained `&&` is the ceiling — past that, extract a named
predicate.

### `async fn in traits` (AFIT)

Stable. Use it when a trait is genuinely async-shaped. In this
project, async lives on the Python side (FastMCP, tokio). Rust code
is overwhelmingly sync — rayon parallelism, blocking I/O via gix and
tantivy. Do NOT introduce tokio inside the Rust core just to use
AFIT. See [`design/concurrency.md`](design/concurrency.md).

### `impl Trait` in argument position

```rust
pub fn score_hits(iter: impl IntoIterator<Item = RawHit>) -> Vec<ScoredHit> { ... }
```

Preferred over generic `<I: IntoIterator<Item = RawHit>>` for
single-use generics. Switch back to a named type parameter when the
caller needs `turbofish` or when the function needs `where` bounds on
the item type.

### GATs (generic associated types)

Allowed when a trait genuinely needs a lifetime-parameterized
associated type (e.g., streaming iterators). Do NOT reach for GATs
speculatively; most apparent GAT needs are solved by returning
`impl Iterator<Item = ...>` from a method.

### Type alias impl trait (TAIT)

Stable. Use sparingly — prefer concrete return types at module
boundaries. Acceptable inside a module to DRY up a complex
`impl Iterator` signature; avoid at the public API.

---

## Macros — when and when not

### When `#[derive]`

Prefer derive for everything the ecosystem provides:

- `Debug` — on every public type unless the `Debug` output would leak
  secrets (cursor HMAC keys, for example). If you suppress it, write
  a manual `Debug` impl that redacts.
- `Clone`, `Copy`, `Eq`, `Hash` — derive them when the semantics line
  up; DO NOT derive `Copy` on types larger than 16 bytes without a
  perf reason.
- `Serialize` / `Deserialize` — via `serde` derive. Required for
  cursor payloads, state files, ingest intermediate forms.
- `thiserror::Error` — see [`design/errors.md`](design/errors.md).
  Every variant has a `#[error("...")]` message that reads as a
  sentence.

### When a declarative macro (`macro_rules!`)

Last resort. Use only when:

1. The pattern is used at least 5 times, and
2. Extracting a helper function / trait / generic can't capture the
   pattern because of lifetime or identifier-binding constraints.

Our codebase has no declarative macros today. Keep it that way unless
you have a clear, documented reason.

### When a procedural macro

Never write one for this project. If you think you need one, you're
probably reaching for code generation that belongs in a `build.rs`
or, better, in the schema module
([`src/schema.rs`](../../../src/schema.rs)) where shared Arrow and
tantivy field definitions live.

`#[pymodule]`, `#[pyfunction]`, and `#[pyclass]` from `pyo3` are
proc-macros we use as consumers — that's fine.

---

## Unsafe is last resort

See the dedicated [`unsafe.md`](unsafe.md) for the full rules. The
short version for this file: every current and near-term feature of
this crate can be done in safe Rust, and the crates we depend on
(tantivy, gix, roaring, fst, arrow, parquet, mail-parser,
regex-automata) are safe at their public APIs.

If you find yourself writing `unsafe`, stop and re-read
[`unsafe.md`](unsafe.md) before continuing.

---

## Idioms we prefer

### Return `Result<T, Error>` from library functions

`error::Error` (see [`src/error.rs`](../../../src/error.rs)) is the
only error type library code returns. Binary targets
([`src/bin/reindex.rs`](../../../src/bin/reindex.rs)) use
`anyhow::Result` because the error is formatted for a terminal, not
matched on.

### Iterators over index-driven loops

```rust
// Prefer
let out: Vec<_> = hits.iter().filter(|h| h.score > 0.0).collect();

// Over
let mut out = Vec::new();
for i in 0..hits.len() {
    if hits[i].score > 0.0 { out.push(hits[i].clone()); }
}
```

Rayon's `par_iter` is the parallel counterpart. Ingestion uses
`shards.par_iter().for_each(...)` — one rayon task per shard, never
within a shard.

### `&str` over `String` in arguments

Accept the narrower type. Return owned `String` only when you
actually produce a new allocation. Use `Cow<'_, str>` when the
function sometimes borrows, sometimes owns (normalized subject lines
are a classic case).

### `bytes::Bytes` for zero-copy slicing

The compressed store's decompressed output and mail-parser's body
slices are handed around as `Bytes` so that tier routers can share
views without re-allocating. See
[`src/store.rs`](../../../src/store.rs).

### `Arc<T>` only when truly shared across threads

Dynamic sharing with runtime-determined ownership — yes. "I want to
avoid thinking about lifetimes" — no. The decision tree in
[`index.md`](index.md#arct-or-t) is authoritative.

### Explicit generation-counter checks, not `Arc<Mutex<Reader>>`

The tantivy reader reload discipline (see
[`src/bm25.rs`](../../../src/bm25.rs) doc comment) uses
`ReloadPolicy::Manual` + a generation file. We do NOT wrap the reader
in a `Mutex` to coordinate reloads — that serializes queries.

---

## Idioms we reject

- `.unwrap()` / `.expect()` in library code. Tests may use
  `.unwrap()`; ingestion binary may `.expect("...")` with a
  diagnostic. Library code returns `Err`.
- `.clone()` used to paper over a borrow-checker error without
  understanding the ownership model. Fix the root cause.
- `Box<dyn Error>` in our `Error` type. `thiserror` + concrete
  variants.
- "Just use a `HashMap`." Unordered, non-deterministic iteration
  leaks into test output. Use `BTreeMap` when iteration order matters
  (tokenizer fingerprints, for one).
- `String::new()` followed by `push_str` in a loop. Use
  `String::with_capacity(n)` when `n` is known, or `itertools::join`
  — not in deps today; add it if a second caller shows up.

---

## Language feature decision tree

```
Need early-exit with name binding?
  -> let-else

Need composite condition with binding?
  -> if let ... && ...  (let chains)

Writing a trait that's genuinely async-shaped?
  -> async fn in traits  (but first: does this belong in Rust at all?)

Returning an iterator from a method?
  -> -> impl Iterator<Item = T>  (preferred over boxed dyn)

Tempted to write a proc-macro?
  -> Stop. Put it in schema.rs or build.rs instead.

Tempted to write unsafe?
  -> Stop. Re-read unsafe.md.
```

---

## Cross-references

- [`index.md`](index.md) — standards index.
- [`cargo.md`](cargo.md) — Cargo.toml and feature-flag discipline.
- [`unsafe.md`](unsafe.md) — when (rarely) and how.
- [`design/errors.md`](design/errors.md) — thiserror in library,
  anyhow in binaries, `From<Error> for PyErr` at the boundary.
- [`../python/language.md`](../python/language.md) — Python
  counterpart (3.13/3.14 features, deferred annotations).
