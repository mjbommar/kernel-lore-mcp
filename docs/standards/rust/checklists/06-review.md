# Checklist: Self-Review (Rust)

Rust counterpart to [`../../python/checklists/06-review.md`](../../python/checklists/06-review.md).

Re-read your own diff before committing. Catch what `clippy` cannot.

---

## Non-negotiables

- [ ] Every `unsafe` block has a `SAFETY:` comment describing each upheld invariant.
- [ ] Every panic-in-release (`.unwrap()`, `.expect()`, `panic!`, array indexing that can fail) is intentional and documented.
- [ ] `Send + Sync` verified for every type that crosses a thread.
- [ ] No `anyhow::Error` returned from a library function.
- [ ] `impl From<Error> for PyErr` covers every variant that can cross the PyO3 boundary.

> Source: [`../unsafe.md`](../unsafe.md), [`../design/errors.md`](../design/errors.md), [`../design/concurrency.md`](../design/concurrency.md).

---

## Self-review steps

### Read the diff

- [ ] **`git diff --staged`** — read every line. Not skim, read.

- [ ] **Remove debug artifacts.** Search the diff:
  ```bash
  git diff --staged | grep -E 'dbg!|println!|eprintln!|TODO:|XXX|FIXME'
  ```
  Zero hits (or each is justified in a comment).

- [ ] **No commented-out code.** Delete it; git remembers.

### Unsafe review

- [ ] **Every `unsafe { ... }` has `// SAFETY: ...`** immediately before or inside. Explain: which function/invariant makes this safe? What would break it?

- [ ] **`unsafe fn` has `# Safety` rustdoc.** Document the caller's obligations.

- [ ] **Justify the unsafe.** A safe alternative (bytemuck, NonNull, indexed access) is preferred. If you chose unsafe for performance, the criterion delta is in the commit body.

### Panic audit

- [ ] **`.unwrap()` / `.expect()` only where the invariant is locally proven.** Grep the diff:
  ```bash
  git diff --staged | grep -E '\.unwrap\(\)|\.expect\('
  ```
  Each call site is justified in a comment or self-evident.

- [ ] **`.unwrap()` in test code is fine.** Tests are expected to panic on unexpected conditions.

- [ ] **`slice[i]` / `vec[i]` indexing** — is the bound checked? If not, use `.get(i)?` or document the invariant.

- [ ] **`panic!("unreachable")`** — prefer `unreachable!("because X")` with a reason.

### Send/Sync audit

- [ ] **Any type passed to `rayon::scope` / `std::thread::spawn` / `Arc::new`** is `Send + Sync`. Verify with:
  ```rust
  fn assert_send_sync<T: Send + Sync>() {}
  assert_send_sync::<MyType>();
  ```

- [ ] **Interior mutability (`RefCell`, `Cell`) does NOT cross threads.** If it must, it's `Mutex` / `RwLock`.

- [ ] **Tantivy's `IndexWriter` is single-threaded.** If your diff adds a second writer, stop.

### Error handling

- [ ] **Every function returns `Result<T, crate::Error>` or a module-local typed error.** No `anyhow::Result` in library code (only `src/bin/*`).

- [ ] **Every `Error::Variant` that can cross PyO3 has a `From<Error> for PyErr` arm.** Read `src/error.rs`; verify.

- [ ] **Error messages actionable.** User-facing errors describe what went wrong AND suggest a fix (e.g., "regex requires DFA syntax; rewrite without backrefs").

### PyO3 boundary

- [ ] **Every `#[pyfunction]` / `#[pymethod]` has a Python-side test.**

- [ ] **GIL release on heavy calls.** Any call > 1 ms wrapped in `py.detach(|| ...)`. Never `allow_threads` — that's the renamed-from name.

- [ ] **Input/output types are cheap.** `&str` not `String`. `Vec<u8>` not `PyBytes` clones.

- [ ] **`.pyi` stub updated.** Every new pyfunction has a stub entry in `src/kernel_lore_mcp/_core.pyi`.

### Three-tier invariants

- [ ] **Positions OFF on BM25 prose fields.** Search diff for `WithFreqsAndPositions` on prose — reject.

- [ ] **Trigram candidates are confirmed with real regex.** Don't return trigram hits directly; post-confirm via `regex-automata` DFA.

- [ ] **Compressed store is the source of truth.** If the diff introduces state that can't be rebuilt from the store, reconsider.

- [ ] **Writer lockfile held.** Only `klmcp-ingest` opens the tantivy writer / trigram builder / store appender.

### Naming

- [ ] **Same concept, same name.** `message_id` everywhere; not `mid` in some modules and `msg_id` in others.
- [ ] **Rust conventions:** `snake_case` functions, `UpperCamelCase` types, `SCREAMING_SNAKE_CASE` consts.
- [ ] **Units in names.** `timeout_ms`, `budget_bytes`, `cap_rows`.

### Dependencies

- [ ] **No new `Cargo.toml` entry without a reason.** Every new dep is justified in the commit body. Ask: can we do this in stdlib in ~100 lines?

- [ ] **No crate version bump mixed with feature work.** Dep bumps are separate commits (TODO.md rule).

### Atomicity

- [ ] **One logical change per commit.** If the diff description contains "and", split.

### Read like a stranger

- [ ] **Open the file without context.** Does the naming, structure, and flow make sense cold? If not, refactor now.

---

## Cross-references

- [`../unsafe.md`](../unsafe.md)
- [`../design/errors.md`](../design/errors.md)
- [`../design/concurrency.md`](../design/concurrency.md)
- [`../ffi.md`](../ffi.md)
- [`../../python/checklists/06-review.md`](../../python/checklists/06-review.md)
