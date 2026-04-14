# Indexing — trigram tier

Zoekt-style. Indexes patch/diff content only. Answers regex,
substring, and identifier-fragment queries over code.

## Why not tantivy's NgramTokenizer

NgramTokenizer produces BM25 postings (with frequencies). We don't
care about BM25 ranking on patches — we want presence / absence of
byte trigrams. Separate implementation is ~400 LOC and gives us:

- No score computation overhead.
- Roaring-bitmap posting lists (vs tantivy's custom skiplists).
- Direct FST term dict over u16-keyed trigrams (no string interning).
- Regex → automaton → FST range queries, with docid-bitmap
  intersection.

## On-disk layout

One trigram index per (list, segment). Segment = one ingestion
run's worth of messages (~100k-1M typically).

```
<data_dir>/trigram/<list>/<segment_id>/
    trigrams.fst           # fst::Map: trigram (u32 packed 3 bytes) -> postings_offset
    trigrams.postings      # concatenated serialized roaring bitmaps
    trigrams.docs          # docid (u32 local) -> global message_id (u64)
    meta.json              # segment metadata
```

Segments are immutable. A compaction pass merges them later.

## Trigram encoding

A byte trigram is 3 bytes = 24 bits, encoded as a u32 (high byte 0).
Kernel patches are effectively ASCII; we don't emit trigrams
containing bytes >= 0x80 in v1 (any non-ASCII byte breaks the
window; query-time handling is strict-ASCII regex only).

## Query path

1. Parse the query. Literal substring → every overlapping byte
   trigram. Regex → analyze via `regex-automata` DFA; reject any
   pattern that doesn't compile to a DFA.
2. For each required trigram, FST lookup → roaring posting bitmap.
3. Intersect posting bitmaps (ns per doc).
4. **Cap the candidate set** at `TRIGRAM_CONFIRM_LIMIT` (initial
   value 4096; tuned so p95 confirmation stays under 500 ms on the
   reference box). When the cap kicks in, the router returns
   partial results with `truncated_by_candidate_cap: true` so the
   LLM caller knows.
5. For each surviving docid, **decompress the patch body from the
   compressed store** (one zstd frame per message, random-access by
   `(offset, length)` — see `docs/indexing/compressed-store.md`)
   and re-run the real DFA against it. Confirmation cost is
   dominated by zstd decompression, not regex; zstd-dict decoding
   is ~1 GB/s of decompressed throughput on modern CPUs.
6. Return confirmed hits: `message_id` + `tier_provenance: ["trigram"]`
   + `is_exact_match: true`.

### Why not mmap'd uncompressed patches

The v0 draft said "mmap the raw patch bytes." We can't: the store
is zstd-compressed per message. Options considered:
 - **Chosen**: decompress-per-candidate with `TRIGRAM_CONFIRM_LIMIT`
   cap. Simpler, no extra disk.
 - Rejected: maintain a parallel *uncompressed* mmap of patches.
   Would add 15–25 GB and double the write path.
 - Rejected: accept trigram-only matches without confirmation.
   False positives are real; we refuse to return them.

## Compression

Roaring bitmaps auto-pick between array, bitmap, and RLE
containers. Expected on-disk size for all-of-lore patch content:
~15–25 GB. That's dominated by a handful of extremely common
trigrams (` fo`, `for`, `or `, newline runs, etc.); the FST term
dict itself is tiny (~50 MB).

## Incremental updates

Ingestion runs produce new segments. Existing segments are
immutable. Query reads all segments per list and unions. Monthly
`compact` pass merges segments within a list.

## Limits

- Strict ASCII trigrams in v1. Queries with non-ASCII bytes return
  empty.
- Regex queries with leading `.*` over a large term-dict can blow
  up; the query router caps unanchored regex search to specific
  lists or time ranges.
- Trigram hit ⇒ confirm with real regex. Never return a
  trigram-only match; false positives are real.
