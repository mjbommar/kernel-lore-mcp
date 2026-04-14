# Data Structures — Rust

Rust-specific (no Python parallel). Python's guides cover
pydantic models and dataclasses; Rust has a richer pointer and
container vocabulary, and choosing wrong has real perf and
safety costs.

---

## Ownership vocabulary

### `Vec<T>` — default

Owned, contiguous, heap-allocated, growable. If you aren't sure,
this is the answer. Every tier's in-memory accumulator (pending
`RecordBatch`, pending posting entries, pending writer docs) is
a `Vec<T>` of the natural element type. Don't reach for
fancier containers without a specific need.

### `Box<T>` — single-owner heap

Indirection with one owner. We use it sparingly:

- To break a large recursive enum variant (avoids blowing up
  the enum's stack size).
- To put a trait object behind a known owner (`Box<dyn Trait>`).
- Rarely, to move a very large struct into heap so it doesn't
  blow the stack.

Not needed for "I want to heap-allocate this" — `Vec`, `String`,
and `Arc<T>` already heap-allocate.

### `Arc<T>` — shared across threads

Atomic reference count. Use when:

- Shared across rayon tasks.
- Shared across the PyO3 boundary via `#[pyclass]` (every
  pyclass is effectively `Arc`-shared on the Python side).
- Multiple owners, unknown lifetimes at compile time.

`Arc<T>` is `Send + Sync` iff `T: Send + Sync`. Our
`Arc<fst::Map<Vec<u8>>>` qualifies; the built FST is read-only
and safe to share.

### `Rc<T>` — we don't use it

Single-threaded reference count. We have no single-threaded
code that also needs runtime-dynamic ownership. If you're
reaching for `Rc<T>`, check whether you need the ownership at
all (usually `&T` + lifetime works) or whether it should be
`Arc<T>` because you'll want to parallelize later.

### `Cow<'a, T>` — borrowed-or-owned

`Cow<'a, str>` and `Cow<'a, [u8]>` matter on hot paths where
most values come through unmodified but some need a per-call
allocation:

- Subject normalization: most subjects pass through; a few need
  tag-stripping. `Cow<'_, str>` lets us return the borrow when
  nothing changed.
- Quoted-reply stripping in prose: most messages are unquoted;
  strip only when `^> ` is present.

Decision rule: reach for `Cow` when the "no-op" case is >50%
of calls and the allocation is non-trivial (>100 bytes). For
small strings, just own.

### `&T` with explicit lifetime — preferred for function params

A function that reads but doesn't own takes `&T` (or `&[T]`,
`&str`, `&Path`). Don't take `Vec<T>` or `String` unless you
need to move. Don't take `Arc<T>` unless you need to hold past
the call.

---

## Specialized containers

### `SmallVec<[T; N]>` — usually not worth it

Inline storage for small collections, heap for large. Tempting
for "most thread reference lists are ≤4 items." But:

- `smallvec` is another dep to pin.
- The common ingest path operates on `Vec` via rayon; the
  inline optimization is invisible under mmap'd Parquet.
- Compiler and allocator optimizations for `Vec` have improved
  since smallvec mattered.

When we'd use it: an inner-loop struct that's allocated
millions of times and typically holds 2-4 items (e.g., series
index lookup). Even then, measure first. Today: not in
`Cargo.toml`.

### `bytes::Bytes` — zero-copy slices

Pinned at 1.x in `Cargo.toml`. Use for:

- Passing decompressed message bodies between the store and
  trigram confirmation step. One zstd decompress → `Bytes` →
  slice into prose vs patch halves without copying.
- Any "I have a byte buffer and multiple consumers want views
  into it" pattern.

Key methods:

- `Bytes::slice(range)` — zero-copy slice, shares the underlying
  allocation via Arc.
- `BytesMut` — mutable, single-owner. Freeze to `Bytes` when
  done.

Don't use `Bytes` for every byte slice. If you own the buffer
and there's one consumer, `Vec<u8>` is simpler.

### `HashMap` — `ahash` default

The stdlib `HashMap` uses SipHash (DoS-resistant but slow on
short keys). Our hashes are over `message_id` (already a Git
OID-looking string), `trigram` (u32), and `(list, segment_id)`
tuples — none of them DoS vectors.

Pin `ahash` (or `rustc-hash`) if you need a perf-sensitive map.
We don't have this in `Cargo.toml` today because our hot maps
live inside tantivy and fst, which bring their own hashing.
When we add a large in-memory map on the hot path:

```toml
ahash = "0.8"
```

and

```rust
type FastMap<K, V> = std::collections::HashMap<K, V, ahash::RandomState>;
```

Do not swap to `ahash` globally without benchmark evidence.

### `RoaringBitmap` — posting lists

`roaring` 0.11 is our posting-list container in the trigram
tier. See `../libraries/roaring-fst.md`.

Idioms we use:

- `RoaringBitmap::new()` + `insert(docid)` at build time.
- `RoaringBitmap::from_sorted_iter(iter)` when docids are known
  sorted (faster).
- `a & b` / `a | b` for intersect / union. Out-of-place by
  default; use `&=` / `|=` to reuse the left-hand buffer.
- Serialize via `bitmap.serialize_into(&mut writer)` — portable
  format. Deserialize read-only with
  `RoaringBitmap::deserialize_from(reader)`.

Notes:

- Not `Sync` today. Pass bitmaps by move into rayon workers; if
  two workers need the same bitmap, `Arc<RoaringBitmap>` and
  clone the Arc — the underlying bitmap remains effectively
  immutable under shared ownership.
- `serialize_into` writes the portable format; `serialize`
  writes a slightly larger but identical layout. We use portable
  for on-disk (`trigrams.postings`).

### `fst::Map<D>` — term dictionary

`fst` 0.4. We build from sorted `(trigram_u32_bytes,
posting_offset_u64)` pairs and either load via `Map::new(mmap)`
or own via `Map::from_bytes(Vec<u8>)`.

Construction pattern — trigrams packed as 3 big-endian bytes so
FST iteration order matches numeric order:

```rust
let mut builder = fst::MapBuilder::new(Vec::new())?;
for (trigram, offset) in sorted_pairs {      // MUST be sorted
    let key = trigram.to_be_bytes();         // [u8; 3]; fst wants &[u8]
    builder.insert(&key, offset)?;
}
let fst_bytes = builder.into_inner()?;
```

Reading:

```rust
let mmap = unsafe { memmap2::Mmap::map(&file)? };   // SAFETY: read-only, file outlives map
let fst = fst::Map::new(mmap)?;                     // zero-copy over mmap
```

See `../libraries/roaring-fst.md` for regex-automata →
FST-range-query bridging.

### `memmap2::Mmap` — memory-mapped reads

For large, read-mostly files (FST bytes, posting bitmap
concatenation, segment files), mmap avoids pulling the whole
file into RAM while letting the kernel page-cache-manage it.

```rust
let file = File::open(path)?;
// SAFETY: we treat the mapping as read-only; the file is not
// truncated or modified under us for the lifetime of `mmap`.
let mmap = unsafe { memmap2::Mmap::map(&file)? };
```

Safety contract — documented once here, re-cited in the code:

1. The file must not be truncated or modified while the mmap
   is live. Our segment files are append-only-then-sealed; by
   the time a reader mmaps, the file is frozen.
2. The mmap must be dropped before the file is deleted.
3. Errors from the underlying storage surface as `SIGBUS`, not
   `io::Error`. Operate on trusted disk paths.

---

## Arrow vs `Vec<T>` for in-memory records

Two places this decision recurs:

### Use `arrow::RecordBatch` when

- The data is about to be written to Parquet (metadata tier).
- The schema is stable and centrally defined (`schema.rs`).
- You want predicate pushdown on read.
- You're transferring rows to Python without row-by-row
  conversion (pyarrow zero-copy).

Build one `RecordBatchBuilder`-style function per column, fill
column-by-column, assemble one `RecordBatch` per N thousand
rows, write. See `../libraries/arrow-parquet.md`.

### Use `Vec<T>` when

- Data is transient (per-message intermediate in ingest).
- Schema is per-callsite, not shared.
- Downstream code is a simple Rust consumer (not Parquet, not
  Python).

Common shape:

```rust
// Per-message scratch — Vec
let mut trailers: Vec<Trailer> = extract_trailers(&body);
// Batch to commit — Arrow
let batch: RecordBatch = build_metadata_batch(&pending_rows)?;
```

Don't use Arrow for data that never leaves Rust-land. The
column-builder machinery has real overhead compared to a plain
struct Vec.

---

## `String` vs `&str` vs `Cow<'_, str>`

| Situation | Type |
|---|---|
| Function parameter, read-only | `&str` |
| Function parameter, need to store | `&str` + `.to_owned()` at the one callsite that stores, OR `String` if callers routinely pass ownership. |
| Return value, always owned | `String` |
| Return value, usually borrowed | `&str` with a lifetime tied to input. |
| Return value, sometimes owned | `Cow<'a, str>`. |
| Struct field | `String` (owned). Fields with lifetimes are contagious and we've avoided them. |

Our structs don't carry lifetimes. Hits, Queries, RecordBatches
— all owned. Readability > the last 2% of allocation savings.

---

## Numeric types

- `u32` for docids local to a segment. Matches tantivy's
  internal. Fits 4B docs per segment — enough.
- `u64` for global message identifiers (offsets into Parquet),
  `state::generation`, packed trigram offsets.
- `usize` for lengths and indices into local containers only.
  Don't persist `usize` — it's platform-width.
- `i64` (Arrow timestamp-ns convention) for dates. Convert at
  the edges.

---

## Optional and enum patterns

- **`Option<T>` everywhere a value can be absent.** Don't use
  sentinel values (`-1`, `""`).
- **Enums for finite choices.** Tier dispatch, hit provenance,
  query predicate kind — all enums.
- **`#[non_exhaustive]` on public enums** we expect to grow.
  Forces external matches to have a `_` arm; internal matches
  still fail to compile on missing variants. Today we apply
  this sparingly — `Error` is not `non_exhaustive` because
  `impl From<Error> for PyErr` matches exhaustively.

---

## Pinning decisions

All data-structure crates are pinned in `Cargo.toml`:

| Crate | Version | Role | Breaking history |
|---|---|---|---|
| `roaring` | 0.11 | posting lists | 0.11 removed some deprecated methods; API is stable since 0.10. |
| `fst` | 0.4 | term dict | stable since 0.3; 0.4 added `Stream::next` ergonomics. |
| `bytes` | 1 | zero-copy slices | 1.0 stable since 2020. |
| `memmap2` | ^ (not pinned, audit adds) | mmap wrapper | maintained fork of `memmap`; 0.9 is current. |

Adding a new container crate requires:

1. Benchmark showing it beats the obvious stdlib path.
2. Audit of transitive deps.
3. Entry in `../dependencies.md` (TODO).

---

## Decision flowchart

```
Is this a collection of owned items I'll grow and read?
  -> Vec<T>

Do I need to share across threads with runtime-dynamic ownership?
  -> Arc<T> (or Arc<[T]> for slices)

Do I need to share across threads, read-only, with one clear
producer and many consumers?
  -> Arc<T> or &T with explicit lifetime, depending on whether
     owners are dynamic.

Do I have a large read-mostly file on disk?
  -> memmap2::Mmap, with the SAFETY comment verbatim.

Do I want zero-copy slices of a byte buffer for multiple consumers?
  -> bytes::Bytes.

Am I building a posting list over sorted integer docids?
  -> RoaringBitmap.

Am I building a term dictionary from sorted keys?
  -> fst::Map.

Am I about to write columnar data to Parquet?
  -> arrow::RecordBatch.

Anything else?
  -> Start with Vec<T> / HashMap<K, V>. Justify in review if
     swapping to something specialized.
```

See also:
- `../libraries/roaring-fst.md` — concrete trigram-tier code.
- `../libraries/arrow-parquet.md` — metadata-tier code.
- `../libraries/zstd.md` — compressed store and `Bytes` usage.
- `concurrency.md` — Send/Sync rules for these types.
