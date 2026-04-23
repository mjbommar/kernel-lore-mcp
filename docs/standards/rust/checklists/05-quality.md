# Checklist: Code Quality (Rust)

Rust counterpart to [`../../python/checklists/05-quality.md`](../../python/checklists/05-quality.md).

Every change passes the full Rust QA pipeline. No shortcuts, no `#[allow(clippy::...)]` without justification.

---

## Non-negotiables

- [ ] `cargo fmt --check` — zero diff.
- [ ] `cargo clippy --all-targets -- -D warnings` — zero warnings.
- [ ] `cargo test` — all tests pass.
- [ ] `cargo doc --no-deps` — no warnings (broken rustdoc links fail here).
- [ ] MSRV: `cargo +1.88 build` — builds on the pinned toolchain.

> Source: [`../code-quality.md`](../code-quality.md), [`../index.md`](../index.md).

---

## QA pipeline

Run these in order. Fix each before moving on.

### 1. Formatting

- [ ] **`cargo fmt --check`** — exits 0 if everything is formatted.
  ```bash
  cargo fmt --check
  ```
  Fix with `cargo fmt`. No manual formatting disputes; `rustfmt.toml` is authoritative.

### 2. Linting

- [ ] **`cargo clippy --all-targets -- -D warnings`** — promote every warning to an error.
  ```bash
  cargo clippy --all-targets --all-features -- -D warnings
  ```

- [ ] **Every `#[allow(clippy::...)]` has a comment** explaining why. Unexplained allows get deleted in review.

- [ ] **No `#[allow(dead_code)]` without justification.** If the code isn't called, delete it. If it's feature-gated, gate it.

### 3. Tests

- [ ] **`cargo test`** — all Rust tests pass.
  ```bash
  cargo test --all-features
  cargo test --no-default-features   # verify free-threaded build path compiles + tests
  ```

- [ ] **Doc tests** run as part of `cargo test`. Broken examples fail here.

### 4. Docs

- [ ] **`cargo doc --no-deps`** — no broken rustdoc links, no missing docs on `pub` items.
  ```bash
  cargo doc --no-deps --all-features
  ```

- [ ] **Deny missing docs on the PyO3 surface.** Add `#![deny(missing_docs)]` at the top of modules that form the public extension surface.

### 5. MSRV check

- [ ] **`cargo +1.88 build --all-targets`** — the pinned toolchain must compile everything.
  ```bash
  cargo +1.88 build --all-targets --all-features
  ```
  If a new API requires a newer compiler, the toolchain bump is a separate, justified commit.

### 6. Cross-layer check

- [ ] **`maturin develop` + Python tests.**
  ```bash
  uv run maturin develop --release
  uv run pytest tests/python -v
  ```

### 7. Optional (run when dependencies change)

- [ ] **`cargo-deny`** — license compliance, duplicate deps, yanked crates.
  ```bash
  cargo deny check
  ```

- [ ] **`cargo-audit`** — known vulnerabilities.
  ```bash
  cargo audit
  ```

- [ ] **`cargo tree -d`** — duplicate dep versions. Critical when the Arrow/Parquet/zstd chain is touched (TODO.md Phase 1).

- [ ] **`cargo-outdated`** — awareness only; do not upgrade pinned deps without a commit explaining why.

---

## Code-quality rules (caught by humans, not tools)

- [ ] **No `.unwrap()` in production paths.** `.expect("local invariant")` only where the invariant is proven locally.

- [ ] **No `panic!` in library code** unless the caller has violated a typed invariant. Prefer `Result::Err`.

- [ ] **No `println!` / `eprintln!`.** Use `tracing::info!` / `tracing::error!`.

- [ ] **No `std::process::exit`.** Return an error; let the binary's `main` decide.

- [ ] **No `std::env::var` in library code.** Configuration is passed as arguments. Env access is confined to the binary entry point.

- [ ] **No raw `*const T` / `*mut T`** unless inside `unsafe` with a `SAFETY:` comment.

- [ ] **No `Box<dyn Trait>` on the ingest hot path.** Static dispatch is free.

- [ ] **Positions OFF on BM25 fields.** `IndexRecordOption::WithFreqs`. If you see `WithFreqsAndPositions` on a prose field, reject the change.

---

## Common clippy lints to honour

- [ ] `clippy::needless_clone` — fix, don't allow.
- [ ] `clippy::redundant_clone` — fix, don't allow.
- [ ] `clippy::single_match_else` — rewrite as `if let`.
- [ ] `clippy::large_enum_variant` — box the large variant.
- [ ] `clippy::mutex_atomic` — use `AtomicBool` / `AtomicU64` when a mutex wraps a primitive.
- [ ] `clippy::await_holding_lock` — n/a on Rust side (no tokio), but flag if it ever appears.

---

## Cross-references

- [`../code-quality.md`](../code-quality.md)
- [`../cargo.md`](../cargo.md)
- [`../language.md`](../language.md) — MSRV + edition notes
- [`../../python/checklists/05-quality.md`](../../python/checklists/05-quality.md)
