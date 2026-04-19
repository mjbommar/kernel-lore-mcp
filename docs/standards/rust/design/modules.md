# Module Tree — Rust

Rust counterpart to `../../python/design/modules.md`.

Where the Python guide argues about `.py` files and packages,
this one argues about `.rs` files and their `mod` hookup. The
underlying question is identical: what belongs together, what
belongs apart, and who re-exports what.

---

## Our tree (authoritative)

```
src/
  lib.rs                -- #[pymodule] root, `mod` declarations
  error.rs              -- thiserror enum + From<Error> for PyErr
  state.rs              -- last_indexed_oid, generation, writer lockfile
  schema.rs             -- shared Arrow + tantivy field defs
  store.rs              -- compressed raw store (zstd-dict, segments)
  metadata.rs           -- Arrow/Parquet columnar tier
  trigram.rs            -- fst + roaring trigram tier
  bm25.rs               -- tantivy tier
  ingest.rs             -- gix shard walk; drives store + metadata + trigram + bm25
  router.rs             -- query grammar, tier dispatch, merge
  bin/
    reindex.rs          -- rebuild indices from store
  kernel_lore_mcp/      -- Python package (maturin mixed layout)
    __init__.py
    _core.pyi           -- type stubs for the #[pymodule]
    ...
```

Every `.rs` file above is one `mod X;` line in `lib.rs`. New
sibling modules are added to `lib.rs` in **alphabetical order**
(makes rebase conflicts trivial).

See `../../../architecture/four-tier-index.md` for the
high-level design this tree implements.

---

## The sibling rule

**Sibling modules at the same level don't import from each other
unless a one-way dependency is justified and declared.**

This is the Rust analogue of the Python "extraction modules never
import from each other" rule. In our tree:

- `store.rs`, `metadata.rs`, `trigram.rs`, `bm25.rs` are siblings.
  None of them import from each other. They only depend on
  `schema`, `state`, and `error`.
- `ingest.rs` depends on all four tiers **and** `schema`, `state`,
  `error`. That direction is fine — it's the orchestrator.
- `router.rs` depends on all three index tiers plus `metadata`
  for predicate pushdown. Also fine — it's the other orchestrator.
- **Never:** `metadata.rs` importing from `bm25.rs`. If you find
  yourself wanting to, the shared thing goes into `schema.rs`.

The dependency graph is a DAG rooted at `error`, `state`, `schema`:

```
error  <-  state  <-  schema  <-  store, metadata, trigram, bm25
                                    ^
                                    |
                                  ingest, router  (orchestrators)
```

If a code review shows a cycle or a lateral sibling import, that
is the review finding. Promote the shared code to a lower layer
(`schema`, `state`, `error`) rather than cross-importing.

---

## Module size thresholds

Same numbers as the Python guide, same justification.

| Lines | Treatment |
|-------|-----------|
| < 300 | Always fine. |
| 300–800 | Fine if the file has one coherent purpose. |
| > 800 | Examine for independent subsystems; split. |

`store.rs` is a good worked example of the upper-bound case.
When it grows past ~600 lines, the natural split is a
`src/store/` directory:

```
src/store/
  mod.rs          -- pub re-exports: Store, StoreReader, StoreWriter
  dict.rs         -- zstd dictionary training + open
  segment.rs      -- segment append + seal + read (offset, length)
  index.rs        -- message_id -> (segment_id, offset, length) Parquet side
```

Split criteria (Rust-specific add-ons over the Python list):

1. **Feature-flag boundary.** If half the file is behind
   `#[cfg(feature = "foo")]`, that half wants its own file. We
   don't have this today, but it would trigger a split.
2. **`unsafe` quarantine.** Any `unsafe` block gets its own
   submodule with a `SAFETY:` doc comment. Reviewers look at one
   file, not the whole module.
3. **Independent test module.** If you have one giant
   `#[cfg(test)] mod tests` that has four clearly distinct
   fixture sets, the file has four concerns. Split.

---

## When NOT to split

Do not create `src/foo/` for a single file. A directory with one
`mod.rs` plus one `bar.rs` is pure noise. The Python guide's "four
levels of nesting for a single function" anti-pattern applies
verbatim.

Do not create `utils.rs` or `helpers.rs`. Name the file after
what it does. If we had ASCII-normalization helpers we'd put
them in a named file (`ascii.rs`), not in a grab bag.

---

## `pub` / `pub(crate)` discipline

Rust's privacy system is our public API boundary. Get it right
at file-creation time; loosening later is an API break.

### Rules

1. **Default to `pub(crate)`.** Items used across modules inside
   this crate but NOT part of the PyO3-facing surface stay
   `pub(crate)`. Example: `store::SegmentWriter::append`.
2. **`pub` means `kernel_lore_mcp._core` exposes it.** Because
   `[lib] crate-type = ["cdylib", "rlib"]`, a `pub` item is
   reachable by:
   - Python via the `#[pymodule]` block in `lib.rs`.
   - Any future Rust consumer linking the `rlib`.
3. **`pub(super)` is almost always wrong in our tree.** We're
   flat. If you wrote `pub(super)`, what you wanted was
   `pub(crate)`.
4. **Never `pub use` something just to satisfy a warning.** If
   clippy says `dead_code`, either the thing is genuinely
   dead (delete it) or the caller hasn't landed yet (gate with
   `#[allow(dead_code)]` **with a comment** about which TODO
   phase will use it — see `error.rs` for the pattern).
5. **`#[doc(hidden)]` for stable-but-not-public.** We have zero
   of these today. If we ever need a workaround item that must
   be `pub` for technical reasons but shouldn't be part of the
   API, document it.

### Example from our code

```rust
// src/error.rs
pub enum Error {            // pub — crosses the PyErr boundary
    #[error("...")]
    Gix(String),            // no pub — enum variants inherit
    ...
}

pub type Result<T> = ...;   // pub — used by every module
```

`Error` is `pub` because `From<Error> for PyErr` needs to be
reachable from the `#[pymodule]`. `Error::Gix` variant is only
constructed inside `ingest.rs` (today) but is `pub` by virtue of
its parent enum being `pub`. That's fine — enums are closed.

---

## Re-exports in `lib.rs`

`lib.rs` in our crate has two jobs:

1. Declare `mod` for every sibling `.rs` file.
2. Host the `#[pymodule] mod _core { ... }` block.

It does **not** re-export library-internal items. There is no
`pub use crate::store::Store` at the top of `lib.rs`. The Python
side doesn't need it (it calls `#[pyfunction]`s inside
`_core`), and the `rlib` side should import from the module
path directly (`kernel_lore_mcp::store::Store`).

### The `#[pymodule]` inline-mod form

pyo3 0.28 prefers the inline-mod form over the older function
form. See `../libraries/pyo3.md` for the full rationale. Shape:

```rust
#[pymodule]
mod _core {
    use super::*;

    #[pyfunction]
    fn version() -> &'static str { env!("CARGO_PKG_VERSION") }

    // Re-register pyclasses / pyfunctions from sibling modules here.
    // They LIVE in sibling modules; they are VISIBLE here.
}
```

This is the single place where crate-internal items appear
under a public name. Adding a new MCP tool means:

1. Implement the pure-Rust function in the appropriate sibling
   module (e.g., `router.rs`).
2. Write a thin `#[pyfunction]` wrapper in `_core` that calls
   it and releases the GIL via `Python::detach`.
3. Stub it in `src/kernel_lore_mcp/_core.pyi`.

See `../../python/design/boundaries.md` and `../ffi.md` for the
thin-wrapper rule.

---

## `src/bin/` versus `src/lib.rs`

Binary targets under `src/bin/` are separate compilation units.
They `use kernel_lore_mcp::foo::bar;` exactly as an external
consumer would — they only see `pub` items.

This means: if a binary needs an internal helper, you have two
choices:

1. Promote the helper to `pub(crate)` and expose a `pub` facade
   for binaries. Prefer this when the helper is stable.
2. Duplicate the helper in the binary. Prefer this when the
   helper is binary-specific (argument parsing, progress
   reporting).

Our one binary today is `src/bin/reindex.rs`. It will eventually
need to open the store, walk its index, and feed the four tier
builders. That's all `pub` API. See `boundaries.md` for the
binary vs library split (anyhow vs thiserror).

---

## `tests/` — where integration tests live

Unit tests go in `#[cfg(test)] mod tests` inside the file they
test. Integration tests go in `tests/rust/` (parallel to
`tests/python/`). Integration tests see only `pub` items. If a
test needs something that is `pub(crate)`, that's a signal the
test is too low-level and wants a unit test instead.

---

## Anti-patterns (Rust-specific additions)

| Anti-pattern | Why bad | Fix |
|---|---|---|
| `pub use foo::*;` in `lib.rs` | Hides where things live; star-re-exports leak private items the day someone adds a `pub` accidentally. | Explicit `pub use`, one item per line. |
| `mod internal;` with all real code and a one-line facade file | Indirection with no payoff. | Put the code in the named file. |
| `mod foo { mod bar { ... } }` nested inline modules | Fine for test helpers, bad for production code — file and module names desync. | One file, one module. |
| Global `use super::*;` outside test modules | Imports everything from parent; refactors silently break. | Explicit `use` per item. |
| A `prelude` module | We have 11 modules. A prelude is overkill and hides dependencies. | Named imports. |

---

## Summary

| Decision | Guideline |
|---|---|
| New file? | Single concern, under 800 lines, wants its own `mod`. |
| New directory (`src/foo/`)? | At least 3 files sharing internal types. |
| `pub` or `pub(crate)`? | `pub(crate)` by default; `pub` only if PyO3 or downstream `rlib` needs it. |
| `pub use` in `lib.rs`? | Don't, except inside `#[pymodule] mod _core`. |
| Sibling imports? | Forbidden. Push shared code down to `schema`/`state`/`error`. |
| Where does a binary belong? | `src/bin/*.rs`. Binaries see only `pub` items. |

See also:
- `boundaries.md` — library vs binary; pure-Rust vs PyO3 glue.
- `errors.md` — why `error.rs` is the root of the DAG.
- `../libraries/pyo3.md` — the `#[pymodule]` inline-mod form.
