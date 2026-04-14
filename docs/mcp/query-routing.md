# MCP — query routing

The router lives in Rust (`src/router.rs`), exposed via PyO3.

## Grammar (v1)

Subset of lei (public-inbox Xapian). Deliberately **subset** —
easier to add than to remove.

| Operator | Meaning | Tier |
|---|---|---|
| `s:<term>` | subject match | metadata (subject_normalized) + BM25 |
| `b:<term>` | body prose match | BM25 |
| `f:<term>` | from match | metadata |
| `t:<term>` | to match | metadata (v2) |
| `c:<term>` | cc match | metadata (v2) |
| `dfn:<file>` | diff filename | metadata (touched_files) |
| `dfhh:<func>` | diff hunk function | metadata (touched_functions) |
| `rt:<since>..<until>` | date range | metadata |
| `list:<name>` | mailing list pin | metadata |
| `mid:<id>` | message-id exact | metadata |
| `"phrase"` | phrase | BM25 or trigram (router decides) |
| `/regex/` | regex over patch content | trigram |
| `AND`, `OR`, `NOT`, `+`, `-` | boolean | merged |

## Dispatch rules

1. **Parse** into an AST (every node has required tier set).
2. **Hoist metadata predicates** first — they're cheap and narrow
   the candidate set.
3. **Dispatch tier queries in parallel** via rayon:
   - metadata predicates → metadata tier → `RoaringBitmap<u64>`
     of matching internal docids.
   - trigram predicates → trigram tier → `RoaringBitmap<u64>`.
   - BM25 predicates → tantivy searcher → ranked `Vec<(docid,
     score)>`.
4. **Intersect bitmaps** for AND; union for OR.
5. **Score merge**: if BM25 participated, use its score; otherwise
   order by `date DESC`.
6. **Confirm** trigram hits by re-running the real regex against
   mmap'd patch content (trigrams are filter-only, not proof).

## Defaults

- No `list:` in query → all lists. Router adds a soft warning to
  response metadata if the candidate set > 1M.
- No `rt:` in query → last 5 years. Older queryable via explicit
  `rt:`. This is purely a sanity default; users can override.
- `limit` defaults to 25; caps at 200.

## Safety limits

- Leading-wildcard regex (`.*foo`) over a full term dict errors
  with a helpful message: "anchor the regex or add `list:`/`rt:`".
- Per-query total CPU wall-clock: 5s hard cap via a Rust
  `crossbeam::channel::after` kill switch.
- Per-IP rate limit: 60 req/min anonymous, higher with Bearer.

## Cursoring

Cursors are opaque base64-encoded tuples:
```
(last_seen_score: f32 or null,
 last_seen_date:  i64 ns,
 last_seen_mid:   String,
 query_hash:      u64)
```

On continuation we re-run the query but add `score < last_seen_score`
(BM25) or `date < last_seen_date` (metadata) filter. Cursor validity
is single-index-snapshot — if the index reopens between page
requests the cursor still works but may return slightly different
results. We document that.
