# Concurrency — Rust

Rust counterpart to `../../python/design/concurrency.md`.

The Python guide is async-first because its world is I/O-bound
(HTTP, Playwright, LLM streaming). Our Rust world is the exact
opposite: CPU-bound, data-parallel, single-writer-per-index.
The primitives are different; the reasoning is the same —
match the runtime to the workload, never mix runtimes.

---

## The one-liner

**rayon for CPU-parallel work. No tokio on the Rust side. No
manual `std::thread::spawn` except for the `flock`-holding
writer lifecycle. Async lives in Python.**

---

## rayon vs tokio vs std threads

### rayon — yes, for CPU-bound data parallelism

`rayon` 1.10 is our sole concurrency library. It owns a global
work-stealing thread pool, and we use it for:

- **Shard fanout at ingest time.** One rayon task per lore
  shard (~390 shards today; `rayon::current_num_threads()`
  pool). Never within a shard — packfile cache locality.
- **Parallel index build.** When `router` dispatches to tier
  readers, trigram and BM25 can run in parallel via
  `rayon::join` on the narrowed candidate set.
- **Per-candidate confirmation in the trigram tier.** After
  posting-bitmap intersection yields up to
  `TRIGRAM_CONFIRM_LIMIT` docids, decompress-and-confirm is
  embarrassingly parallel.

Key rayon idioms we use:

```rust
use rayon::prelude::*;

shards.par_iter().try_for_each(|shard| -> crate::Result<()> {
    let repo = shard.repo.to_thread_local();    // see gix.md
    ingest_shard(&repo, &mut emitter(shard))
})?;
```

```rust
let (meta_hits, bm25_hits) = rayon::join(
    || metadata::search(&ctx, &predicates),
    || bm25::search(&ctx, &bm25_query),
);
```

### tokio — no

Not in `Cargo.toml`. Not in `[dev-dependencies]`. Not pulled in
by a transitive dep we haven't audited (check `cargo tree`
before adding anything that might).

Reasons:

1. **Our HTTP lives in Python.** FastMCP routes requests; Rust
   just computes. No async needed.
2. **tokio and rayon don't compose.** A tokio task that
   `spawn_blocking`s into a rayon pool is an anti-pattern that
   reliably deadlocks under load (two pools, both finite).
3. **PyO3 `async fn` support exists but complicates the
   boundary.** We detach the GIL, do synchronous work, return.
   The Python side can `asyncio.to_thread` us if concurrency at
   the call site matters.

If you think you need tokio, re-read `boundaries.md`: the thing
you want is probably an MCP tool that delegates to an existing
Python async path.

### `std::thread` — one legitimate use

The single writer process (ingest) holds `writer.lock` via
`fs2::FileExt::lock_exclusive` for its lifetime. That lifetime
is the main thread. No `spawn` needed.

One exception: long-lived background maintenance (segment
compaction, generation-file stat poller) may want a dedicated
`std::thread` outside the rayon pool. When we add one:

- Name it (`.name("klmcp-compactor")`).
- Use a `crossbeam_channel` for shutdown signalling.
- Document why rayon can't own it (usually because it blocks
  on a file lock and would starve the pool).

Today we have zero of these.

---

## Send / Sync for our types

Rust enforces thread-safety at the type system. Most of our
types are auto-`Send + Sync` because they're composed of
primitives, `Arc<T>`, and owned data. Where they aren't, we
explain why.

| Type | Send | Sync | Notes |
|---|---|---|---|
| `crate::Error` | Y | Y | All variants wrap `Send + Sync` payloads. |
| `gix::ThreadSafeRepository` | Y | Y | The whole point of the type. `to_thread_local` produces a `!Send` `Repository`. |
| `gix::Repository` | Y | **N** | Created per-rayon-task via `to_thread_local`. Never cross-thread. |
| `tantivy::IndexWriter` | Y | Y | But only one exists per system — see single-writer discipline below. |
| `tantivy::IndexReader` | Y | Y | Cloned into each query path. |
| `Arc<fst::Map<_>>` | Y | Y | The map is `Send + Sync` once built; we share via `Arc`. |
| `RoaringBitmap` | Y | **N** (by current API) | Pass by move into rayon tasks; don't share refs. Use `Arc` + clone cheap metadata or produce new bitmaps per task. |

Compile-time proof pattern — add this static assertion in any
module that owns a type we claim is `Send + Sync`:

```rust
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MyType>();
};
```

We put these in `#[cfg(test)]` blocks to keep them out of
release artifacts.

---

## Single-writer discipline for tantivy

Tantivy supports one `IndexWriter` at a time; a second attempt
blocks on the internal lock. We enforce a stricter invariant:
exactly one process in the entire system holds a writer.

Mechanism — `src/state.rs`:

```
<data_dir>/state/writer.lock      -- advisory file lock
                                     held by the ingest process
                                     for its lifetime
```

Ingest acquires the lock at startup. The server process (any
worker) refuses to open a writer if the lock file is held. If
the lock file is stale (process died without releasing), the
`fs2` lock call will succeed on the next acquire — that's
fine; `flock` semantics are tied to the OS process, not the
file.

Why stricter than "one writer per index dir"? Because our
server deployment has N uvicorn workers reading the same index.
One of them accidentally opening a writer would silently
corrupt state. The lockfile + refuse-to-open pattern makes the
failure loud and immediate.

See `../libraries/tantivy.md` for the read-side reload
discipline (`ReloadPolicy::Manual` + generation stat).

---

## `ThreadSafeRepository::to_thread_local` pattern

`gix 0.81` has two repository types:

- `gix::ThreadSafeRepository` — `Send + Sync`. Can cross
  threads. Cheap to `open`. Not usable for most operations
  directly.
- `gix::Repository` — `Send` but not `Sync`. What you actually
  use. Created via `ThreadSafeRepository::to_thread_local()`.

The pattern for rayon-parallel shard ingest:

```rust
// Outside the parallel section:
let shards: Vec<gix::ThreadSafeRepository> =
    discover_shards(root)?.into_iter()
        .map(|p| gix::ThreadSafeRepository::open(p))
        .collect::<Result<_, _>>()?;

// Inside par_iter:
shards.par_iter().try_for_each(|ts_repo| -> crate::Result<()> {
    let repo = ts_repo.to_thread_local();    // cheap, per-worker
    walk_shard(&repo)
})?;
```

Never call `to_thread_local()` outside the worker, then pass
the `Repository` into the closure. That fails to compile
(`Repository: !Sync`) — which is the point.

See `../libraries/gix.md` for the full walk pattern.

---

## No async in Rust

Stated positively: our Rust crate has no `async fn`, no
`.await`, no `Future` types. If you're tempted to write `async
fn`, stop and ask:

1. Is this I/O-bound work that the Python side should own?
   (Usually yes — for anything that hits the network.)
2. Is this CPU-bound work that rayon can parallelize? (Use
   rayon.)
3. Is this a single file-lock operation that just needs to
   complete before we move on? (Synchronous `fs2::FileExt`.)

Stated negatively, to pre-empt common questions:

- **Parquet async reader?** No. `parquet` 58 supports async
  via `object_store`, but we're on local disk and the sync
  reader is fine. Keep the crate's `async` feature for the
  Python-side column extraction path if we ever need it.
- **Long-running query with progress reporting?** No. Cap
  wall-clock at 5s (`router.rs`), fail fast.
- **Concurrent writes to multiple index tiers?** Sequential
  after the parallel ingest phase. `bump_generation` happens
  once, at the end, after all tiers are durable.

---

## Interaction with the GIL

Every `#[pyfunction]` detaches the GIL before any non-trivial
work via `py.detach(|| { ... })`. (In pyo3 0.28 this is the
current spelling; it was `allow_threads` in 0.24, renamed in
PRs #5209 / #5221. Our code uses `detach`.)

Inside the detach closure, we are holding *no Python lock*. We
can:

- Spawn rayon work.
- Acquire file locks.
- Block on disk I/O.

We cannot:

- Touch any `Py<T>` or `Bound<'py, T>`. Those require the GIL.
  The closure's return type must be pure Rust.

The thin wrapper unpacks Python inputs to Rust types before
calling detach, and packs Rust outputs into Python types after
detach. See `../libraries/pyo3.md`.

---

## Concurrency anti-patterns

| Anti-pattern | Why | Fix |
|---|---|---|
| `tokio::runtime::Runtime::new().block_on(async { ... })` inside a `#[pyfunction]` | Two runtimes, no async needed. | Synchronous rayon + file locks. |
| `Arc<Mutex<T>>` shared across rayon tasks for a hot path | Mutex contention serializes the pool. | Partition the data. Per-worker accumulators. Merge at the end. |
| `thread::spawn` inside a rayon task | Nested pools. | `rayon::spawn` or plain `par_iter`. |
| Holding the GIL across a `rayon::par_iter` | Every worker tries to take the GIL back to send results. Deadlocks or serializes. | Detach first, parallelize inside, return after. |
| Calling into Python from a rayon worker | Requires reacquiring the GIL per worker. Kills throughput. | Emit Rust values; convert once in the wrapper. |
| Two `IndexWriter` instances | Tantivy lock + our lockfile both block, but the failure mode is confusing. | One writer, in the ingest process. Readers only elsewhere. |

---

## Summary

| Decision | Answer |
|---|---|
| rayon or tokio? | rayon, always. |
| async fn? | No. |
| `std::thread`? | Only for a single lifetime-bound task (writer lock holder). |
| Send/Sync audit? | Static `assert_send_sync::<T>()` in tests. |
| Shared index writer? | One per system, flock-enforced. |
| GIL handling? | `Python::detach` before anything non-trivial. |

See also:
- `boundaries.md` — pure vs glue; why no HTTP in Rust.
- `../libraries/pyo3.md` — detach/attach semantics.
- `../libraries/gix.md` — ThreadSafeRepository pattern.
- `../libraries/tantivy.md` — reader reload discipline.
