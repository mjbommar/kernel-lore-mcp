# Path tier — file-mention reverse index

**Status:** design. Implementation in `src/path_tier.rs`.

## Why a separate tier

The metadata tier stores `touched_files[]` per message — the set of
paths from `diff --git a/<path>` headers. This answers "which
messages touched file X" when the asker knows the exact full path.

The path tier answers the broader question: "which messages **mention**
file X anywhere in the body — in prose, in quoted diffs, in
shortlogs, in free discussion — regardless of whether the sender
wrote a patch for that file."

Use cases:
- "Who has been discussing `smbacl.c` on linux-cifs in the last
  90 days?" (basename match; reviewer mentions, not only patch
  authors).
- "Find every message anywhere mentioning `fs/smb/server/` as a
  directory prefix" (suffix match — all files under that subtree).

## Design: Aho-Corasick automaton over the known-path vocabulary

The key insight: we already have a ground-truth set of "paths that
exist in the kernel" — the union of `touched_files[]` across every
ingested message. Building an Aho-Corasick automaton from this set
lets us do **exact multi-pattern matching** over each message body
in linear time, with zero false positives by construction.

Why AC instead of regex:
- **Zero false positives.** Only paths we've actually seen in the
  corpus trigger. No `foo.c` noise.
- **O(n) in body length**, independent of pattern count. 500k
  patterns, 15 KB body → microseconds.
- **The `aho-corasick` crate** (BurntSushi, same author as `regex`)
  is already in our transitive dep tree via tantivy. DFA-backed,
  `&[u8]` input, streaming mode, overlapping matches.
- **Single pass.** No extract-then-intersect pipeline; the automaton
  IS the intersection.

### Data flow

```
Ingest (one-time + incremental):

  1. Union all touched_files[] across every Parquet row
     → PathVocab: sorted Vec<String> of ~500k unique paths.

  2. Build AhoCorasick automaton from PathVocab.
     Serialize to $KLMCP_DATA_DIR/paths/automaton.bin.

  3. Build basename → [path_ids] lookup table.
     Serialize alongside (small: ~500k entries → ~15 MB).

  4. For each message body (prose + patch):
     Run automaton.find_overlapping_iter(body).
     Collect {path_id} set per message.
     Append path_id → doc_id to posting lists.

  5. Posting lists: one Roaring bitmap per path_id.
     Segment-based, same pattern as trigram tier.
     Serialize to $KLMCP_DATA_DIR/paths/postings-NNNNNN.roaring.

Query:

  lore_path_mentions(path="smbacl.c", match="basename")

  1. Basename lookup: "smbacl.c" → [path_id_42, path_id_1337]
     (all full paths whose basename is smbacl.c).

  2. Union Roaring bitmaps for path_id_42 + path_id_1337
     → candidate doc_ids.

  3. Fetch metadata rows for those doc_ids → SearchHit[].

  lore_path_mentions(path="fs/smb/server/smbacl.c", match="exact")

  1. Exact lookup: path → path_id_42.
  2. Load single Roaring bitmap → doc_ids.

  lore_path_mentions(path="fs/smb/server/", match="prefix")

  1. Prefix scan over sorted PathVocab: all path_ids whose
     full path starts with "fs/smb/server/".
  2. Union their bitmaps.
```

### Storage estimates (April 2026, full lore)

| Component | Size |
|---|---|
| PathVocab (sorted strings, ~500k) | ~25 MB |
| AC automaton (DFA-backed) | ~40-80 MB |
| Basename→path_id lookup | ~15 MB |
| Posting lists (Roaring, ~500k paths × avg 50 docs) | ~100-300 MB |
| **Total** | **~200-400 MB** |

For the personal 5-list mirror: ~5-20 MB total.

### Incremental ingest

On each grokmirror tick:
1. If any new message introduces a previously-unseen path in
   `touched_files[]`, append it to PathVocab + rebuild the AC
   automaton (atomic rename). Rare — most ticks introduce zero
   new paths.
2. For each new message, run the (possibly updated) automaton
   over the body and append to posting-list segments.
3. Segment compaction on the same schedule as the trigram tier.

### Rust module: `src/path_tier.rs`

```
pub struct PathVocab {
    paths: Vec<String>,          // sorted
    basename_index: HashMap<String, Vec<u32>>,  // basename → path_ids
    automaton: AhoCorasick,
}

pub struct PathPostingsBuilder { ... }
pub struct PathPostingsReader { ... }

impl PathVocab {
    pub fn from_touched_files(all_touched: &[Vec<String>]) -> Self;
    pub fn lookup_exact(&self, path: &str) -> Option<u32>;
    pub fn lookup_basename(&self, basename: &str) -> &[u32];
    pub fn lookup_prefix(&self, prefix: &str) -> Vec<u32>;
    pub fn scan_body(&self, body: &[u8]) -> HashSet<u32>;
}
```

### Python binding

```python
# _core.pyi
class Reader:
    def path_mentions(
        self,
        path: str,
        match: str = "exact",  # "exact" | "basename" | "prefix"
        list: str | None = ...,
        since_unix_ns: int | None = ...,
        limit: int = ...,
    ) -> list[dict[str, Any]]: ...
```

### MCP tool

```python
async def lore_path_mentions(
    path: str,
    match: Literal["exact", "basename", "prefix"] = "exact",
    list: str | None = None,
    since_unix_ns: int | None = None,
    limit: int = 100,
    response_format: Literal["concise", "detailed"] = "concise",
) -> RowsResponse:
    """Find messages that mention a kernel source-tree path anywhere
    in their body — prose, quoted diffs, shortlogs, or patches.

    Unlike lore_activity(file=...) which only searches diff headers,
    this tool catches reviewer discussions, bug reports, and free
    mentions of filenames.

    Cost: cheap — expected p95 80 ms (Aho-Corasick precomputed).
    """
```

### Why not the fst + roaring approach from the trigram tier

The trigram tier is domain-agnostic (arbitrary substring patterns).
The path tier has a **closed vocabulary** — every path we care
about is known at index-build time. AC over a closed vocabulary is
simpler, faster, and produces zero false positives. No need for
fst term dictionaries, no need for trigram extraction, no need for
real-body confirmation.

### Why not regex extraction + intersect (the research doc's approach)

The research doc (`2026-04-15-path-mentions.md`) proposed a regex
to extract candidate path tokens, then intersect against the
`touched_files[]` union. That's a two-stage pipeline with a
precision problem at the regex stage. AC folds both stages into
one pass with perfect precision. The regex work was useful for
understanding the mention classes (it confirmed we need full/
basename/prefix modes), but the AC automaton supersedes it as the
implementation strategy.

## References

- `aho-corasick` crate: <https://docs.rs/aho-corasick/>
- Our trigram tier (different design point): `src/trigram.rs`
- Path-mention research: `docs/research/2026-04-15-path-mentions.md`
- `touched_files[]` extraction: `src/parse.rs`
