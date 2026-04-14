# roaring 0.11 + fst 0.4

Rust-specific (no Python parallel).

These two crates together own the trigram tier. `fst` is the
term dictionary (byte-trigram → posting-offset). `roaring`
stores each posting bitmap. Full tier spec in
`../../../indexing/trigram-tier.md`.

Pinned:

```toml
roaring = "0.11"
fst     = "0.4"
```

---

## Why these two, not tantivy's NgramTokenizer

Rationale from `docs/indexing/trigram-tier.md`:

- No BM25 score computation overhead.
- Roaring postings compress better than tantivy's skiplists
  for the trigram distribution (very skewed: a few trigrams
  are ubiquitous).
- Direct FST over u32-packed trigrams; no string interning.
- FST composes with `regex-automata` DFAs via `fst::Automaton`,
  which is the critical feature for regex queries.

Size budget: ~15–25 GB for all-of-lore patch content. FST
itself is ~50 MB.

---

## Building the term dict from sorted pairs

`fst::MapBuilder` requires keys in sorted byte order. We pack
byte trigrams as 3 big-endian bytes (= u32 with high byte 0);
numeric sort of the u32s equals lex sort of the 3-byte keys.

### Accumulation phase (ingest)

Per-segment build. Segment = one ingestion run. ~100k–1M
messages each.

```rust
use std::collections::BTreeMap;
use roaring::RoaringBitmap;

pub struct SegmentBuilder {
    // trigram (u32, high byte 0) -> docids in this segment (u32 local)
    postings: BTreeMap<u32, RoaringBitmap>,
    next_docid: u32,
    docid_to_mid: Vec<u64>,   // docid -> global message_id
}

impl SegmentBuilder {
    pub fn add_patch(&mut self, message_id: u64, patch_bytes: &[u8]) {
        let docid = self.next_docid;
        self.next_docid += 1;
        self.docid_to_mid.push(message_id);

        // Emit one trigram per overlapping window; skip on non-ASCII.
        for win in patch_bytes.windows(3) {
            if win.iter().any(|&b| b >= 0x80) { continue; }
            let tg = ((win[0] as u32) << 16) | ((win[1] as u32) << 8) | (win[2] as u32);
            self.postings.entry(tg).or_default().insert(docid);
        }
    }
}
```

### Finalize — write FST + postings + docs

```rust
pub fn finalize(self, out_dir: &Path) -> crate::Result<()> {
    std::fs::create_dir_all(out_dir)?;

    let mut postings_file = File::create(out_dir.join("trigrams.postings"))?;
    let mut fst_builder   = fst::MapBuilder::new(
        File::create(out_dir.join("trigrams.fst"))?
    )?;

    for (tg, bitmap) in self.postings {
        let offset = postings_file.stream_position()?;
        bitmap.serialize_into(&mut postings_file)?;  // roaring portable format
        let key = tg.to_be_bytes();                  // [u8; 4]; we use last 3
        fst_builder.insert(&key[1..4], offset)?;
    }

    fst_builder.finish()?;
    postings_file.sync_all()?;

    // docid -> global message_id
    let docs_path = out_dir.join("trigrams.docs");
    let mut f = File::create(&docs_path)?;
    for mid in &self.docid_to_mid {
        f.write_all(&mid.to_le_bytes())?;
    }
    f.sync_all()?;

    // meta.json with counts, segment_id, etc.
    Ok(())
}
```

Notes:

- `BTreeMap` guarantees key-sorted iteration, which `fst::MapBuilder`
  requires. If you use `HashMap`, you must sort before inserting.
- **Roaring portable format** via `serialize_into`. Compatible
  across language bindings. We record byte offsets into the
  concatenated file; the FST value is the offset.
- Segments are **immutable** once finalized. Incremental = new
  segment. Compaction merges segments monthly.

---

## Reading — mmap'd FST, mmap'd postings

```rust
pub struct SegmentReader {
    fst: fst::Map<memmap2::Mmap>,
    postings: memmap2::Mmap,
    docid_to_mid: Vec<u64>,   // small; read fully
}

impl SegmentReader {
    pub fn open(dir: &Path) -> crate::Result<Self> {
        let fst_file = File::open(dir.join("trigrams.fst"))?;
        // SAFETY: files are immutable after finalize; no concurrent writers.
        let fst_mmap = unsafe { memmap2::Mmap::map(&fst_file)? };
        let fst = fst::Map::new(fst_mmap)?;

        let post_file = File::open(dir.join("trigrams.postings"))?;
        let postings  = unsafe { memmap2::Mmap::map(&post_file)? };

        let docs = read_u64_vec(&dir.join("trigrams.docs"))?;

        Ok(Self { fst, postings, docid_to_mid: docs })
    }

    pub fn lookup_trigram(&self, tg: u32) -> crate::Result<Option<RoaringBitmap>> {
        let key = tg.to_be_bytes();
        let Some(offset) = self.fst.get(&key[1..4]) else { return Ok(None) };
        let offset = offset as usize;
        // Roaring figures out its own length from the portable header.
        let bitmap = RoaringBitmap::deserialize_from(&self.postings[offset..])?;
        Ok(Some(bitmap))
    }
}
```

Safety contract on the mmap — documented once in
`../design/data-structures.md`, re-cited here: segment files
are immutable after `finalize`, there's no writer mutating them
under a reader.

---

## Intersection / union patterns

Query → required trigram set (from literal or regex analysis).
Result = intersection of all required posting bitmaps.

```rust
pub fn query_trigrams(
    reader: &SegmentReader,
    required: &[u32],
) -> crate::Result<RoaringBitmap> {
    let mut iter = required.iter();
    let Some(&first) = iter.next() else {
        return Ok(RoaringBitmap::new());  // empty query -> no hits
    };
    let mut acc = reader.lookup_trigram(first)?
        .unwrap_or_else(RoaringBitmap::new);

    for &tg in iter {
        if acc.is_empty() { break; }
        let b = reader.lookup_trigram(tg)?.unwrap_or_else(RoaringBitmap::new);
        acc &= b;   // in-place intersect
    }
    Ok(acc)
}
```

For multi-segment queries (one reader per segment in a list):

```rust
let mut union_of_candidates = RoaringBitmap::new();
for segment in &readers {
    let hits = query_trigrams(segment, &required)?;
    // Translate local docids -> global message_ids before union
    // so we don't collide across segments.
    for docid in hits {
        union_of_candidates.insert_global(segment.docid_to_mid[docid as usize]);
    }
}
```

(In practice `message_id` is too large for a `RoaringBitmap<u32>` —
we use a `Vec<u64>` or `RoaringTreemap` for the global set. See
`roaring` docs for `RoaringTreemap` if we adopt it.)

---

## Regex → FST bridge via `regex-automata`

The load-bearing feature of `fst`: it can stream keys matching
a DFA-like `Automaton`. `regex-automata`'s `DFA` implements
`fst::Automaton` (via an adapter — details in
`regex-automata.md`).

Pattern:

```rust
// See regex-automata.md for `compile_dfa`.
let dfa = compile_dfa(pattern)?;
let stream = reader.fst.search(&DfaAsFstAutomaton(&dfa)).into_stream();

while let Some((trigram_key, offset)) = stream.next() {
    // trigram_key is &[u8] of length 3
    // offset is u64 into postings; dereference and union into acc.
}
```

This is how unanchored regex searches stay tractable: the FST
walk only visits trigram keys that the DFA can match. Without
this bridge, a full-FST scan would be a regression over naive
grep.

**Caveat**: the required set for a pattern like `/foo.*bar/`
is `{foo, bar}`; we extract those trigrams and intersect their
postings rather than enumerate FST keys. FST range-scan only
helps when the *set* of matching trigrams is large but the DFA
can enumerate it. Details in `regex-automata.md`.

---

## Roaring specifics

### Container choice is automatic

Roaring picks between array (< 4096 entries), bitmap (≥ 4096,
< ~32k), and run-length (long runs) per 64k chunk. Don't
micro-manage. Call `bitmap.optimize()` after a bulk build if
you want the crate to re-pick containers with full info.

### Iteration

- `bitmap.iter()` — ascending docids, `impl Iterator<Item = u32>`.
- `bitmap.len()` — O(chunks), fast.
- `bitmap.is_empty()` — faster than `len() == 0` (short-circuits
  on first non-empty chunk).

### Serialization

- **Portable format** (`serialize_into` / `deserialize_from`) —
  matches roaring-java, roaring-c. On-disk standard.
- **Native format** — slightly faster but incompatible. We
  don't use it.

### Send/Sync

`RoaringBitmap: Send`, **not `Sync`**. For cross-thread sharing
read-only, wrap in `Arc<RoaringBitmap>` and never mutate. See
`../design/data-structures.md`.

### Version 0.11 notes

- 0.10 → 0.11 removed a few long-deprecated methods; our code
  uses current idioms.
- `insert(u32)` → returns `bool` (was the old API in some
  versions).
- `|` / `&` / `-` operators create new bitmaps; `|=` / `&=` /
  `-=` mutate in place. Prefer in-place for large bitmaps.
- `BitOrAssign<&RoaringBitmap>` is implemented — you can `|=`
  from a borrow without cloning.

---

## FST specifics

### `fst::Map` vs `fst::Set`

- `Set<D>` — just keys, no values. ~10% smaller for the same
  keys.
- `Map<D>` — keys + u64 values. Our posting-offset usage
  requires `Map`.

### `D` generic

The backing data type. We use two:

- `fst::Map<memmap2::Mmap>` for on-disk mmap.
- `fst::Map<Vec<u8>>` for small in-memory dicts (tests).

`Map<&[u8]>` (a borrow) is also valid for lifetime-bound reads.

### Ordering

FST demands sorted insertion. BTreeMap + iterate is the safe
pattern. If you must use `HashMap`, call
`.keys().collect::<Vec<_>>().sort()` before insertion.

### `Stream` API

Streams yield `(&[u8], u64)`. They are not `Iterator` — they
use their own `Streamer` trait (`next()` returns
`Option<...>` with a borrow tied to the stream's lifetime).
This is how FST avoids allocating per key.

```rust
let mut stream = fst.stream();
while let Some((k, v)) = stream.next() {
    // `k: &[u8]`, `v: u64`, both borrowed from the stream.
}
```

---

## Don't-do list

| Anti-pattern | Why |
|---|---|
| `HashMap` for trigram accumulation without a sort before FST build | Fails `MapBuilder::insert` ordering invariant. |
| Using roaring's native (non-portable) format on disk | Forward-compat hazard. |
| Mutating a `Mmap`-backed FST | Undefined behavior. FSTs are read-only after build. |
| Storing absolute segment offsets as u32 | Postings file can exceed 4 GB. u64. |
| Indexing non-ASCII bytes into trigrams in v1 | Breaks window semantics; query side is strict ASCII. |
| Forgetting to `sync_all()` on postings before writing FST | FST references offsets that may not be durable. Crash-recovery hole. |

---

## Checklist for a trigram change

1. Segment build still uses `BTreeMap` or an explicit sort
   before `MapBuilder::insert`.
2. Postings written in roaring **portable** format.
3. `trigrams.postings` fsync'd before `trigrams.fst` is
   finalized.
4. Reader opens via mmap with the SAFETY comment.
5. Local docid → global `message_id` translation happens at
   query-time union, not during per-segment lookup.
6. Regex path has a DFA-reject fallback (see
   `regex-automata.md`).

See also:
- `../../../indexing/trigram-tier.md` — full tier spec.
- `regex-automata.md` — DFA-only discipline and FST bridge.
- `../design/data-structures.md` — SAFETY contract for mmap.
- `zstd.md` — candidate confirmation reads from the store.
