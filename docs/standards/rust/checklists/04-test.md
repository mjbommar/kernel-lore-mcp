# Checklist: Testing (Rust)

Rust counterpart to [`../../python/checklists/04-test.md`](../../python/checklists/04-test.md).

`cargo test` for pure Rust. The Python test suite covers PyO3 glue (it exercises the built wheel end-to-end, which is the only way to verify the boundary works). `proptest` for invariants, `criterion` for perf regressions, `insta` for snapshots.

---

## Non-negotiables

- [ ] `cargo test` green before any commit.
- [ ] Python-side tests cover every `#[pyfunction]` / `#[pymethods]` surface.
- [ ] `criterion` before/after numbers committed for any change to a hot path.
- [ ] Tests use synthetic fixtures from `tests/python/fixtures/`. No live lore fetches from test machines.

> Source: [`../testing.md`](../testing.md), [`../../../CLAUDE.md`](../../../CLAUDE.md).

---

## Testing steps

### Unit tests (pure Rust)

- [ ] **Inline `#[cfg(test)] mod tests`** at the bottom of each `.rs` file for module-private tests.

- [ ] **Integration tests** in `tests/*.rs` (Rust-level, not Python) when testing the public crate API — rare; most crate API is the PyO3 surface.

- [ ] **Run:**
  ```bash
  cargo test
  cargo test --release   # before benchmarks; before any perf claim
  ```

- [ ] **Name tests for behaviour.** `parses_patch_trailer_with_multiple_reviewed_by` beats `test_trailer_2`.

- [ ] **Assert content, not just "didn't panic".**
  ```rust
  assert_eq!(parsed.reviewed_by.len(), 3);
  assert_eq!(parsed.reviewed_by[0], "alice@example.org");
  ```
  Not `assert!(!parsed.reviewed_by.is_empty())`.

### Property tests (`proptest`)

- [ ] **Use `proptest` for invariants.** Tokenizer: `tokenize(s) -> tokens -> detokenize` should never panic on arbitrary UTF-8. Trigram posting-list merge should be associative and commutative.

- [ ] **Shrink-friendly generators.** Prefer `any::<String>()` over hand-rolled random; let proptest shrink counterexamples.

- [ ] **Example:**
  ```rust
  proptest! {
      #[test]
      fn roaring_merge_is_commutative(a: Vec<u32>, b: Vec<u32>) {
          let ab = merge_postings(&a, &b);
          let ba = merge_postings(&b, &a);
          prop_assert_eq!(ab, ba);
      }
  }
  ```

### Snapshot tests (`insta`)

- [ ] **Use `insta` for shape-heavy outputs.** Tokenizer output, query plans, router merge results.

- [ ] **Review snapshots before committing.** `cargo insta review`. Do not blindly accept — a snapshot change is a behaviour change.

- [ ] **Snapshot files live in `src/snapshots/`.** Commit them.

### Benchmarks (`criterion`)

- [ ] **Benches live in `benches/*.rs`.** Wire in `Cargo.toml`:
  ```toml
  [[bench]]
  name = "router"
  harness = false
  ```

- [ ] **Run:**
  ```bash
  cargo bench
  cargo bench -- --save-baseline before
  # ... make change ...
  cargo bench -- --baseline before
  ```

- [ ] **Commit the criterion report summary.** The perf commit body includes:
  ```
  router::dispatch — 1M-row metadata tier
  before: 4.21 ms/query
  after:  0.63 ms/query
  speedup: 6.7x
  ```

- [ ] **Do not claim a speedup without a committed baseline.**

### PyO3 glue tests (Python side)

- [ ] **Build wheel:** `uv run maturin develop --release`.

- [ ] **Run Python tests:** `uv run pytest tests/python -v`.

- [ ] **Verify the GIL-release path.** A test that spawns N threads calling a `Python::detach` function and asserts they run concurrently (timing-based; use a wide tolerance).

- [ ] **Verify error mapping.** Every `thiserror` variant exercised from Python asserts the expected `PyErr` subclass.

### Fixtures

- [ ] **Use `tests/python/fixtures/`** for mbox samples, synthetic patches, expected tokenizer outputs. Real lore fetches are banned from CI — see CLAUDE.md.

- [ ] **Hand-craft the minimum needed.** A 5-line mbox beats a 500-line one if both exercise the bug.

### Coverage checks

- [ ] **`cargo tarpaulin` or `cargo llvm-cov`** for line coverage when helpful. Don't chase a number; chase uncovered branches that matter.

- [ ] **Error paths tested.** For every `Error::*` variant, a test triggers it and asserts the variant (not just "an error occurred").

### Regression tests

- [ ] **Every bug fix adds a regression test.** The test fails before the fix and passes after. Commit both together.

- [ ] **Snapshot tests are regression tests.** Accept the diff only after verifying the new shape is correct.

---

## Cross-references

- [`../testing.md`](../testing.md)
- [`../ffi.md`](../ffi.md)
- [`../../python/testing.md`](../../python/testing.md)
- [`../../python/checklists/04-test.md`](../../python/checklists/04-test.md)
