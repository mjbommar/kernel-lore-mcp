# Checklist: Research and Planning (Rust)

Rust counterpart to [`../../python/checklists/01-research.md`](../../python/checklists/01-research.md).

Before touching Rust code, understand the existing invariants, the relevant architecture doc, and the project's proscriptions. Rust changes cost more than Python changes — the research phase pays that cost down.

---

## Non-negotiables

- [ ] No stemmer (tantivy `stemmer` feature is OFF). Not proposing one.
- [ ] No `git2-rs` / `libgit2`. `gix` only.
- [ ] No `allow_threads` / `with_gil` in new code. Use `Python::detach` / `Python::attach`.
- [ ] No `rayon` task inside a `tokio` runtime (or vice versa). Not proposing one.
- [ ] No `tokio` on the Rust side — I/O concurrency lives in the Python async layer.
- [ ] No `anyhow::Error` in library function signatures.

> Source: [`../../../CLAUDE.md`](../../../CLAUDE.md), [`../index.md`](../index.md).

---

## Research steps

- [ ] **State the goal in one sentence.** What Rust invariant or tier does this change touch (metadata / trigram / BM25 / ingest / router / store / schema)? If you can't name the tier, you aren't ready.

- [ ] **Read the authoritative proscription.** Re-read the relevant section of [`../../../CLAUDE.md`](../../../CLAUDE.md) — "What NOT to use", tokenizer proscriptions, stack pins.

- [ ] **Read the Rust standards guide.** [`../index.md`](../index.md) is the entry point; follow links to the relevant design/library guide.

- [ ] **Read the relevant architecture doc.**
  - Ingest work: [`../../../architecture/four-tier-index.md`](../../../architecture/four-tier-index.md) + [`../../../architecture/over-db.md`](../../../architecture/over-db.md) + `docs/ingestion/` subdir.
  - Indexing work: `docs/indexing/` (tier spec, tokenizer spec).
  - Router / query grammar: `docs/mcp/query-routing.md`.
  - Storage / on-disk layout: `docs/architecture/` + `docs/ingestion/`.

- [ ] **Search the existing Rust tree for prior art.** Use `Grep` / `Glob` over `src/*.rs`. Does `store.rs`, `schema.rs`, `state.rs` already expose what you need? Do not duplicate.

- [ ] **Inspect the crate you plan to call.** `cargo doc --open -p <crate>` or read the source under `~/.cargo/registry/src/`. Never guess a signature — verify it.

- [ ] **Identify the correct layer.** Pure-Rust core (`src/*.rs` excluding `lib.rs`), PyO3 glue (`src/lib.rs` + `#[pymodule]`), or binary (`src/bin/*.rs`, `anyhow` allowed). See [`../design/boundaries.md`](../design/boundaries.md).

- [ ] **Decide: library error (`thiserror`) or binary error (`anyhow`)?** Only binaries get `anyhow`. Library functions always return a typed `Error`.

- [ ] **Check the four-tier contract.** Does this change cross a tier boundary? Rebuildability contract: the compressed raw store is the source of truth; metadata Parquet, trigram, and BM25 rebuild from it; over.db rebuilds from metadata Parquet.

- [ ] **Check the single-writer contract.** `tantivy::IndexWriter`, trigram builder, and store appender all live in `klmcp-ingest`, never the MCP server. If you're about to open a writer in the wrong binary, stop.

- [ ] **Check the tokenizer spec.** If this touches tokenization: no stemming, no stopwords, no asciifolding, no typo tolerance. Preserve leading-underscore signal. Atomic tokens for emails / Message-IDs / SHAs / CVEs.

- [ ] **Verify PyO3 thread model.** If you plan to release the GIL, use `Python::detach`. If you need the GIL, use `Python::attach`. Heavy Rust work (tier dispatch, ingest) must release the GIL.

- [ ] **Verify Send/Sync.** Any type that crosses a `rayon::scope` or a thread boundary must be `Send + Sync`. `gix::ThreadSafeRepository` — yes. `tantivy::IndexWriter` — single-threaded by design.

- [ ] **Check TODO.md.** Is this item listed? Which phase? Are its prerequisites `[x]`? Do not jump phases.

- [ ] **Sketch the benchmark.** If this is a hot path, plan the `criterion` bench now. What inputs, what budget, what regression threshold?

- [ ] **Check MSRV.** Your change must compile on `cargo +1.88 build`. Don't pull in an API that requires a newer compiler without moving the toolchain pin (which is a separate, justified commit).

---

## Cross-references

- [`../index.md`](../index.md) — Rust Standards overview
- [`../../python/checklists/01-research.md`](../../python/checklists/01-research.md) — Python counterpart
- [`../../../CLAUDE.md`](../../../CLAUDE.md) — proscriptions
- [`../../../TODO.md`](../../../TODO.md) — execution contract
