# Four-tier index — why

## The premise

A kernel mailing list archive is not one corpus. It's four, with
different query classes and different optimal data structures:

| Tier | Content | Query class | Right data structure |
|------|---------|-------------|----------------------|
| Metadata (analytical) | Structured headers + extracted facts (from, date, touched_files, touched_functions) | Range scans, batch analytics, Parquet readers | Columnar (Arrow/Parquet) |
| Metadata (point lookup) | Same fields, indexed by `(message_id, list)` + common predicates | `mid:`, `f:`, `list:`, `since:`, BM25/trigram hydration | SQLite `over.db` (public-inbox pattern): indexed columns + zstd-msgpack `ddd` blob |
| Trigram | Patch/diff content (code, hunk context) | Regex, substring, identifier-part | Zoekt-style trigram: `fst` term dict + `roaring` postings |
| BM25 | Prose body (message minus patch), subject | Ranked free-text | Inverted index (tantivy), no positions, no stemming |

A single monolithic index (e.g., tantivy over everything) either
overpays in storage (positions for prose you never phrase-query) or
underserves code queries (word tokenizer mangles
`vector_mmsg_rx` into junk). And a single columnar store for
metadata fails the *point-lookup* class catastrophically — see
[`over-db.md`](./over-db.md) for the failure mode that motivated
adding the fourth tier.

## Query class → tier

Almost every real kernel query routes to one or two tiers:

- `dfn:fs/smb/server/smbacl.c since:30d`
  → metadata (over.db indexed scan). Sub-millisecond.
- `dfhh:ksmbd_alloc_user from:greg`
  → metadata (over.db `from_addr` + `list` filter). ~3 ms.
- `mid:<20260415.foo@bar>`
  → metadata (over.db point lookup). ~0.06 ms.
- `/SMB2_CREATE.*oplock/`
  → trigram + over.db hydration. Sub-100ms at lore scale.
- `"ordering problem" body:"teardown"`
  → BM25 (phrase). Requires positions on a narrow subindex if we
  ever re-enable them. v1: drop to substring via trigram.
- `s:"[PATCH v3]" list:linux-cifs`
  → metadata (subject_normalized + list). over.db `list+date`
  composite index.
- `author:namjae touched:ksmbd.c since:6mo`
  → metadata (over.db `from_addr+date`) + trigram intersection,
  no BM25.

The BM25 tier exists for the residual prose-search case
(reviewer comments, bug discussions) that the other two can't
answer. We expect it to serve <25% of queries. Every BM25 hit is
hydrated to a full row via over.db's `get_many` (chunked
`IN (?,?,…)`) — that's the path that dropped from 170 s to 23 ms
at p50 in the Phase 5 validation run.

## Why two metadata tiers

Parquet is column-scan optimized. It is the wrong data structure
for ID-keyed point lookups: `fetch_message(<mid>)` against the
17.6 M-row metadata Parquet was 187 s before over.db landed. The
Apache Arrow team is explicit about this — bloom filters and page
indexes help marginally but cannot match a B-tree.

over.db (SQLite, public-inbox `over.sqlite3` pattern) sits in
front of Parquet for the predicate paths the router actually
hits. Parquet stays as the source of truth for analytical scans,
schema migration replays, and as the rebuild source for over.db.
See [`over-db.md`](./over-db.md) for full design rationale,
schema, and the validation numbers.

## Storage estimates for full lore (~17–29M messages)

| Tier | Estimate | Rationale |
|------|----------|-----------|
| Metadata Parquet | 4–5 GB | Arrow+zstd on highly-repetitive columns (From, Date, List-Id) compresses ~15–20×. |
| Metadata over.db | 12–19 GB | One row per (message_id, list) with indexed columns + zstd-msgpack `ddd` blob. Measured at 19 GB for 17.6M rows; scales linearly. |
| Trigram | 15–25 GB | roaring postings per trigram; dominated by common trigrams (`var`, `end`, etc.). |
| BM25 | 8–15 GB | tantivy WithFreqs (no positions), prose-only (patches excluded). |
| Compressed raw store | 20–104 GB | zstd-dict-trained per list on headers+bodies. Rebuilds indices. |
| Subsystem maintainer trees (git) | 50–100 GB | NOT mailing list; separate mirror. |
| **Total (v1)** | **~110–270 GB** | Fits 500 GB gp3 with headroom. |

Measured footprint for the full 17.6M-message corpus on the
reference workstation lives in
[`../ops/corpus-coverage.md`](../ops/corpus-coverage.md).

## Rebuildability contract

The compressed raw store is the source of truth. You can nuke any
or all four index tiers and rebuild them from it without
refetching anything. The dependency chain:

1. **Compressed store** — source of truth. Holds raw RFC822
   bytes, addressable by `(segment_id, offset, length, sha256)`.
   Rebuilds from `grokmirror` in ~2 hours.
2. **Metadata Parquet** + **trigram** + **BM25** — all rebuild
   from the store via `cargo run --bin reindex` (~3 hours on a
   workstation; ~1.5 hours on `r7g.xlarge`).
3. **over.db** — rebuilds from metadata Parquet via
   `kernel-lore-build-over` in ~30 minutes for 17.6M rows. No
   re-walk of the store needed; over.db is a downstream
   projection of Parquet.

This lets us change tokenizers, schema, or the over.db `ddd`
payload format with one targeted rebuild instead of a full
re-ingest. The build binary writes to a tempfile and atomic-renames
on success, so a crash mid-build never exposes a half-written DB.

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
- **Parquet as the primary metadata-lookup path.** Demoted to
  analytical / cold tier in favor of over.db for predicate paths.
  See [`over-db.md`](./over-db.md) §Background for the failure
  mode that forced this.
