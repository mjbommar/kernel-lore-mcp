# Three-tier index — why

## The premise

A kernel mailing list archive is not one corpus. It's three, with
different query classes and different optimal data structures:

| Tier | Content | Query class | Right data structure |
|------|---------|-------------|----------------------|
| Metadata | Structured headers + extracted facts (from, date, touched_files, touched_functions) | Predicate intersection, range scans | Columnar (Arrow/Parquet) |
| Trigram | Patch/diff content (code, hunk context) | Regex, substring, identifier-part | Zoekt-style trigram: `fst` term dict + `roaring` postings |
| BM25 | Prose body (message minus patch), subject | Ranked free-text | Inverted index (tantivy), no positions, no stemming |

A single monolithic index (e.g., tantivy over everything) either
overpays in storage (positions for prose you never phrase-query) or
underserves code queries (word tokenizer mangles
`vector_mmsg_rx` into junk).

## Query class → tier

Almost every real kernel query routes to one or two tiers:

- `dfn:fs/smb/server/smbacl.c since:30d`
  → metadata only. ~1ms.
- `dfhh:ksmbd_alloc_user from:greg`
  → metadata only. ~1ms.
- `/SMB2_CREATE.*oplock/`
  → trigram. Sub-100ms at lore scale.
- `"ordering problem" body:"teardown"`
  → BM25 (phrase). Requires positions on a narrow subindex if we
  ever re-enable them. v1: drop to substring via trigram.
- `s:"[PATCH v3]" list:linux-cifs`
  → metadata (subject_normalized + list).
- `author:namjae touched:ksmbd.c since:6mo`
  → metadata + trigram intersection, no BM25.

The BM25 tier exists for the residual prose-search case
(reviewer comments, bug discussions) that the other two can't
answer. We expect it to serve <25% of queries.

## Storage estimates for full lore (~15–25M messages)

| Tier | Estimate | Rationale |
|------|----------|-----------|
| Metadata | 2–5 GB | Arrow+zstd on highly-repetitive columns (From, Date, List-Id) compresses ~15–20×. |
| Trigram | 15–25 GB | roaring postings per trigram; dominated by common trigrams (`var`, `end`, etc.). |
| BM25 | 8–15 GB | tantivy WithFreqs (no positions), prose-only (patches excluded). |
| Compressed raw store | 20–35 GB | zstd-dict-trained per list on headers+bodies. Rebuilds indices. |
| Subsystem maintainer trees (git) | 50–100 GB | NOT mailing list; separate mirror. |
| **Total (v1)** | **~100–175 GB** | Fits 500 GB gp3 with huge headroom. |

## Rebuildability contract

The compressed raw store is the source of truth. You can nuke all
three index tiers and rebuild them from it without refetching
anything. This lets us change tokenizers or schema with one
`cargo run --bin reindex`.

## What we dropped

- **Positional BM25 postings.** Saves ~30–50% of the BM25 tier.
  Cost: no native phrase queries. Mitigation: regex-through-trigram
  handles most "multi-word-in-order" cases for code; for prose
  phrases we can reintroduce positions on a narrow field later.
- **Stemming.** Mangles kernel identifiers. tantivy 0.26 gates
  stemming behind a feature flag; we never enable it.
- **Stopwords.** Kernel prose has none useful.
- **Keeping full git shards after ingest.** We keep the compressed
  raw store. Shards can be rebuilt from `grokmirror` in an hour.
