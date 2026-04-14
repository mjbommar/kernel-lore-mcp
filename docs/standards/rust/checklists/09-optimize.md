# Checklist: Optimization (Rust)

Rust counterpart to [`../../python/checklists/09-optimize.md`](../../python/checklists/09-optimize.md).

Measure. Cut. Prove with numbers. Every optimization commit carries committed before/after.

---

## Non-negotiables

- [ ] Never optimize without committed before/after criterion numbers.
- [ ] The function being optimized is demonstrably on the hot path (flamegraph or profile shows it).
- [ ] No `unsafe` added for speed without a safe-baseline delta.
- [ ] Optimizations preserve correctness (tests + property tests green).

> Source: [`../testing.md`](../testing.md), [`../index.md`](../index.md).

---

## Optimization steps

### Measure first

- [ ] **Baseline with `criterion`.**
  ```bash
  cargo bench -- --save-baseline before
  ```

- [ ] **CPU profile with `cargo flamegraph`.**
  ```bash
  cargo install flamegraph
  cargo flamegraph --bench router
  ```
  Or on a running binary:
  ```bash
  cargo flamegraph --bin reindex -- --shard /path/to/shard
  ```

- [ ] **`perf record` + `perf report`** on Linux for deeper analysis:
  ```bash
  cargo build --release
  perf record -g ./target/release/reindex ...
  perf report
  ```

- [ ] **Allocation profile.**
  - `heaptrack ./target/release/reindex ...` -> `heaptrack_gui`
  - Or `dhat` via `#[global_allocator] dhat::Alloc`.

- [ ] **Identify the bottleneck.** If one function takes >30% of total time, that's the bottleneck. Optimizing a 2% function saves 2%.

### Sharpen the tight loop

- [ ] **Pre-allocate.** `Vec::with_capacity(n)`, `String::with_capacity(n)`, `HashMap::with_capacity_and_hasher(...)`.

- [ ] **Reuse buffers across iterations.** `buf.clear()` + refill.

- [ ] **Avoid bounds checks in proven-safe hot loops.** `iter()` + `enumerate()` beats indexed access; the compiler elides bounds checks when it can.

- [ ] **SmallVec / tinyvec** for collections that are almost always small.

- [ ] **`AHashMap` / `FxHashMap`** for hash-heavy inner loops (stdlib HashMap uses SipHash for DoS resistance, unnecessary for internal keys).

- [ ] **Replace `String` with `&str` / `Cow<'a, str>`** in pure-function signatures.

- [ ] **Inline hints (`#[inline]`, `#[inline(always)]`)** sparingly. Benchmark the delta; only keep if measurable.

### Data structures

- [ ] **Roaring bitmap merges** for trigram postings — already the choice; verify you're using `BitOr`-based merge, not element-wise.

- [ ] **`fst::Map` range queries** for term dictionary — O(log n) prefix scans.

- [ ] **Arrow columnar access** — scan by column, not by row. Filter pushdown via predicates.

- [ ] **Zero-copy where possible.** `bytes::Bytes`, `memmap2` for mmap reads, `zerocopy` for POD transmute.

### Parallelism

- [ ] **`rayon::par_iter()`** for shard-parallel ingest. One task per shard; never within.

- [ ] **Thread pool size configurable** from Python (`kernel_lore_mcp.init(rayon_threads=N)` — TODO.md Phase 1).

- [ ] **Never spawn more rayon tasks than cores.** The pool is global.

### FFI cost

- [ ] **Batch across the PyO3 boundary.** ~25 ns per FFI call adds up at scale. A single `dispatch(queries: Vec<Query>) -> Vec<Hits>` beats N calls to `dispatch(query)`.

- [ ] **Release the GIL** on any call > 1 ms. `py.detach(|| ...)`.

- [ ] **Avoid `.to_string()` / `.to_vec()` in glue.** Borrow where you can.

### Compiler optimizations

- [ ] **Verify release profile is right (in `Cargo.toml`):**
  ```toml
  [profile.release]
  lto = "thin"
  codegen-units = 1
  panic = "abort"
  strip = "symbols"
  ```
  (TODO.md Phase 0.)

- [ ] **PGO via `cargo-pgo`** when criterion justifies:
  ```bash
  cargo install cargo-pgo
  cargo pgo build
  cargo pgo run -- <training workload>
  cargo pgo optimize
  ```
  Only apply PGO if baseline -> PGO delta is >= 5% on a representative workload.

- [ ] **Cross-check `-C target-cpu=native`** for single-box deploys. Requires a separate build for the deploy platform (r7g.xlarge = Graviton). Never ship a generic-x86_64 binary to arm64 or vice versa.

### Memory

- [ ] **Watch peak RSS during ingest.** Heaptrack reports peak. The hot set target is 25-45 GB (CLAUDE.md); peak ingest should stay within EC2 budget (r7g.xlarge = 32 GB).

- [ ] **Dictionary-trained zstd** for the compressed store — already the choice; verify dict-per-list, not global.

### Re-measure

- [ ] **`cargo bench -- --baseline before`** — compare against the saved baseline.

- [ ] **Reject <10% improvements** unless the change is simpler than the original. Complexity for 5% is a bad trade.

- [ ] **Commit numbers.** Body of the commit:
  ```
  perf(router): 3.2x speedup in BM25 tier via IDF table

  Criterion: router::bm25 on 100k-doc corpus
  before: 4.21 ms/query (p50), 8.1 ms (p99)
  after:  1.31 ms/query (p50), 2.5 ms (p99)

  Trade-off: +48 MB RAM (IDF table). Budget OK on r7g.xlarge.
  ```

---

## Cross-references

- [`../testing.md`](../testing.md)
- [`../index.md`](../index.md)
- [`../design/data-structures.md`](../design/data-structures.md)
- [`../ffi.md`](../ffi.md)
- [`../../python/checklists/09-optimize.md`](../../python/checklists/09-optimize.md)
