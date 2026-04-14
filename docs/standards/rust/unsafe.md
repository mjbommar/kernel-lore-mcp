# Unsafe Rust — Last Resort

Rust-specific (no Python parallel).

**Our policy: no `unsafe` code in kernel-lore-mcp.** Every current
and near-term feature is achievable in safe Rust with our current
dependency set (`tantivy`, `gix`, `roaring`, `fst`, `arrow`,
`parquet`, `mail-parser`, `regex-automata`, `bytes`).

If you believe you need `unsafe`, stop. Read this file. Then open a
discussion — `unsafe` in this crate is a project-level decision, not
a per-PR call.

---

## Why so strict

1. **Ingestion runs unattended.** A memory safety bug in the Rust
   core corrupts lore data silently or crashes a systemd unit at 3am.
2. **The Rust core is reached from Python.** An undefined-behavior
   bug on the Rust side can corrupt Python interpreter state — at
   which point every stack trace is a lie.
3. **Free-threaded Python (3.14t) sharpens the blade.** Any data
   race in `unsafe` code that was covered by the GIL becomes real
   UB without it. See [`ffi.md`](ffi.md).
4. **Our deps already earn their keep.** `tantivy`, `gix`, `roaring`,
   `fst`, `arrow`, `parquet` — the ones that need `unsafe` for SIMD,
   mmap, or zero-copy parsing already have it, audited and tested
   upstream. We do NOT need to re-implement that.

---

## The bar for introducing `unsafe`

All must be true:

1. **A named, measured performance bottleneck** that safe Rust
   cannot meet. Measured with `criterion` or `flamegraph`, numbers
   committed. "I think this will be faster" is not a reason.
2. **No safe alternative exists**, including:
   - `Arc`, `Mutex`, `RwLock`, `parking_lot::{Mutex, RwLock}`
   - `bytes::Bytes` for reference-counted byte slices
   - `roaring::RoaringBitmap` for posting sets
   - `Cow<'_, T>` for conditional ownership
   - standard iterator adapters
3. **The unsafe block is small** — ideally under 20 lines — and
   wrapped by a safe abstraction. Users of the module do not
   transact in `*const T` / `*mut T`.
4. **Every invariant is written up.** See the `SAFETY:` comment rule
   below.
5. **CI runs miri on the affected tests.** Non-negotiable.
6. **A reviewer who is not the author approves it.** A fresh set of
   eyes catches the invariant the author missed.

Opening `unsafe` as a foot-gun for "well, here's how I'd do it if we
were pure Rust" is not acceptable. We are not pure Rust — we are
middleware that a Python process loads, and safety budgets compound
across that boundary.

---

## Prefer safe primitives

Reach for these before considering `unsafe`:

| Need | Safe primitive |
|------|----------------|
| Shared ownership across threads | `std::sync::Arc<T>` |
| Exclusive access across threads | `std::sync::Mutex<T>` / `parking_lot::Mutex<T>` |
| Shared-read / exclusive-write | `parking_lot::RwLock<T>` |
| Interior mutability single-threaded | `std::cell::RefCell<T>` (never in `#[pyclass]`) |
| Zero-copy byte slices | `bytes::Bytes` (already a dep) |
| Shared read-only buffer | `Arc<[u8]>` or `Arc<str>` |
| Ref-counted string | `Arc<str>` > `Arc<String>` |
| Large bitmap | `roaring::RoaringBitmap` |
| Lazy initialization | `std::sync::OnceLock`, `once_cell::sync::Lazy` |
| Atomic counters | `std::sync::atomic::*` |
| Byte searching | `memchr` (already a dep) |
| UTF-8 decode / validation | `std::str::from_utf8` |

`parking_lot` is not in our deps today. Add it only when a mutex
contention profile motivates it — `std::sync::Mutex` is sufficient
for our current workload.

---

## If unsafe is unavoidable

These are the rules. Not suggestions.

### `SAFETY:` comments are mandatory

Every `unsafe` block and every `unsafe fn` gets a `// SAFETY:`
comment immediately above, stating:

1. **Which invariants** the unsafe code relies on.
2. **Why each invariant holds** at this call site.
3. **What would break** if the invariants failed.

```rust
// SAFETY:
// - `ptr` originates from a `Vec<u8>` that is live for 'a
//   (tied to `&self.buffer` one line up).
// - `len` equals `buffer.len()`, so the slice is in-bounds.
// - The buffer is only read here, never written while this slice
//   is alive — enforced by taking `&self` above.
// If `buffer` were replaced, this slice would dangle.
let slice: &'a [u8] = unsafe { std::slice::from_raw_parts(ptr, len) };
```

Reviewers: if a `SAFETY:` comment is missing, vague ("obviously
safe"), or refers to invariants that aren't actually enforced, NAK
the PR. No exceptions.

### Scope the unsafe block as narrowly as possible

Wrap only the minimum. Not entire functions, not entire blocks.

```rust
// Good
fn first_byte(buf: &[u8]) -> Option<u8> {
    if buf.is_empty() { return None; }
    // SAFETY: bounds checked above; buf is non-empty.
    Some(unsafe { *buf.get_unchecked(0) })
}

// Bad
unsafe fn first_byte(buf: &[u8]) -> Option<u8> {
    if buf.is_empty() { return None; }
    Some(*buf.get_unchecked(0))
}
```

### Never `transmute` without a written-up reason

`std::mem::transmute` is the sharpest edge in the language.
Acceptable reasons are narrow:

- **Layout-compatible** reinterpret of a `#[repr(C)]` type or
  primitive (e.g., `u32` <-> `[u8; 4]`). Prefer
  `u32::from_ne_bytes` / `to_ne_bytes` or
  `bytemuck::cast_slice` over raw transmute.
- **Lifetime extension** that is actually sound. Usually you wanted
  an ownership change instead.

Whenever you reach for `transmute`, ask if `bytemuck::cast` (safe,
compiles to the same code for Plain Old Data) or an explicit
`from_ne_bytes` handles it. They usually do.

If raw `transmute` is genuinely the answer, the PR must include:

1. The `SAFETY:` comment with layout / lifetime justification.
2. A miri test case that exercises the path.
3. An explicit reviewer approval calling out the transmute.

### Run miri on affected tests

Miri is the interpreter that catches UB at runtime. It's slow; use
it on the specific tests that touch unsafe code:

```bash
rustup toolchain install nightly
rustup +nightly component add miri

# Run all tests under miri (slow)
cargo +nightly miri test

# Target one test
cargo +nightly miri test unsafe_slice_path
```

CI wires this in only when unsafe code lands. If we ever add
unsafe, a new job runs `cargo +nightly miri test` on the affected
module.

### Document the invariant-preservation points

When `unsafe` relies on an invariant maintained elsewhere in the
code, the maintaining site also gets a comment:

```rust
// src/store.rs
pub struct Buffer {
    // INVARIANT: `len <= data.len()` at all times.
    // Violating this breaks `unsafe` in src/store.rs::view().
    data: Vec<u8>,
    len: usize,
}
```

Grep-ability matters. `grep -n "INVARIANT" src/` should surface every
invariant the crate relies on.

---

## Interactions with PyO3 / free-threaded Python

See [`ffi.md`](ffi.md) for the full boundary rules. For unsafe
specifically:

- **Any `#[pyclass]` that will be `Sync`** (required for free-threaded)
  and uses unsafe interior mutation needs *both* miri coverage AND
  a Loom model or a stress test. Don't ship a hand-rolled lock-free
  structure in this crate.
- **Never cross `Python::detach` with raw pointers** into Python
  objects. When the GIL / attachment is released, Python can
  move or deallocate. Use `Py<T>` / `Bound<'py, T>` per
  [`ffi.md`](ffi.md) — and don't carry raw refs across detach.
- **`unsafe impl Send for X` / `unsafe impl Sync for X`** is subject
  to the full bar above. The most common mistake is marking
  something `Send` when it wraps a `*const T` that was, in truth,
  aliased from Python-managed memory.

---

## External crates that contain `unsafe`

Our runtime dependencies contain plenty of internal `unsafe` — SIMD
(`memchr`), mmap (`tantivy` via `mmap`), zero-copy buffer parsing
(`bytes`, `arrow`, `parquet`), compression (`zstd`), etc. We rely on
those crates' safe public APIs.

This is fine. The rule is about *our* code.

If you add a new dependency that exposes an `unsafe` function in its
public API, reaching for that function falls under the rules in this
file. Most of the time, a different crate or a different part of the
same crate has a safe equivalent.

---

## Escape valves — when unsafe is *acceptable*

Narrow cases this project may, in the future, allow:

- **SIMD intrinsics** for tokenizer hot paths, AFTER a safe
  implementation has been benched and shown to be the bottleneck.
- **Arena allocation** for short-lived parse trees, AFTER a
  measurement showing allocator pressure is the bottleneck.
- **mmap wrapper** — if we grow our own mmap-based data structure
  (unlikely; tantivy + parquet already do this well).

None of these are in scope today.

---

## Checklist — reviewing an unsafe PR

If an `unsafe` PR ever shows up, the reviewer checks:

- [ ] Is there a measured, documented reason? (criterion output in
      commit body)
- [ ] Is the safe alternative genuinely unavailable? (proved by
      having tried it)
- [ ] Is the `unsafe` block minimal and wrapped by a safe function?
- [ ] Does every `unsafe` block have a `SAFETY:` comment that is
      correct, specific, and refers to enforced invariants?
- [ ] Are corresponding invariant-maintaining sites marked with
      `INVARIANT:` comments?
- [ ] Does CI run miri on this module?
- [ ] Has a second reviewer who is not the author signed off?
- [ ] If `transmute` is used, is it documented and miri-tested?
- [ ] Is `#[allow(unsafe_code)]` scoped narrowly, not at crate
      root?

One "no" and the PR doesn't merge.

---

## Cross-references

- [`index.md`](index.md) — standards index.
- [`language.md`](language.md) — the "unsafe is last resort"
  summary.
- [`code-quality.md`](code-quality.md) — miri integration
  requirements when unsafe lands.
- [`ffi.md`](ffi.md) — PyO3 boundary rules; why unsafe is
  especially costly at the boundary.
- [`../../../CLAUDE.md`](../../../CLAUDE.md) — project
  proscriptions.
