# Checklist: Implementation (Rust)

Rust counterpart to [`../../python/checklists/03-implement.md`](../../python/checklists/03-implement.md).

Write code that is small, typed tightly, and easy to delete. Check the pipeline after every logical change.

---

## Non-negotiables

- [ ] Prefer `pub(crate)` over `pub`. Only items intended for the PyO3 surface or bins are `pub`.
- [ ] `#[must_use]` on every fallible return (`Result`, `Option` where ignoring is a bug).
- [ ] `#[derive(Debug)]` on every struct / enum. Helps `tracing` and panic messages.
- [ ] `cargo fmt` + `cargo clippy --all-targets -- -D warnings` + `cargo test` after every logical change.
- [ ] No `unsafe` without a `SAFETY:` comment explaining the invariants upheld.

> Source: [`../index.md`](../index.md), [`../unsafe.md`](../unsafe.md).

---

## Implementation steps

### Module layout

- [ ] **Match the canonical tree.** New files go into the existing taxonomy: `store.rs`, `schema.rs`, `state.rs`, `metadata.rs`, `trigram.rs`, `bm25.rs`, `ingest.rs`, `router.rs`, `error.rs`. Add a new file only when an existing module doesn't fit.

- [ ] **`mod` declarations live in `lib.rs`.** Re-exports (`pub use`) minimal and curated. The `#[pymodule]` function in `lib.rs` is the Python-facing surface.

- [ ] **Use `pub(crate)` by default.** Promote to `pub` only when the item crosses the PyO3 boundary or the `reindex` binary needs it.

- [ ] **Module-level `//!` comment.** One paragraph: what this module does, where it fits in the three-tier architecture, and what invariants it upholds.

### Types

- [ ] **Define types at the top of the file.** `struct` / `enum` / `type` aliases before any `impl`.

- [ ] **Derive purposefully.**
  - `Debug`: always.
  - `Clone`: only when cloning is needed and cheap, or when cloning is semantically an owned copy.
  - `Copy`: only on small POD (<= 16 bytes typically).
  - `Default`: when there's a genuine zero value.
  - `PartialEq, Eq, Hash`: when used as a map key or in a test assertion.
  - `serde::Serialize, Deserialize`: only when the type crosses a persistence boundary (Arrow schema derivations are separate).

- [ ] **`#[repr(C)]` or `#[repr(packed)]` only with justification.** Note the reason in a comment.

### Functions

- [ ] **Annotate with `#[must_use]`.**
  ```rust
  #[must_use]
  pub(crate) fn parse_subject_tags(raw: &str) -> Vec<SubjectTag> { ... }
  ```

- [ ] **Return `Result<T, crate::Error>` for fallible work.** Use `?` liberally. Let the caller decide logging.

- [ ] **Accept borrows, not owned.** `&str` not `String`; `&[T]` not `Vec<T>`, unless you need to consume.

- [ ] **Keep signatures typed.** No `impl Into<String>` where `&str` suffices. No `&dyn Trait` on the hot path.

- [ ] **No allocations in the inner loop.** Pre-allocate `Vec::with_capacity(n)`, reuse `String::clear()` across iterations, use `SmallVec` for small dynamic collections.

### Errors

- [ ] **Propagate with `?`; never `.unwrap()` in production paths.** `.expect("invariant: ...")` only where the invariant is proven locally.

- [ ] **Use `#[from]` for trivial conversions** (`std::io::Error`, `tantivy::TantivyError`). For contextual conversions (`path` included), implement `From` manually or use a builder.

- [ ] **Error messages describe the fix when possible.**
  ```rust
  #[error("regex requires DFA-compatible syntax; backrefs are rejected (query rewrite suggested: substitute literal)")]
  RegexComplexity,
  ```

### Concurrency

- [ ] **Use `rayon::iter::ParallelIterator`** for shard-level parallelism. Never parallelize *within* a shard — packfile cache locality matters.

- [ ] **Never spawn a tokio task.** If you need async, the Python caller wraps with `asyncio.to_thread`.

- [ ] **Release the GIL for any PyO3 call > 1 ms.**
  ```rust
  py.detach(|| core::dispatch(query))
  ```
  (PyO3 0.28.3: `detach` replaces `allow_threads`.)

### Unsafe

- [ ] **Every `unsafe` block has `// SAFETY: ...`** explaining why each invariant holds.
- [ ] **Prefer safe alternatives.** `bytemuck` / `zerocopy` over raw transmute. `std::ptr::NonNull` over raw pointers. See [`../unsafe.md`](../unsafe.md).

### Logging

- [ ] **Use `tracing`, not `log`, not `println!`.** Spans around tier boundaries (`ingest::shard`, `router::dispatch`).
- [ ] **Structured fields, not interpolated strings.** `tracing::info!(shard = %shard_id, "starting walk")` not `info!("starting walk on {shard_id}")`.
- [ ] **Python owns production logging.** Rust `tracing` is a dev/test aid; `tracing-subscriber` is a dev-dep only.

### Continuous checks

- [ ] **After every logical change, run:**
  ```bash
  cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
  ```
- [ ] **Before crossing the PyO3 boundary, run:**
  ```bash
  uv run maturin develop && uv run pytest tests/python -v
  ```

---

## Cross-references

- [`../design/modules.md`](../design/modules.md)
- [`../design/data-structures.md`](../design/data-structures.md)
- [`../design/errors.md`](../design/errors.md)
- [`../design/concurrency.md`](../design/concurrency.md)
- [`../ffi.md`](../ffi.md)
- [`../unsafe.md`](../unsafe.md)
- [`../../python/checklists/03-implement.md`](../../python/checklists/03-implement.md)
