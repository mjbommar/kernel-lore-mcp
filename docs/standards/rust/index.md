# Rust Standards — kernel-lore-mcp

Parallel to [`../python/index.md`](../python/index.md). Covers the
native core (ingestion, indexing, query routing) and the Rust side
of the PyO3 boundary.

## Philosophy (five principles, mirrored from Python)

1. **Ecosystem first.** Prefer stdlib, a pinned set of crates, and
   our own code. Adding a crate is a maintenance commitment.
2. **Benchmark-driven.** `criterion` benches, `flamegraph` profiles,
   committed before/after numbers. No "probably fast."
3. **Checked at compile time.** Clippy with `-D warnings`. No
   `unsafe` without justification and a `SAFETY:` comment. `cargo
   test` covers pure-Rust paths; FFI glue covered from the Python
   side.
4. **Tested with real data.** Synthetic fixtures for unit tests;
   real lore corpus for integration. Never let a code path rot
   behind a mock.
5. **Make the Rust side carry its weight.** If a Rust function
   doesn't measurably earn its place over pure Python, delete it.
   See [`ffi.md`](ffi.md) for the cost model.

## Stack pins (authoritative: `../../CLAUDE.md`)

- Rust **stable 1.85** (edition 2024).
  Pinned in `rust-toolchain.toml`.
- `pyo3` 0.28.3 — uses `Python::detach` / `Python::attach`, NOT the
  renamed-from names (`allow_threads` / `with_gil`).
- `tantivy` 0.26 — stemmer feature never enabled.
- `gix` 0.81 — not `git2-rs`. `ThreadSafeRepository` for fanout.
- `mail-parser` 0.11 — `full_encoding` feature for legacy charsets.
- `roaring` 0.11, `fst` 0.4 — trigram tier.
- `regex-automata` 0.4 — DFA-only regex, safe for untrusted input.
- `arrow` 58, `parquet` 58 — metadata tier.
- `rayon` 1.10 — data parallelism; one task per lore shard.
- Errors: `thiserror` in library; `anyhow` only in binaries.

## Guides

| Guide | When to read |
|-------|--------------|
| [language.md](language.md) | Edition 2024 features we rely on + MSRV |
| [cargo.md](cargo.md) | Workspace, features, profiles, Cargo.toml rules |
| [code-quality.md](code-quality.md) | fmt / clippy / cargo-deny pipeline |
| [testing.md](testing.md) | `cargo test`, `proptest`, `criterion`, `insta` |
| [naming.md](naming.md) | Crate / module / function conventions |
| [ffi.md](ffi.md) | PyO3 boundary rules (pairs with python/pyo3-maturin.md) |
| [unsafe.md](unsafe.md) | When (rarely) and how |

### Design (`design/`)

| Guide | Description |
|-------|-------------|
| [modules.md](design/modules.md) | Module tree, privacy, re-exports |
| [boundaries.md](design/boundaries.md) | Pure-Rust core, PyO3 glue, binary targets |
| [concurrency.md](design/concurrency.md) | rayon vs tokio; Send/Sync; single-writer discipline |
| [data-structures.md](design/data-structures.md) | Vec/Box/Arc/Cow/SmallVec; roaring; fst |
| [errors.md](design/errors.md) | thiserror design; From<_> for PyErr |

### Libraries (`libraries/`)

The ones we actually use:

| Guide | Description |
|-------|-------------|
| [pyo3.md](libraries/pyo3.md) | 0.28 idioms; detach/attach; module init |
| [tantivy.md](libraries/tantivy.md) | Schema, tokenizer registration, reader reload |
| [gix.md](libraries/gix.md) | ThreadSafeRepository, rev_walk, incremental |
| [roaring-fst.md](libraries/roaring-fst.md) | Trigram tier primitives |
| [regex-automata.md](libraries/regex-automata.md) | DFA-only discipline |
| [arrow-parquet.md](libraries/arrow-parquet.md) | Metadata tier |
| [zstd.md](libraries/zstd.md) | Compressed store; dict training |

### Checklists (`checklists/`)

Parallel to Python's:

| Checklist | Stage |
|-----------|-------|
| [01-research.md](checklists/01-research.md) | Before touching code |
| [02-design.md](checklists/02-design.md) | New module / crate |
| [03-implement.md](checklists/03-implement.md) | Implementation |
| [04-test.md](checklists/04-test.md) | Testing |
| [05-quality.md](checklists/05-quality.md) | Pre-commit |
| [06-review.md](checklists/06-review.md) | Self-review |
| [07-commit.md](checklists/07-commit.md) | Commit + push |
| [08-debug.md](checklists/08-debug.md) | Debugging |
| [09-optimize.md](checklists/09-optimize.md) | Optimization |
| [10-document.md](checklists/10-document.md) | Documentation |

## Quick decision trees

### Library (`thiserror`) or binary (`anyhow`)?

```
Are you writing a library function (called from other modules)?
  → thiserror::Error with per-module variants

Are you writing a binary target (src/bin/*)?
  → anyhow::Result — the error will only be formatted, not matched
```

### rayon or tokio?

```
CPU-bound data-parallel work? (ingestion, index building)
  → rayon

I/O-bound concurrency? (HTTP, async DB)
  → tokio. (In our project this is entirely on the Python side.)

Mixing both? (call tokio from inside rayon task)
  → STOP. Redesign. These runtimes don't cooperate.
```

### `Arc<T>` or `&T`?

```
Shared across threads AND ownership is runtime-dynamic?
  → Arc<T>

Shared across threads, one clear owner?
  → &T with explicit lifetime

Single-threaded, shared ownership?
  → Rc<T>

Not shared?
  → Box<T> or owned T
```

## Cross-references

- [`../../CLAUDE.md`](../../../CLAUDE.md) — project proscriptions.
- [`../../TODO.md`](../../../TODO.md) — execution contract.
- [`../python/index.md`](../python/index.md) — Python counterpart.
- [`../python/pyo3-maturin.md`](../python/pyo3-maturin.md) —
  authoritative PyO3 rules (this Rust side just describes the
  implementation side; the shared contract lives in the Python doc).
