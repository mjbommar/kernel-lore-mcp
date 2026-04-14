# Checklist: Debugging (Rust)

Rust counterpart to [`../../python/checklists/08-debug.md`](../../python/checklists/08-debug.md).

Observe, then hypothesize. Reproduce before fixing. Verify with a test.

---

## Non-negotiables

- [ ] Reproduce with a minimal test case before proposing a fix.
- [ ] `RUST_BACKTRACE=full` on every debug run.
- [ ] `tokio-console` NOT applicable — we have no tokio on the Rust side.
- [ ] Every bug fix ships with a regression test (failing before, passing after).

> Source: [`../testing.md`](../testing.md), [`../../../CLAUDE.md`](../../../CLAUDE.md).

---

## Debugging steps

### Reproduce

- [ ] **Write a minimal `#[test]` that triggers the bug.** If the bug is on the Python side of the boundary, a minimal `pytest` that crosses to Rust.

- [ ] **Fixture first.** If the bug depends on real mbox data, add a cut-down sample to `tests/python/fixtures/`.

- [ ] **Can't reproduce locally? Stop.** Collect more info before guessing. Flaky repro is usually a threading/ordering bug — note that.

### Observe

- [ ] **`RUST_BACKTRACE=full cargo test <name>`** — full stack on panic.
  ```bash
  RUST_BACKTRACE=full cargo test -- --nocapture test_name
  ```

- [ ] **`RUST_LOG=debug`** (or `trace`) with `tracing-subscriber` enabled in the test harness:
  ```bash
  RUST_LOG=kernel_lore_mcp=debug cargo test -- --nocapture
  ```

- [ ] **Read the backtrace bottom-up.** The panic site is at the bottom. Find your code in the middle.

- [ ] **Check the error chain.** `thiserror` preserves `#[source]`. Walk it:
  ```rust
  let mut cur: &dyn std::error::Error = &err;
  while let Some(src) = cur.source() {
      eprintln!("caused by: {src}");
      cur = src;
  }
  ```

### Macro-expansion issues

- [ ] **`cargo expand`** (requires `cargo install cargo-expand`). When the problem is inside a derive or a PyO3 macro:
  ```bash
  cargo expand --lib module_name::item_name
  ```

- [ ] **`#[pyfunction]` / `#[pyclass]` expansion** reveals the actual generated wrapper — often the bug is in how the wrapper converts types.

### Binary-level debug

- [ ] **`rust-gdb target/debug/deps/kernel_lore_mcp-<hash>`** — source-aware gdb for segfaults, FFI crashes, or heap corruption.

- [ ] **`rust-lldb`** on macOS.

- [ ] **Debug symbols in release:** add `debug = true` under `[profile.release]` temporarily for flamegraph/perf work.

### PyO3-specific

- [ ] **Crash under Python but not `cargo test`?** Likely a GIL / refcount / conversion bug. Test with `RUST_BACKTRACE=full uv run pytest tests/python -v -x`.

- [ ] **Hang under Python?** Check for GIL reacquisition order. `Python::detach` -> inside, you must NOT call anything that wants the GIL without re-attaching.

- [ ] **Wrong `PyErr` type on the Python side?** Check `src/error.rs` — does the `From<Error> for PyErr` map the variant to the right exception class?

### Concurrency

- [ ] **Data race?** `cargo +nightly test --target x86_64-unknown-linux-gnu -Z sanitizer=thread` when suspected. Rare — most races show up as Send/Sync compile errors.

- [ ] **rayon task hang?** Likely an unbounded channel, a recursive `rayon::scope` inside a pool-exhausted task, or a Mutex deadlock. Dump stacks with `gdb` attach.

- [ ] **Writer contention?** There should be ONE writer. If two exist, a second process opened an index it shouldn't have. Check `state.rs` lockfile.

### Tantivy-specific

- [ ] **`tantivy::TantivyError::SchemaError`?** Schema mismatch between writer and reader. Check `schema.rs` fingerprint sidecar.

- [ ] **Stale reader?** The reader didn't reload. Verify `reader.reload()?` runs on every request entry per the generation-file protocol.

- [ ] **"Lock poisoned"?** A previous writer panicked. Check `index.writer.lock` and the tracing logs from the crash.

### gix-specific

- [ ] **"object not found"?** `last_oid` references a commit that was repacked / pruned. Fall back to full re-walk + dedupe by `message_id` (see TODO.md Phase 1).

- [ ] **Thread-safety complaint?** Use `ThreadSafeRepository`, not `Repository`.

### State inspection

- [ ] **Add `tracing` spans at tier boundaries.** `ingest::shard`, `router::dispatch`, `bm25::query`, `trigram::scan`. Re-run with `RUST_LOG=trace` to see the span lifecycle.

- [ ] **Dump type with `dbg!` in tests** (not committed). Fine for iteration; remove before commit (`06-review.md`).

### Verify the fix

- [ ] **Regression test fails before the fix, passes after.**

- [ ] **Check for the pattern elsewhere.** If you found "missing `?`" or "wrong lifetime" in one place, grep for the same pattern across `src/`.

- [ ] **Commit fix with root-cause summary in the body.**

---

## Cross-references

- [`../testing.md`](../testing.md)
- [`../design/concurrency.md`](../design/concurrency.md)
- [`../ffi.md`](../ffi.md)
- [`../../python/checklists/08-debug.md`](../../python/checklists/08-debug.md)
