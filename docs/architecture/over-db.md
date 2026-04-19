# over.db — SQLite metadata point-lookup tier

`over.db` is the fourth tier of the index ([overview](./four-tier-index.md)).
It is a SQLite database modeled directly on
[public-inbox's `over.sqlite3`](https://public-inbox.org/public-inbox.git/tree/lib/PublicInbox/Over.pm)
that sits between the BM25 / trigram tiers and the analytical
Parquet metadata. It services every router predicate that resolves
to "give me the row(s) for these message_ids" or "give me the
newest-N rows for `f:<addr>` / `list:<name>` / `since:<ts>`".

## Background — why this tier exists

The 2026-04-16 full-corpus ingest (29M messages across 346 lists)
exposed an architectural mismatch: **Parquet is the wrong format
for ID-keyed point lookups.** Every metadata-fetch path —
`fetch_message`, `eq()`, BM25 hit hydration — was doing a full
Parquet scan over all 29M rows. Measured wall-clock for a query
that returned 20 hits: **2 minutes 53 seconds**. tantivy itself
returned the docids in milliseconds; the slowness was entirely the
Parquet round-trip. Worst-case (`from_addr` of a high-volume
maintainer like `gregkh@linuxfoundation.org`): OOM at 36 GB RSS.

The Apache Arrow team is explicit about this: Parquet is column-
scan optimized, not point-lookup optimized. Bloom filters and page
indexes help marginally for sparse predicates but cannot match a
B-tree.

Surveyed production hybrid-search systems all converge on the same
pattern: **search engine returns IDs; separate ID-keyed row store
serves displayable metadata.**

- **public-inbox / lore.kernel.org** — the canonical kernel email
  archive — uses Xapian + `over.sqlite3`. Display fields packed
  into one zstd-compressed BLOB column (`ddd`), keyed by integer
  docid. Handles the same scale we're targeting.
- **lei** — same `over.sqlite3` pattern locally.
- **Quickwit** — colocated 1MB-block ZSTD docstore inside each split.
- **Elasticsearch / OpenSearch** — Lucene `_source` stored fields.
- **Meilisearch** uses LMDB; **Typesense** uses RocksDB.

Adopting the public-inbox `over.db` pattern aligns us with the
kernel community's own infrastructure and was the lowest-risk path.

Full design rationale and validation:

- [Plan: SQLite "over.db" metadata tier](../plans/2026-04-17-overdb-metadata-tier.md)
- [over.db tier — Phase 5 validation](../research/2026-04-17-overdb-validation.md)

## Schema

This is the schema as actually implemented in [`src/over.rs`](../../src/over.rs).
It diverges from the original plan in two places — see
[Index changes](#index-changes-after-validation) below.

```sql
-- One row per (message_id, list). The unique index makes
-- INSERT OR REPLACE keyed on that pair the natural idempotency
-- contract: re-ingesting a shard never doubles cross-posts.
CREATE TABLE over (
    rowid           INTEGER PRIMARY KEY AUTOINCREMENT,

    -- Indexed lookup keys.
    message_id      TEXT NOT NULL,                -- canonical RFC 822 mid (no <>)
    list            TEXT NOT NULL,                -- e.g. "linux-cifs"
    from_addr       TEXT,                         -- normalized lowercase
    date_unix_ns    INTEGER,                      -- nullable; missing dates = NULL
    in_reply_to     TEXT,                         -- for thread reconstruction
    tid             TEXT,                         -- thread id (NULL until rebuild_tid wires through)

    -- Body locator (so callers don't need a separate Parquet scan
    -- to find a message's body in the compressed store).
    body_segment_id INTEGER NOT NULL,
    body_offset     INTEGER NOT NULL,
    body_length     INTEGER NOT NULL,
    body_sha256     TEXT NOT NULL,

    -- Boolean flags worth promoting to columns (cheap; queryable).
    has_patch       INTEGER NOT NULL DEFAULT 0,
    is_cover_letter INTEGER NOT NULL DEFAULT 0,
    series_version  INTEGER,
    series_index    INTEGER,
    series_total    INTEGER,

    -- Patch stats (for activity dashboards).
    files_changed   INTEGER,
    insertions      INTEGER,
    deletions       INTEGER,
    commit_oid      TEXT,

    -- Compressed display blob: zstd-3(msgpack(DddPayload)).
    -- DddPayload contents:
    --   subject_raw, subject_normalized, subject_tags[],
    --   references[], touched_files[], touched_functions[],
    --   signed_off_by[], reviewed_by[], acked_by[], tested_by[],
    --   co_developed_by[], reported_by[], suggested_by[],
    --   helped_by[], assisted_by[], fixes[], link[], closes[],
    --   cc_stable[], trailers_json, from_name,
    --   from_addr_original_case, shard
    -- Decoded only when serializing a row to the caller. Never
    -- inspected by the query path.
    ddd             BLOB NOT NULL
);

-- Primary lookup: by canonical message-id.
CREATE INDEX over_msgid      ON over (message_id);

-- f:<addr> queries — composite (from_addr, date_unix_ns DESC) lets
-- popular-author queries pull newest-N matches in index order.
-- Replaces the original single-column over_from index after Phase 5
-- showed 5.4 s p95 on gregkh-volume queries.
CREATE INDEX over_from_date  ON over (from_addr, date_unix_ns DESC);

-- list:<name> ordered by date for newest-first scans.
CREATE INDEX over_list_date  ON over (list, date_unix_ns DESC);

-- since:<ts> queries without a list scope.
CREATE INDEX over_date       ON over (date_unix_ns DESC);

-- Thread reconstruction.
CREATE INDEX over_tid        ON over (tid);
CREATE INDEX over_reply      ON over (in_reply_to);

-- Cross-posts legitimately share message_id across lists, so
-- message_id alone cannot be UNIQUE. (message_id, list) is the
-- natural identity key. INSERT OR REPLACE on this index gives
-- re-ingest idempotency.
CREATE UNIQUE INDEX over_mid_list ON over (message_id, list);

-- Schema version + bookkeeping.
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO meta(key, value) VALUES
    ('schema_version', '1'),
    ('source_tier',    'parquet:metadata/'),
    ('built_at',       '');  -- set at build time
```

### Pragmas

Set in `OverDb::configure` ([src/over.rs#L209](../../src/over.rs)):

| Pragma | Value | Why |
|---|---|---|
| `journal_mode` | `WAL` | Concurrent readers + single writer; safe for incremental ingest. |
| `synchronous` | `NORMAL` | Safe under WAL; ~3× write throughput vs `FULL`. |
| `mmap_size` | `268_435_456` (256 MB) | Lowered from 4 GB after Phase 5 measured 1.75 GB reader RSS without latency benefit. Point lookups touch a tiny working set. |
| `temp_store` | `MEMORY` | Avoids on-disk temp files for sorts. |
| `cache_size` | `-200_000` (200 MB) | Same RSS-vs-latency tradeoff as `mmap_size`. |

The build binary overrides `synchronous=OFF` and
`journal_mode=MEMORY` in `OverDb::open_for_bulk_load` — safe
because the build writes to a tempfile that is atomically renamed
only on success.

### Index changes after validation

The plan §Schema specified `CREATE INDEX over_from ON over (from_addr)`.
Phase 5 measured `gregkh@linuxfoundation.org` queries at 5.4 s p95
because the single-column index forced a sort-after-fetch over
10k+ matching rows. Replacing it with the composite
`(from_addr, date_unix_ns DESC)` lets the query planner traverse
in index order and stop at LIMIT, dropping p95 to 5.27 ms — a
1000× speedup. See [validation §5b after fixes](../research/2026-04-17-overdb-validation.md).

The same validation pass dropped `mmap_size` and `cache_size`
(see table above). Both changes are in `src/over.rs` at HEAD.

## Query patterns

Five `Reader` paths in [`src/reader.rs`](../../src/reader.rs) use
over.db. Every path keeps a Parquet-scan fallback for callers
without an over.db on disk (graceful degradation when the file is
absent).

### `fetch_message(message_id) -> Option<MessageRow>`

Direct point lookup via the `over_msgid` index. Cross-posts
collapse to the freshest row by `date_unix_ns` inside `OverDb::get`.

| | Latency |
|---|---|
| Before over.db (Parquet scan) | 187 000 ms |
| After over.db (post-tuning) | **0.06 ms p50, 2.21 ms max** |

### `eq(EqField, value, since, list_filter, limit) -> Vec<MessageRow>`

Indexed scan via `OverDb::scan_eq`. Routes:

| Field | Index used |
|---|---|
| `MessageId` | `over_msgid` (delegates to `get`) |
| `FromAddr` | `over_from_date` (composite) |
| `List` | `over_list_date` (composite) |
| `InReplyTo` | `over_reply` |
| `Tid` | `over_tid` |
| Other (`SignedOffBy`, `TouchedFile`, …) | sequential `ddd` decode + filter; logs a warning |

| | Latency |
|---|---|
| `eq from_addr` before over.db | 187 000 ms / OOM |
| `eq from_addr` first build (single-column index) | 587 ms p50, 5414 ms p95 |
| `eq from_addr` after composite index | **3.08 ms p50, 5.27 ms p95** |
| `eq list` after over.db | **2.33 ms p50, 2.85 ms p95** |

### `prose_search_filtered(query, list_filter, limit) -> Vec<(MessageRow, f32)>`

BM25/tantivy returns top-K message_ids; over.db hydrates them via
`OverDb::get_many` (chunked `IN (?,?,…)` at SQLite's 999-parameter
limit).

| | Latency |
|---|---|
| Before over.db (per-hit Parquet scan) | 170 000 ms |
| After over.db | **23.50 ms p50, 47.48 ms p95** |

### `patch_search(...)` and `patch_search_fuzzy(...)`

Trigram tier returns candidate message_ids; over.db hydrates them
via the same `get_many` path as prose_search. The `list_filter` is
pushed down via the `over_list_date` index where applicable.

### `all_rows(list, since)`

When called with a `list:` predicate, routes through `scan_eq` on
`over_list_date` with a 1 000 000 row safety limit. The legacy
`Reader::scan` Parquet path is the fallback for the no-list case
and pre-over.db deployments.

## Build process

The `kernel-lore-build-over` binary
([src/bin/build_over.rs](../../src/bin/build_over.rs)) builds (or
rebuilds) over.db from the existing metadata Parquet in a single
streaming pass.

```sh
KLMCP_DATA_DIR=/var/klmcp/data kernel-lore-build-over
# or:
kernel-lore-build-over \
    --data-dir   /var/klmcp/data \
    --output     /var/klmcp/data/over.db \
    --from-list  linux-cifs \
    --batch-size 10000
```

**Wall-clock:** ~30 minutes for the full 17.6M-row corpus on the
reference workstation. ~2 minutes per million rows after the
deferred-index speedup.

**Strategy:**

1. Open `<output>.tmp.<run_id>` via `OverDb::open_for_bulk_load`
   (table only — no indexes). Bulk-load pragmas (`synchronous=OFF`,
   `journal_mode=MEMORY`).
2. Stream every row through `Reader::scan_streaming` (NOT
   `scan_all`, which would materialize 17M+ rows in a Vec).
3. Flush in batches of `--batch-size` (default 10 000) inside one
   transaction per batch.
4. After all inserts: `create_indexes()`, `PRAGMA optimize`,
   `VACUUM`, set `meta.built_at`.
5. On success, atomic rename `.tmp.<run_id>` → `<output>`. On
   error, the tempfile stays in place for inspection.

The deferred-index strategy (CREATE INDEX *after* the bulk INSERT
loop) is the dominant speedup. Index build time on the populated
17.6M-row over.db: 2:15.

## Public-inbox precedent

over.db is a near-direct port of public-inbox's `over.sqlite3`
([Over.pm](https://public-inbox.org/public-inbox.git/tree/lib/PublicInbox/Over.pm)).
The structural parallels:

| public-inbox `Over.pm` | kernel-lore-mcp `over.rs` |
|---|---|
| `over.sqlite3` per inbox | one shared `over.db` for all lists |
| `ddd` BLOB column with packed display fields | `ddd` BLOB column with zstd(msgpack(DddPayload)) |
| Indexed `(num, mid, ...)` | Indexed `(message_id, list, from_addr, list+date, …)` |
| Xapian returns docids → Over fills in display | tantivy/trigram return mids → OverDb fills in display |

The differences are minor: we use msgpack instead of public-inbox's
custom packed format (Rust ergonomics), zstd-3 instead of zlib
(better ratio at comparable speed), and one shared file instead of
per-inbox. The shared-file choice is reversible via SQLite ATTACH
if file size becomes painful at much larger scale; out-of-scope for
v1.

Choosing this pattern aligned us with the kernel community's own
infrastructure: the same data layout that lore.kernel.org runs
against in production.

## Limitations

### Cross-post collapse (validation §5e)

The Phase 5 cross-post check returned zero rows in a 17.6M-message
corpus, which is suspicious — public-inbox cross-posts (same
Message-ID on multiple lists, e.g. patches CC'd to lkml + a
subsystem list) should appear. The hypothesis: the upstream
`Reader::scan` mtime-DESC + freshest-wins dedup collapses
duplicates by `message_id` alone before they reach over.db,
flattening cross-posts to one (list, message_id) pair. The schema
supports the multi-row representation; the upstream dedup pass is
what to fix.

Filed as a follow-up — does not block validation closure since
the displayed list is correct (just one of N) and queries return
correct rows. `cross_posted_to[]` reconstruction is a v2 feature.

### Non-indexed `EqField` fallback

`scan_eq` for `SignedOffBy`, `TouchedFile`, `Reference`,
`SubjectTag`, etc. has no dedicated index — these fall through to
a sequential scan over `ddd`-decoded rows in
`OverDb::scan_eq_sequential` (logs a warning). On a 17.6M-row DB
that's not free; for the dominant query mix it's rare and
acceptable, but should it become a hot path the answer is to
promote the field to its own column or a side-table.

The plan §Out-of-scope deferred this: see also the validation
report's "Other findings worth recording" section.

### `tid` column is NULL

Phase 4 wired ingest to write to over.db incrementally, but the
`tid` column is left NULL pending the `rebuild_tid` pass. Reader
paths that join on `tid` either route through the Parquet
side-table at `<data_dir>/tid/tid.parquet` or fall back to
in-memory thread reconstruction from `in_reply_to` /
`references[]`. When `rebuild_tid` is wired through, the
`over_tid` index becomes useful for `series_timeline`-style
queries; until then it is dormant.

## References

- public-inbox `Over.pm`:
  https://public-inbox.org/public-inbox.git/tree/lib/PublicInbox/Over.pm
- public-inbox v2 format:
  https://www.mankier.com/5/public-inbox-v2-format
- lei store layout:
  https://public-inbox.org/lei-store-format.html
- Apache Arrow on Parquet point lookups:
  https://arrow.apache.org/blog/2022/12/26/querying-parquet-with-millisecond-latency/
- Quickwit 0.4 docstore design:
  https://quickwit.io/blog/quickwit-0.4
- Lucene/ES `_source` storage:
  https://www.elastic.co/blog/store-compression-in-lucene-and-elasticsearch
- This project's plan:
  [`../plans/2026-04-17-overdb-metadata-tier.md`](../plans/2026-04-17-overdb-metadata-tier.md)
- This project's validation report:
  [`../research/2026-04-17-overdb-validation.md`](../research/2026-04-17-overdb-validation.md)
- Sibling tier docs:
  [`./four-tier-index.md`](./four-tier-index.md)
