# Rust SDLC Checklists — kernel-lore-mcp

Rust counterpart to [`../../python/checklists/index.md`](../../python/checklists/index.md).

Progressive checklists for every stage of the Rust core's development lifecycle. Use each as a pre-flight check. They distill the most important rules from the [Rust Standards](../index.md), [`../../../CLAUDE.md`](../../../CLAUDE.md), and [`../../../TODO.md`](../../../TODO.md).

---

## The Lifecycle

```
Research -> Design -> Implement -> Test -> Quality -> Review -> Commit
                                                         |
                                           Debug <- Optimize -> Document
```

---

## Non-Negotiables (pulled from CLAUDE.md + docs/standards/rust/index.md)

These are never optional, regardless of the task:

| Rule | Source |
|------|--------|
| Rust stable 1.88, edition 2024 (pinned in `rust-toolchain.toml`) | [index.md](../index.md) |
| PyO3 0.28.3: use `Python::detach` / `Python::attach`. NEVER `allow_threads` / `with_gil` in new code | CLAUDE.md |
| `tantivy` stemmer feature is OFF. Never enable. | CLAUDE.md |
| `gix` 0.81, NOT `git2-rs` (not `Sync`) | CLAUDE.md |
| `regex-automata` DFA-only. No backrefs. | CLAUDE.md |
| No `tokio` on the Rust side. I/O concurrency lives in Python. | [design/concurrency.md](../design/concurrency.md) |
| Never mix `rayon` inside `tokio` or vice versa | [index.md](../index.md) |
| `thiserror` in libraries; `anyhow` only in `src/bin/*` | [index.md](../index.md) |
| Every `thiserror` variant that can cross PyO3 has `impl From<_> for PyErr` | [design/errors.md](../design/errors.md) |
| No `anyhow::Error` crosses the PyO3 boundary | TODO.md Phase 1 |
| `unsafe` only with a `SAFETY:` comment + justification | [unsafe.md](../unsafe.md) |
| `cargo clippy --all-targets -- -D warnings` is green before commit | [code-quality.md](../code-quality.md) |
| Positions OFF in BM25 (`IndexRecordOption::WithFreqs`) — no phrase queries on prose | CLAUDE.md |
| `abi3-py312` gated behind a default Cargo feature (free-threaded builds disable it) | TODO.md Phase 1 |
| Never split ingestion across workers within a shard (packfile cache locality) | CLAUDE.md |
| Single writer: `tantivy::IndexWriter` lives in one process (`klmcp-ingest`), never the MCP server | CLAUDE.md |
| Reader reload discipline: `stat(generation_file)` + `reader.reload()?` on every request entry | CLAUDE.md |
| Benchmark before/after numbers are committed for every `perf` commit | [testing.md](../testing.md) |

---

## Stage Summary

### 1. [Research and Planning](01-research.md)

Before writing any Rust code, understand the invariants and where the code lives.

- [ ] State the goal in one sentence
- [ ] Verify no proscription blocks the approach (stemmer / git2-rs / allow_threads / rayon-in-tokio)
- [ ] Read the relevant `docs/architecture|indexing|ingestion` subdir
- [ ] Identify the right layer: pure-Rust core, PyO3 glue, or binary

### 2. [Design and Architecture](02-design.md)

Types first. Sketch signatures before writing a line of logic.

- [ ] Types and trait impls defined before any `fn` body
- [ ] Error variants modeled via `thiserror` with source chain
- [ ] Module placement decided (core vs glue vs binary)
- [ ] Send/Sync requirements identified for cross-thread types

### 3. [Implementation](03-implement.md)

Write clean, typed Rust with tight module surface.

- [ ] Prefer `pub(crate)` over `pub`
- [ ] `#[must_use]` on fallible/result-producing returns
- [ ] `#[derive(Debug)]` everywhere it aids diagnostics
- [ ] Continuous: `cargo fmt` -> `cargo clippy` -> `cargo test`

### 4. [Testing](04-test.md)

`cargo test` covers pure Rust; PyO3 glue tested from Python. Property + snapshot + criterion as needed.

- [ ] Unit tests in `#[cfg(test)] mod tests`
- [ ] `proptest` for invariants (roundtrips, monotonic merges)
- [ ] `criterion` benches for anything on the hot path
- [ ] `insta` for snapshot tests (tokenizer output, query-plan shapes)

### 5. [Quality](05-quality.md)

Every change passes the full Rust QA pipeline.

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test`
- [ ] `cargo doc --no-deps`
- [ ] MSRV: `cargo +1.88 build`

### 6. [Self-Review](06-review.md)

Re-read your own diff. Check what the compiler cannot.

- [ ] Every `unsafe` block has `SAFETY:`
- [ ] Every `panic!` / `.unwrap()` / `.expect()` in release paths is intentional
- [ ] Send/Sync verified for types crossing threads
- [ ] No `anyhow::Error` returned from a library function
- [ ] `impl From<_> for PyErr` on every error variant that can cross PyO3

### 7. [Commit and Push](07-commit.md)

Atomic, buildable commits.

- [ ] `Cargo.lock` bump NOT mixed with feature code
- [ ] Dep-pin bumps (CLAUDE.md) get their own commit with rationale
- [ ] No generated wheels, `target/`, or indexed data committed
- [ ] `perf` commits include before/after criterion numbers

### 8. [Debugging](08-debug.md)

Observe, then hypothesize.

- [ ] Reproduce with `RUST_LOG=debug RUST_BACKTRACE=full`
- [ ] `cargo expand` for macro-generated mysteries
- [ ] `rust-gdb` for segfaults / FFI crashes
- [ ] `tracing` spans at tier boundaries

### 9. [Optimization](09-optimize.md)

Measure, then cut.

- [ ] `cargo bench` via `criterion` first
- [ ] `cargo flamegraph` for CPU hotspots
- [ ] `heaptrack` / `dhat` for allocations
- [ ] PGO via `cargo pgo` only if criterion justifies
- [ ] Never optimize without committed before/after numbers

### 10. [Documentation](10-document.md)

Doc comments on every public item.

- [ ] `///` on every `pub` item
- [ ] Code examples in `///` blocks compile via `cargo test --doc`
- [ ] Cross-link with `[`Type`]` rustdoc links
- [ ] Module-level `//!` describes the module's role in the four-tier architecture

---

## Cross-references

- [`../index.md`](../index.md) — Rust Standards overview
- [`../../python/checklists/index.md`](../../python/checklists/index.md) — Python counterpart
- [`../../../CLAUDE.md`](../../../CLAUDE.md) — project proscriptions (authoritative)
- [`../../../TODO.md`](../../../TODO.md) — execution contract
- [`../ffi.md`](../ffi.md) — PyO3 boundary rules
- [`../design/errors.md`](../design/errors.md) — thiserror + PyErr mapping
- [`../design/concurrency.md`](../design/concurrency.md) — rayon discipline
