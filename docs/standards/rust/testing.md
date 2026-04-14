# Testing, Property Tests, Benchmarks

Rust counterpart to [`../python/testing.md`](../python/testing.md).

Three feedback loops mirror the Python side: **unit tests** prove
the code works on small inputs, **property tests** prove it works on
adversarial ones, **benchmarks** prove it's fast enough. All three
run against the live tantivy / gix / arrow code paths — we do not
mock the stack.

---

## The test tree

```
kernel-lore-mcp/
├── src/
│   ├── router.rs           # unit tests in a #[cfg(test)] mod tests { ... }
│   ├── trigram.rs          # same
│   └── ...
├── tests/
│   ├── ingest_smoke.rs     # integration: drive ingest over synthetic shard
│   ├── router_grammar.rs   # integration: parse + dispatch lei queries
│   └── python/             # pytest suite (Python-side tests, FFI covered here)
├── benches/
│   ├── router.rs           # criterion benches (when added)
│   └── trigram_confirm.rs
└── Cargo.toml
```

Rust has two test locations; they mean different things.

### Unit tests — same file as the code

Keep them next to the code they test. One `#[cfg(test)] mod tests`
block per file:

```rust
// src/router.rs
pub fn parse_rt(token: &str) -> Result<TimeRange, Error> { ... }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rt_accepts_relative_years() {
        let r = parse_rt("rt:5y").unwrap();
        assert_eq!(r.years(), 5);
    }

    #[test]
    fn rt_rejects_unknown_unit() {
        assert!(matches!(parse_rt("rt:5z"), Err(Error::QueryParse(_))));
    }
}
```

Unit tests have full access to `pub(crate)` and module-private items.
Use them for everything below the module boundary — the parser, the
tokenizer-fragment emitter, trigram bitmap intersection, cursor
HMAC.

### Integration tests — `tests/` at the crate root

Each file is its own crate. Imports go through the public API
(`use kernel_lore_mcp::...`) — this is what catches accidental
private-to-public leaks.

```rust
// tests/router_grammar.rs
use kernel_lore_mcp::router::parse_query;

#[test]
fn phrase_on_prose_is_rejected() {
    let err = parse_query("\"exact phrase\"").unwrap_err();
    // body_prose is positionless in v1; no silent degradation.
    assert!(matches!(err, _));
}
```

Integration tests drive whole pipelines: spin up a tempfile
`tantivy::Index`, run a synthetic shard through ingest, query via
the router. Use `tempfile::TempDir` (already a dev-dep) for scratch
paths.

---

## Running tests

```bash
# Run everything
cargo test --locked

# One test by filter
cargo test --locked rt_rejects_unknown_unit

# Only one file in tests/
cargo test --locked --test router_grammar

# Only unit tests in one module
cargo test --lib router::tests

# Include #[ignore]'d slow tests
cargo test --locked -- --ignored

# Show println! / dbg! output
cargo test --locked -- --nocapture
```

### The `#[ignore]` tier

Tests that touch real lore corpora, run minutes, or need a specific
environment variable get `#[ignore]`:

```rust
#[test]
#[ignore = "requires KLMCP_LORE_CORPUS pointing at a grokmirror checkout"]
fn end_to_end_linux_kernel_shard() { ... }
```

Run them with `cargo test -- --ignored`. Never make them the default
suite.

---

## proptest — property-based tests

Already in `[dev-dependencies]`. Use it for invariants, not example
inputs.

Good candidates in this codebase:

- **Tokenizer round-trips** — for any byte-ascii identifier
  `[A-Za-z_][A-Za-z_0-9]*`, the tokenizer emits the full identifier
  as one token AND the expected subtokens.
- **Byte-to-char offset tables** — for any UTF-8 string, the table
  built in the PyO3 glue agrees with `text.char_indices()` at every
  byte boundary.
- **Cursor HMAC** — any cursor serialized by us is accepted by us;
  any bit-flip is rejected.
- **Trigram generation** — every 3-byte window of the input
  contributes to the posting set; union-of-candidates is a superset
  of the ground-truth substring matches.

Pattern:

```rust
// src/router.rs (inside #[cfg(test)] mod tests)
use proptest::prelude::*;

proptest! {
    #[test]
    fn cursor_roundtrip(mid in "[a-z0-9]{1,64}", score in -1e9f32..1e9f32) {
        let c = Cursor::new(mid.clone(), score).sign(TEST_KEY);
        let back = Cursor::verify(&c, TEST_KEY).unwrap();
        prop_assert_eq!(back.mid, mid);
        prop_assert!((back.score - score).abs() < 1e-6);
    }
}
```

Rules:

- **Shrinking works.** Don't disable shrinking to speed up failures;
  a failure that shrunk to a small input is the whole point.
- **Seeds matter.** proptest persists failing seeds under
  `proptest-regressions/`. Commit that directory — it's how future
  CI catches the same bug.
- **Use `prop_assert!` / `prop_assert_eq!`** inside `proptest!`
  blocks so failures include the shrunk input.

---

## criterion — benchmarks

Already in `[dev-dependencies]` with `html_reports`. Use benchmarks
to prove a performance claim in a commit message, not as a general
"how fast is this" exercise.

### Cargo wiring

```toml
[[bench]]
name = "router"
harness = false    # criterion provides its own main
```

Drop the file at `benches/router.rs`. Then:

```bash
# Run all benches
cargo bench

# One bench
cargo bench --bench router

# Compare against a saved baseline
cargo bench -- --save-baseline main
# ... change code ...
cargo bench -- --baseline main
```

### Bench structure

```rust
// benches/router.rs
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use kernel_lore_mcp::router::parse_query;

fn bench_parse(c: &mut Criterion) {
    c.bench_function("parse_query/simple", |b| {
        b.iter(|| parse_query(black_box("s:patch list:linux-kernel rt:1y")))
    });
}

criterion_group!(benches, bench_parse);
criterion_main!(benches);
```

### What to benchmark

- **Router parse + dispatch.** Target: sub-10 us for realistic
  queries on a hot box.
- **Trigram confirmation.** Target: p95 under 500 ms on the reference
  `r7g.xlarge` (see `TRIGRAM_CONFIRM_LIMIT` in
  [`src/trigram.rs`](../../../src/trigram.rs)).
- **PyO3 FFI cost.** A Rust-native call vs. a Python-callable
  pyfunction — confirm FFI overhead is below the work done per call.
- **Ingest per-message cost** — `mail-parser` + trailer extraction.
  Target sets the box sizing.

### Bench discipline

- Pin benches to fixtures. Never to live lore corpora — the numbers
  drift with data.
- Use `black_box()` around inputs that the optimizer could constant-
  fold.
- When a commit improves perf, paste the before/after into the
  commit body. Example:
  ```
  perf(router): dedup trigram set before bitmap intersect

  Before: 187 us/query (criterion, p50 over 100 iters)
  After:   94 us/query
  Speedup: 2.0x
  ```

---

## insta — snapshot tests (recommend adding)

Not in `[dev-dependencies]` today. Add it the first time you need to
assert against a complex structured output — for example, the parsed
AST of a lei query, or the normalized subject after tag extraction.

```bash
cargo add --dev insta
```

Usage:

```rust
#[test]
fn parses_full_query() {
    let ast = parse_query(
        "s:patch reviewed-by:torvalds rt:1y list:linux-kernel",
    ).unwrap();
    insta::assert_debug_snapshot!(ast);
}
```

Review snapshots with `cargo insta review`. Commit the generated
`.snap` files.

Don't use insta for:

- Simple equality asserts (`assert_eq!` reads better).
- Floating-point numerics (snapshots are textual — fractional
  rounding creates false diffs).
- Anything that might order-depend on `HashMap` iteration. Use
  `BTreeMap` first; see [`language.md`](language.md).

---

## Testing the PyO3 glue — from the Python side

FFI glue is tested from Python, not from `cargo test`. The boundary
code (`#[pyfunction]`, `#[pyclass]`, type conversions,
`From<Error> for PyErr`) lives to be called from Python — test it
there.

See [`../python/testing.md`](../python/testing.md) and
[`ffi.md`](ffi.md). Quick shape:

```python
# tests/python/test_core_version.py
from kernel_lore_mcp import _core

def test_version_matches_wheel():
    assert _core.version() == "0.1.0"
```

```python
# tests/python/test_router_errors.py — error conversion
import pytest
from kernel_lore_mcp import _core

def test_phrase_on_prose_rejected():
    with pytest.raises(ValueError, match="phrase queries not supported"):
        _core.parse_query('"exact phrase"')
```

Rules:

- **Every `impl From<X> for PyErr`** mapping gets at least one
  pytest case that triggers it and asserts the Python exception
  type.
- **Every pyfunction** that crosses the GIL via `Python::detach` gets
  a pytest that actually exercises it under a threaded invocation —
  proves the detach happened, or at least that we didn't deadlock.
- **FFI round-trips with non-ASCII text** (CJK, emoji) go in pytest.
  See [`ffi.md`](ffi.md) on byte-to-char offset conversion.

---

## The development loop

1. **Inspect first.** `cargo doc --open`, `cargo expand` (for
   proc-macro output), `rg` through the source. Don't guess a
   tantivy or gix API — read it.
2. **Write the failing test.** Either a `#[cfg(test)]` unit test, an
   integration test under `tests/`, a `proptest!` block, or a
   pytest.
3. **Implement** the minimum code to make it green.
4. **QA.** `cargo fmt` -> `cargo clippy` -> `cargo test` -> `cargo
   doc`. See [`code-quality.md`](code-quality.md).
5. **Bench if perf matters.** Record before/after.
6. **Commit atomically.**

Never skip step 1. Wrong guesses about tantivy's `IndexRecordOption`,
gix's `rev_walk` semantics, or PyO3's `Bound<'py, T>` lifetime rules
cause subtle bugs that pass mocked tests.

---

## Anti-patterns

- **Testing against mocks.** If you can't drive `tantivy`, `gix`,
  `arrow`, or `mail-parser` directly in the test, the test doesn't
  prove anything.
- **Silent `unwrap`** inside tests that hide the real error. Tests
  may `unwrap()`, but on failure the message should still point at
  the cause.
- **Time-dependent assertions.** The ingest pipeline computes dates
  from RFC822 headers — use fixed fixtures, not `chrono::Utc::now()`.
- **Test files that create real git shards over the network.** Use
  synthetic `tempfile::TempDir` + `gix::init` + hand-crafted
  commits. `scripts/` has helper fixtures for this.
- **Ordering assertions against `HashMap` iteration.** Use
  `BTreeMap`.
- **Benchmarks that measure cold cache.** Warm up first with a
  throw-away iter; criterion does this by default.

---

## Cross-references

- [`index.md`](index.md) — standards index.
- [`code-quality.md`](code-quality.md) — QA pipeline.
- [`ffi.md`](ffi.md) — where FFI tests belong and why.
- [`../python/testing.md`](../python/testing.md) — pytest
  conventions, `fastmcp.Client` integration tests.
- [`../python/pyo3-maturin.md`](../python/pyo3-maturin.md) — the
  three-layer architecture and where tests sit on each layer.
