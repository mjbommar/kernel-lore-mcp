# Checklist: Design and Architecture (Rust)

Rust counterpart to [`../../python/checklists/02-design.md`](../../python/checklists/02-design.md).

Sketch types BEFORE coding. Decide where the code lives, what the errors look like, and who owns what. A bad design caught here costs minutes; the same mistake in `src/ingest.rs` costs hours.

---

## Non-negotiables

- [ ] Types defined before any `fn` body.
- [ ] Error variants modeled via `thiserror` with `#[source]` where a cause exists.
- [ ] Library code returns `Result<T, crate::Error>` (or a local `thiserror` enum). Never `anyhow::Result`.
- [ ] Every error variant that can escape to Python has a planned `impl From<_> for PyErr`.
- [ ] New code lives in the correct layer: pure-Rust core / PyO3 glue / binary.

> Source: [`../design/errors.md`](../design/errors.md), [`../design/boundaries.md`](../design/boundaries.md), [`../index.md`](../index.md).

---

## Design steps

### Where does it live?

- [ ] **Pure-Rust core?** Lives in one of `src/{store,schema,state,metadata,trigram,bm25,ingest,router}.rs`. No PyO3 types in the signature. Unit-testable via `cargo test` alone.

- [ ] **PyO3 glue?** Lives in `src/lib.rs` (or a `src/py_*.rs` submodule wired into `#[pymodule]`). `#[pyfunction]` / `#[pyclass]` goes here. Glue is thin: translate Python -> Rust types, call core, translate result -> Python.

- [ ] **Binary?** Lives in `src/bin/<name>.rs`. `reindex` is the canonical example. `anyhow::Result<()>` in `main` is fine; everywhere else stays typed.

> Ref: [`../design/boundaries.md`](../design/boundaries.md).

### Types first

- [ ] **Sketch the data types.** Write the `struct` / `enum` definitions before any method. Every field typed; no `String` where a `smartstring::SmallString` or enum would encode the invariant better.

- [ ] **Choose ownership carefully.** `&T` with lifetime when one owner is clear. `Arc<T>` when shared across threads with dynamic lifetime. `Cow<'a, str>` when sometimes-borrowed-sometimes-owned. `Box<T>` when sized-on-heap. See [`../design/data-structures.md`](../design/data-structures.md).

- [ ] **Mark `#[derive(Debug, Clone)]` intentionally.** `Debug` always. `Clone` only when needed (wake up if you derive `Clone` on a 1 KB struct used in a hot loop).

- [ ] **Mark `#[non_exhaustive]` on public enums that may grow.** Future-proofs downstream `match`es.

- [ ] **Decide on `Send + Sync` early.** If the type crosses a `rayon` task, it must be `Send + Sync`. Interior mutability (`Mutex`, `RwLock`) is a smell in the ingest hot path — prefer message passing or per-thread builders merged at the end.

### Traits and bounds

- [ ] **Default to free functions.** Traits are for abstractions you will have 2+ implementations of. One implementation = a function.

- [ ] **Keep trait bounds minimal.** `T: AsRef<str>` beats `T: Into<String>` if you just need a read. `impl Iterator<Item = X>` in signatures when you don't want to commit to a concrete type.

- [ ] **Avoid `Box<dyn Trait>` on the hot path.** Static dispatch via generics is free; dynamic dispatch costs an indirect call per invocation.

### Errors

- [ ] **Define the error enum with `thiserror`.**
  ```rust
  #[derive(Debug, thiserror::Error)]
  pub enum Error {
      #[error("index generation file missing at {path}")]
      MissingGeneration { path: std::path::PathBuf },
      #[error("tantivy: {0}")]
      Tantivy(#[from] tantivy::TantivyError),
      #[error("io: {0}")]
      Io(#[from] std::io::Error),
  }
  ```
- [ ] **Include `#[source]` / `#[from]`** so the error chain survives to the top.

- [ ] **Plan the `From<Error> for PyErr` mapping.** Which variants become `ValueError`? Which become `RuntimeError`? Which map to a domain-specific `PyErr` subclass? See [`../design/errors.md`](../design/errors.md).

- [ ] **Never use `anyhow::Error` in a library function signature.** It erases type info. Binaries only.

### PyO3 signatures

- [ ] **Input types: `PyBytes`, `&str`, `i64`, `Vec<T>` — cheap to convert.** Accept `&str` not `String` unless you need ownership.

- [ ] **Output types: pydantic-compatible primitives or pyclasses.** Keep the glue thin; pydantic on the Python side handles validation.

- [ ] **Release the GIL** with `py.detach(|| { /* pure rust */ })` for any call taking >1 ms. Never hold the GIL across a tier dispatch.

- [ ] **`#[pyfunction]` vs `#[pymethods]`.** Prefer free `#[pyfunction]`s wired into the module. `#[pyclass]` only when you need shared state across calls (e.g., an opened index handle).

### Concurrency

- [ ] **rayon for CPU-parallel ingest.** One task per lore shard; never within a shard (packfile cache locality — from CLAUDE.md).

- [ ] **No tokio on the Rust side.** If you're designing async in Rust, stop. Push the async to the Python layer; expose a sync Rust function that releases the GIL.

- [ ] **Single-writer enforcement.** Any type that wraps a tantivy writer / trigram builder / store appender must either panic or error if constructed in the MCP serve process. Enforce via a feature flag or a runtime check.

---

## Cross-references

- [`../design/boundaries.md`](../design/boundaries.md)
- [`../design/errors.md`](../design/errors.md)
- [`../design/modules.md`](../design/modules.md)
- [`../design/data-structures.md`](../design/data-structures.md)
- [`../design/concurrency.md`](../design/concurrency.md)
- [`../ffi.md`](../ffi.md) — PyO3 boundary rules
- [`../../python/checklists/02-design.md`](../../python/checklists/02-design.md) — Python counterpart
