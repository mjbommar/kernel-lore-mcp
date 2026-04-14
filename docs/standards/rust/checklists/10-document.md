# Checklist: Documentation (Rust)

Rust counterpart to [`../../python/checklists/10-document.md`](../../python/checklists/10-document.md).

Document decisions and interfaces, not obvious code. `cargo doc` is the public surface; `///` is how you speak to it.

---

## Non-negotiables

- [ ] `///` doc comment on every `pub` item.
- [ ] Code examples in `///` blocks compile via `cargo test --doc`.
- [ ] Module-level `//!` describes the module's role in the three-tier architecture.
- [ ] `cargo doc --no-deps` emits zero warnings.
- [ ] No auto-generated `.md` files. `CLAUDE.md` + `docs/standards/rust/` are the only doc surfaces.

> Source: [`../../../CLAUDE.md`](../../../CLAUDE.md), [`../index.md`](../index.md).

---

## Documentation steps

### Module-level docs

- [ ] **`//!` at the top of every `.rs` file.** One paragraph:
  ```rust
  //! Trigram tier: FST-indexed term dictionary + roaring posting lists over
  //! patch/diff content. Query path compiles a DFA via `regex-automata`,
  //! narrows candidates via trigram intersection, then confirms by
  //! decompressing the candidate body from the compressed store.
  //!
  //! See `docs/architecture/three-tier-index.md` for the tier contract.
  ```

- [ ] **Link to related modules** with `[`module_name`]` rustdoc links. Cross-link adjacent tiers.

### Public items

- [ ] **Every `pub` function / struct / enum / trait has a `///` summary.** One-line summary; blank line; details if needed.

- [ ] **Every public function documents its parameters** when not self-evident:
  ```rust
  /// Dispatch a parsed query across the three tiers and merge results.
  ///
  /// # Parameters
  /// - `query`: already-parsed `Query` (see [`Query::parse`]).
  /// - `budget`: wall-clock budget in milliseconds.
  ///
  /// # Errors
  /// Returns [`Error::RegexComplexity`] if the query contains a regex that
  /// cannot compile to DFA.
  pub fn dispatch(query: &Query, budget_ms: u64) -> Result<Hits> { ... }
  ```

- [ ] **Document `# Errors`** when the function returns `Result`.

- [ ] **Document `# Panics`** when the function panics on any input.

- [ ] **Document `# Safety`** on every `unsafe fn`. What must the caller uphold?

### Code examples

- [ ] **Include an example on every meaningful public item.**
  ```rust
  /// # Examples
  /// ```
  /// use kernel_lore_mcp::router::Query;
  /// let q = Query::parse("dfpost:CVE-2024-12345 AND rt:30d")?;
  /// # Ok::<(), Box<dyn std::error::Error>>(())
  /// ```
  ```

- [ ] **Doc-tests run via `cargo test --doc`.** Broken examples fail the build.

- [ ] **Hide boilerplate with `#`** prefix lines. Keep the example focused on the public API.

- [ ] **Use `no_run`** for examples that require a real index. They should still compile, just not run.

### Cross-linking

- [ ] **Rustdoc links** `[`Type`]`, `[`mod::Type`]`, `[`Type::method`]`. Makes navigation free.

- [ ] **Link to the relevant `docs/` guide** from the module-level comment when a non-trivial invariant lives there. Example: the tokenizer module links to `docs/indexing/tokenizer-spec.md`.

### Privacy and visibility

- [ ] **`pub(crate)` items may have docs** but don't have to. Use judgment — if the item is tricky, document it.

- [ ] **`#[doc(hidden)]`** on items that must be `pub` for macro reasons but aren't part of the public API.

### `.pyi` stubs

- [ ] **Every `#[pyfunction]` has a stub entry in `src/kernel_lore_mcp/_core.pyi`.**
  ```python
  def dispatch(query: str, budget_ms: int) -> list[Hit]: ...
  ```

- [ ] **Stubs include type hints** matching what the Rust function accepts/returns.

- [ ] **`py.typed` marker file** present in the Python package.

### `cargo doc` checks

- [ ] **Run during development:**
  ```bash
  cargo doc --open --no-deps
  ```

- [ ] **Pre-commit:**
  ```bash
  cargo doc --no-deps --all-features
  ```
  Zero warnings. Broken links fail here.

- [ ] **Deny missing docs** on the public PyO3 module:
  ```rust
  #![deny(missing_docs)]
  ```
  At the top of `lib.rs` (or the relevant submodule).

### Non-obvious decisions

- [ ] **Comment the WHY, never the WHAT.** `// using fxhash because stdlib's SipHash is a DoS-resistant overhead we don't need here` — good. `// compute the hash` — delete.

- [ ] **Link bug workarounds.** If a block of code exists because of a crate bug, link the upstream issue: `// workaround for tantivy#1234`.

- [ ] **Cite the algorithm.** When implementing a known algorithm, cite the paper/section in a comment.

### What NOT to document

- [ ] **No auto-generated `README.md` for the crate.** `README.md` at project root is the public pitch; the crate itself doesn't need one.

- [ ] **No `CHANGELOG.md`.** Git log + release notes suffice.

- [ ] **No docstrings that restate types.** `/// Returns a u64` adds nothing; delete it.

### Update surfaces when patterns change

- [ ] **If you introduced a new pattern / rule / proscription**, update `CLAUDE.md` (project-wide) or `docs/standards/rust/` (Rust-specific). Commit separately from the code change.

- [ ] **If the tier contract or tokenizer spec changed**, update `docs/architecture/three-tier-index.md` / `docs/indexing/tokenizer-spec.md`.

- [ ] **If `.pyi` changed**, update `docs/standards/rust/ffi.md` if the boundary rule changed.

---

## Cross-references

- [`../index.md`](../index.md)
- [`../ffi.md`](../ffi.md) — PyO3 boundary, stubs, `py.typed`
- [`../code-quality.md`](../code-quality.md) — `cargo doc` in the pipeline
- [`../../python/checklists/10-document.md`](../../python/checklists/10-document.md)
