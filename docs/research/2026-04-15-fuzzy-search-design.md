# Research -- Fuzzy (Levenshtein) Search via Trigram Prefilter (April 15 2026)

## Decision

**Go.** Add `triple_accel` 0.4.0 (MIT, zero deps, SIMD-accelerated
Levenshtein substring search on `&[u8]`). At k=1 the existing
trigram prefilter is valid unmodified; at k=2 it is valid for
needles >= 8 bytes. The 5-second wall-clock budget is comfortably
met for both k=1 and k=2 at realistic candidate counts.

---

## 1. Crate evaluation

### StringZilla (`stringzilla` 4.6.0, Apache-2.0)

Rust crate exists on crates.io. Exposes `LevenshteinDistances`
engine for full-string edit distance. However, it does NOT expose
**fuzzy substring search** (finding a window of ~len(needle) in a
longer haystack within edit distance k). Our use case requires
substring-level matching over 5-20 KB bodies, not whole-string
comparison. The Rust bindings are also poorly documented (77%
coverage) and the API surface is thin compared to the Python/C++
side. **Rejected: wrong operation (full-string, not substring).**

### `strsim` 0.11.1 (MIT) -- already a transitive dependency

Scalar Levenshtein on `&str` (generic variants exist for
iterators). No SIMD. No substring search. Full-string only.
**Rejected: scalar, no substring search.**

### `rapidfuzz` 0.5.0 (MIT)

Port of the Python rapidfuzz library. Exposes Levenshtein distance
on byte slices. Has a `fuzz` module but it implements ratio-style
scoring, not positional substring search with edit-distance-bounded
window matching. No documented SIMD acceleration. **Rejected: no
true fuzzy substring search; immature Rust port.**

### `edit-distance` (various versions)

Minimal crate, scalar, `&str` only, full-string Levenshtein.
**Rejected.**

### `triple_accel` 0.4.0 (MIT) -- SELECTED

Purpose-built for exactly our use case. Key properties:

- **SIMD-accelerated**: AVX2 (256-bit), SSE4.1 (128-bit), scalar
  fallback. Uses anti-diagonal DP matrix vectorization.
- **Operates on `&[u8]`**: byte-native, no UTF-8 requirement.
- **Fuzzy substring search**: `levenshtein_search_simd_with_opts()`
  returns an iterator of `Match { start, end, k }` over all
  positions in the haystack where a substring is within edit
  distance k of the needle.
- **Configurable k threshold**: the `k: u32` parameter caps the
  maximum edit distance for returned matches.
- **Zero dependencies** (only dev-deps: criterion, rand).

Signature of the primary function:

```rust
pub fn levenshtein_search_simd_with_opts<'a>(
    needle: &'a [u8],
    haystack: &'a [u8],
    k: u32,
    search_type: SearchType,  // All or Best
    costs: EditCosts,         // custom ins/del/sub costs
    anchored: bool,           // anchor to start of text
) -> Box<dyn Iterator<Item = Match> + 'a>
```

Additional useful functions:
- `levenshtein_simd_k(a, b, k)` -- full-string with early exit
- `levenshtein_search(needle, haystack)` -- auto-threshold at
  len/2

**Recommendation: `triple_accel = "0.4"` in `[dependencies]`.**

---

## 2. Current trigram prefilter (from `src/trigram.rs`)

### Trigram generation

`patch_trigrams(patch)` iterates every overlapping 3-byte window in
the patch body, **skipping windows containing any non-ASCII byte
(>= 0x80)**. Each window is packed into a `u32` via
`pack_trigram([b0, b1, b2]) = (b0 << 16) | (b1 << 8) | b2`.

### Candidate set production

`candidate_docids(needle)` enumerates all distinct ASCII trigrams
from the needle, then **intersects** the corresponding Roaring
posting lists from the FST-indexed segment. If any trigram has no
posting list, the result is empty (short-circuit). The intersection
is progressive: after each AND, if the bitmap is empty, bail early.

### Confirmation

`candidates_for_substring(needle)` returns at most
`TRIGRAM_CONFIRM_LIMIT = 4096` candidate message-IDs from the
bitmap. The caller (`Reader::patch_search` in `src/reader.rs`)
then:
1. Looks up each candidate in the metadata tier.
2. Opens the compressed store for the candidate's list.
3. Decompresses the body via `store.read_at(segment_id, offset)`
   (zstd level 19, single-frame streaming decode).
4. Runs `memchr::memmem::find(&body, needle_bytes)` for exact
   substring confirmation.
5. Collects confirmed hits, sorts newest-first, truncates to
   `limit`.

### Candidate cap

`TRIGRAM_CONFIRM_LIMIT = 4096` per segment. Multiple segments are
unioned into a `HashSet<String>` without a global cap (the metadata
scan + store decompression is the real bottleneck).

---

## 3. Pigeonhole math for edit distance k

### Trigram destruction per edit

A single character edit (insertion, deletion, or substitution)
destroys at most **3 trigrams** in a string of length L (the
3-byte windows that overlap the edit point). Therefore a string at
edit distance k from a needle of length L shares at least:

```
surviving_trigrams >= max(0, (L - 2) - 3*k)
```

where `(L - 2)` is the total number of trigrams in a string of
length L.

### k=1 analysis

At k=1, surviving trigrams >= `L - 2 - 3 = L - 5`. For L >= 6
(which produces >= 1 surviving trigram), the existing trigram
prefilter with full intersection is valid. Since we require all
needle trigrams to be present, we are actually being MORE selective
than necessary -- we might miss some true k=1 matches where the
edit destroys 1-3 trigrams that we required.

**Correct approach for k=1**: require that the candidate matches at
least `(L - 2) - 3` of the needle's trigrams (i.e., allow up to 3
trigram misses). In practice, this means changing the posting-list
combination from pure AND (intersection) to a **threshold
intersection**: a candidate must appear in at least
`max(1, num_trigrams - 3*k)` of the posting lists.

However, there is a simpler approach: **keep the existing full
intersection as a first pass** (it finds all exact matches plus
some near-misses where the edit did not destroy a required
trigram), then **also union posting lists for the dropped-trigram
subsets**. But this is complex.

**Simplest valid approach**: at k=1, intersect ALL trigrams of the
original needle (same as today). This gives a **subset** of the
true k=1 candidates (it misses candidates where the edit destroyed
a required trigram). Then at confirmation time, use
`levenshtein_search_simd_with_opts` with k=1 instead of
`memmem::find`. This gives **recall < 100% but precision = 100%**
for k=1.

For **full recall at k=1**, use threshold intersection: require
`max(1, num_trigrams - 3)` matching trigrams. This widens the
candidate set but is still very selective for needles >= 10 chars.

### k=2 analysis

Surviving trigrams >= `L - 2 - 6 = L - 8`. Valid prefilter when
L >= 10 (>= 2 surviving trigrams, enough for meaningful
filtering). Below L=10, the prefilter becomes vacuous at k=2 (0 or
1 surviving trigrams means we must scan everything).

For full recall at k=2: require `max(1, num_trigrams - 6)` matching
trigrams.

### Worked example: 20-character needle

```
L = 20
num_trigrams = L - 2 = 18

k=1: require >= 18 - 3  = 15 matching trigrams (miss up to 3)
k=2: require >= 18 - 6  = 12 matching trigrams (miss up to 6)

Full intersection (current): requires all 18. At k=1 this catches
  only edits that happen to not destroy any of the 18 trigrams in
  the needle -- unlikely but possible (e.g., appending a char adds
  a trigram but doesn't destroy existing ones).
```

Candidate count estimate: with threshold intersection at
`num_trigrams - 3*k`, the candidate set grows roughly proportional
to the number of trigram-combinations we must test. For a 20-char
needle on a 40k-message shard:

- Exact (current): ~5-50 candidates per shard.
- k=1 threshold: ~50-500 candidates per shard (10x widening).
- k=2 threshold: ~200-2000 candidates per shard (another 4x).

On the full 8M corpus (~200 shards): multiply by ~200, so k=1
yields ~10k-100k candidates globally, k=2 yields ~40k-400k. These
numbers are upper bounds; the metadata-tier join and
`TRIGRAM_CONFIRM_LIMIT` cap them in practice.

---

## 4. Wall-clock budget analysis

### Parameters

| Parameter | Value |
|---|---|
| Wall-clock cap | 5000 ms |
| Candidate count (k=1, realistic) | 500-5000 |
| Candidate count (k=2, realistic) | 2000-20000 |
| Avg body size compressed | 2-8 KB |
| Avg body size decompressed | 5-20 KB |
| Avg decompressed body | ~15 KB (used for calc) |
| zstd decompress throughput | ~2 GB/s (level 19 decode) |
| `triple_accel` SIMD throughput | ~1-5 GB/s (AVX2 Lev search) |
| `memmem` throughput (current) | ~10-20 GB/s |

### Decomposition of per-candidate cost

Per candidate:
1. **Metadata lookup**: ~1 us (in-memory Parquet scan, amortized).
2. **zstd decompress 15 KB**: 15 KB / 2 GB/s = ~7.5 us.
3. **Fuzzy substring scan 15 KB at k=1**: 15 KB / 2 GB/s (conservative for triple_accel) = ~7.5 us.
4. **Total per candidate**: ~16 us.

### Budget calculations

```
5000 candidates x 16 us = 80 ms    << 5000 ms budget.  GO at k=1.
20000 candidates x 16 us = 320 ms  << 5000 ms budget.  GO at k=2.
```

**Breakeven candidate count**:
```
5000 ms / 16 us = 312,500 candidates
```

Even at pessimistic 50 us/candidate (cold disk, large bodies):
```
5000 ms / 50 us = 100,000 candidates
```

This is well above any realistic candidate count from trigram
prefiltering, even at k=2 on the full corpus.

### Comparison with current exact search

Current cost: ~10 us/candidate (zstd + memmem). The fuzzy path
adds ~6 us/candidate (triple_accel vs memmem), a 60% increase in
per-candidate cost but still 2-3 orders of magnitude below the
wall-clock budget.

**Verdict: comfortably within budget at both k=1 and k=2.**

---

## 5. Design sketch

### Integration into the existing search path

The fuzzy search extends `Reader::patch_search` and adds a new
query predicate (e.g., `dfb~1:` or `dfb~2:` for edit distance 1
or 2).

#### Step 1: Trigram prefilter (modified for k >= 1)

**Option A (simple, partial recall)**: Use the existing full
intersection of all needle trigrams. This catches candidates that
happen to contain all trigrams despite being at edit distance k.
Confirmation via `triple_accel` catches the fuzzy matches among
these candidates. Recall < 100% but zero changes to the trigram
tier.

**Option B (full recall, recommended)**: Implement threshold
intersection. Instead of AND-ing all posting lists, count how many
of the needle's trigram posting lists each candidate appears in.
Return candidates appearing in at least `max(1, num_trigrams - 3*k)`
lists. Implementation: sort posting lists by cardinality, intersect
the `threshold` smallest, then check membership in the remaining
lists with early-out.

Recommended: **start with Option A** (ship fast, measure recall
loss on real queries), upgrade to Option B when recall matters.

#### Step 2: Confirmation via fuzzy substring search

Replace:
```rust
// Current: exact substring
memchr::memmem::find(&body, &needle_bytes).is_some()
```

With:
```rust
// Fuzzy: edit-distance-bounded substring search
use triple_accel::levenshtein::levenshtein_search_simd_with_opts;
use triple_accel::{SearchType, EditCosts, Match};

let mut matches = levenshtein_search_simd_with_opts(
    needle_bytes,
    &body,
    k,                    // max edit distance (1 or 2)
    SearchType::Best,     // return only best matches
    EditCosts::default(), // unit costs
    false,                // not anchored
);
matches.next().is_some()  // true if any substring matches
```

#### Step 3: Return enrichment

Each confirmed hit can carry the best match's `Match { start, end,
k }` to report the actual edit distance and highlight the matching
region in the snippet.

### Clarification: fuzzy substring, not whole-body Levenshtein

This is **fuzzy substring search**: find any contiguous window of
approximately `len(needle)` bytes in the body that is within edit
distance k of the needle. This is the correct operation for
finding typo'd or slightly-different kernel identifiers in patch
content.

It is NOT whole-body Levenshtein (comparing the entire body to the
needle), which would be meaningless for this use case.

`triple_accel::levenshtein_search_simd_with_opts` implements
exactly this: it slides a window over the haystack using a
SIMD-vectorized DP matrix (anti-diagonal parallelism), reporting
all positions where the window content is within edit distance k of
the needle.

### Query grammar extension

```
dfb~1:"smb_check_perm_dacl"   # edit distance 1
dfb~2:"vector_mmsg_rx"        # edit distance 2
```

The router parses `~N` as the edit distance parameter. Default
(bare `dfb:`) remains exact (k=0), preserving backward
compatibility.

### Threshold intersection algorithm (Option B detail)

For needles with `T` distinct trigrams and threshold
`t = max(1, T - 3*k)`:

1. Fetch all T posting-list bitmaps from the FST.
2. Sort by cardinality (smallest first).
3. If `t == T`: pure intersection (current path, unchanged).
4. If `t < T`: use a counting approach:
   - For each candidate in the union of the `t` smallest lists,
     count how many of the T lists contain it.
   - Return candidates with count >= t.
   - Optimization: intersect the smallest `t` lists first (gives
     a superset of the answer), then verify against the remaining
     `T - t` lists, allowing up to `T - t` misses.

This is equivalent to the "T-occurrence" problem on sorted/bitmap
posting lists, well-studied in IR literature.

---

## 6. Recommendation

1. **Add `triple_accel = "0.4"` to `Cargo.toml` dependencies.**
   MIT license, zero transitive deps, SIMD-accelerated, byte-native,
   purpose-built for fuzzy substring search.

2. **Phase 1 (ship fast)**: Keep existing full trigram intersection.
   Add `dfb~1:` / `dfb~2:` predicates. Replace `memmem::find` with
   `levenshtein_search_simd_with_opts` at confirmation time. Recall
   is partial (misses candidates whose trigrams were destroyed by
   the edit) but precision is perfect. Wall-clock impact: +60%
   per-candidate cost, still < 100 ms total for typical queries.

3. **Phase 2 (full recall)**: Implement threshold intersection in
   `SegmentReader::candidate_docids`. Add a `min_trigram_matches`
   parameter. Candidate set grows but remains within budget
   (breakeven at 100k+ candidates).

4. **Do not add `stringzilla`** -- it lacks fuzzy substring search
   in its Rust bindings. `strsim` is scalar and full-string only.
   `rapidfuzz` Rust port is immature.

5. **Cap k at 2.** k=3 requires needles >= 13 chars for a valid
   prefilter and widens candidates by another order of magnitude.
   The use case (kernel identifier typos) rarely needs k > 2.
