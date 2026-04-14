# Research — search library landscape (April 14 2026)

Evaluation that produced the three-tier architecture.

## Rubric

For each candidate: fits embedded-library use in Rust? Supports
custom tokenizer for kernel identifiers? Compact on disk?
Regex/substring on code? Exact-match on structured fields?

## Candidates

### tantivy (0.26, chosen for BM25 tier)

- Fits. Rust-native, Lucene-like. Embedded library.
- Custom tokenizer: yes, `TextAnalyzer::builder` chain.
- Compact: ~20–30% of source with positions, ~15–20% without.
  We run without positions.
- Regex: supported (`RegexQuery::from_pattern` via FST
  intersection), but over a full term dict it can blow up. Use
  with anchoring or `list:`/`rt:` narrowing.
- Structured fields: STRING fields are fine for exact match.
- **Chosen for the BM25 prose tier.**

### tantivy-py (0.25, rejected)

- Thin subset. Can't register Rust tokenizers from Python.
- Fine for v0 prototypes, not for our three-tier design.

### Sonic (rejected)

- Daemon not library. No structured fields. No regex.

### Toshi (rejected)

- HTTP wrapper over tantivy. We're already an embedded user;
  adds latency, not features.

### Quickwit (rejected)

- Distributed log engine. Explicitly warns against user-facing
  search.

### Meilisearch (rejected)

- Typo-tolerant consumer search. Wrong fit for code.

### Bluge / Bleve (rejected)

- Go. We're Rust-first.

### bm25 / probly-search crates (rejected)

- In-memory only. Won't scale to 80 GB of text.

### Zoekt-style trigram (chosen pattern, custom impl)

- Google's Zoekt design is ideal for code/patch substring +
  regex.
- No mature Rust library implements it. Ours is ~400 LOC on
  `fst` + `roaring`.
- **Chosen for the patch/code tier.**

### Arrow/Parquet columnar (chosen for metadata tier)

- Not a search library; a columnar store. Perfect for the
  structured-predicate tier. Dictionary encoding handles our
  cardinality locality; Parquet row-group stats handle our
  range queries.
- **Chosen for the metadata tier.**

## Sources

- [tantivy](https://github.com/quickwit-oss/tantivy)
- [Zoekt](https://github.com/sourcegraph/zoekt)
- [Hound](https://github.com/hound-search/hound)
- [Cursor: fast regex search](https://cursor.com/blog/fast-regex-search)
- [ClickHouse inverted indices with roaring bitmaps](https://clickhouse.com/blog/clickhouse-search-with-inverted-indices)
- [Paul Masurel: Behold, tantivy](https://fulmicoton.com/posts/behold-tantivy/)
