# Checklist: Optimization

Adapted from `.../kaos-modules/docs/python/checklists/09-optimize.md`.

Measure, change, measure. Never ship a perf claim without
before/after numbers in the commit body.

---

## Non-negotiables

- [ ] **No perf change without a benchmark.** `cargo bench`
      (criterion) for Rust, `py-spy` or `pytest-benchmark` for
      Python.
- [ ] **No "fast enough" without numbers.** State the target
      (ms/query, MB/s ingest) and whether you hit it.

---

## Items

- [ ] **Profile before touching code.**
      ```bash
      # Rust hot path
      cargo bench --bench router

      # Python side of an MCP request
      uv run py-spy record -o flame.svg -- uv run python -m kernel_lore_mcp ...

      # Micro-benchmark a specific function
      uv run python -c "import timeit; print(timeit.timeit('f()', setup='from x import f', number=1000))"
      ```
      > Ref: [../code-quality.md](../code-quality.md)

- [ ] **Identify the actual bottleneck.** Is >50% of time in one
      function? That is the bottleneck. Optimizing a 2% function
      saves nothing. Read the flame graph bottom-up.

- [ ] **Check if we already have a faster path.**
      - Trigram candidate list too large? Narrow with a metadata
        pre-filter.
      - BM25 slow? Verify `IndexRecordOption::WithFreqs` and
        tokenizer without positions.
      - Metadata scan slow? Add a column projection; don't read
        all fields.

- [ ] **Batch across the PyO3 FFI boundary.** Per-call overhead
      is ~25 ns, but it adds up. Pass a list in, get a list back.
      Never loop `for x in xs: _core.do(x)` at Python scale.
      > Ref: [../pyo3-maturin.md](../pyo3-maturin.md)

- [ ] **Detach the GIL around heavy Rust work.** Inside
      `#[pyfunction]`, wrap the heavy block in
      `py.detach(|| { ... })`. Paired with
      `await asyncio.to_thread(...)` on the Python side, this is
      how multiple queries run in parallel.

- [ ] **Use `asyncio.gather` for independent I/O.** N shard
      reads, N metadata fetches, N HTTP calls — run concurrently,
      not sequentially. Rate-limit with `asyncio.Semaphore`.

- [ ] **Use lazy evaluation for large data.** Generators instead
      of materialized lists when the consumer is streaming.
      Parquet column projection instead of full-row read.

- [ ] **Watch allocations.** `bytearray` for binary building,
      `io.StringIO` for text concatenation. On the Rust side,
      prefer `&str` / `&[u8]` views over `String` / `Vec<u8>`
      in hot loops.

- [ ] **Check cache behavior.** Rayon task per shard (not per
      commit) — packfile cache is per-repo. Tantivy reader reload
      is manual precisely so cache warmth survives queries.

- [ ] **Consider streaming for large MCP responses.** Hits over
      ~256 KB total belong behind a resource URI, not a single
      tool return.

- [ ] **Measure again.** Run the same benchmark. If the
      improvement is <10%, reconsider whether added complexity
      is worth it. If it is 10x, double-check the benchmark
      isn't lying (cache warm, wrong input size, constant
      folded).

- [ ] **Document the numbers in the commit.** Template:
      ```
      perf(trigram): 3.2x speedup via roaring bitmap intersection

      Before: 41.2 ms/query (v6.8 shard, `dfa:skb_unlink`, 1000 iterations)
      After:  12.8 ms/query (same)
      Bench:  cargo bench --bench router -- skb_unlink
      Why:    precomputed intersection for two-term queries;
              three-term+ falls back to the old path.
      ```
      > Ref: [07-commit.md](07-commit.md)

- [ ] **Update the relevant guide.** If the change surfaces a new
      pattern (e.g., "batch across FFI for list-shaped args"),
      add it to `docs/standards/python/` or `docs/architecture/`
      in the same commit.
