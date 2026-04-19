# Plan: SQLite "over.db" metadata tier

**Date:** 2026-04-17
**Author:** Driven by the lore-scale failure mode discovered during full-corpus ingest (2026-04-16/17).
**Status:** Plan; not yet executed.

## Background

The 2026-04-16 full ingest (29M messages across 346 lists) revealed an
architectural mismatch: **Parquet is the wrong format for ID-keyed
point lookups.** Every metadata-fetch path — `fetch_message`, `eq()`,
`router_search` follow-up after a tantivy hit — does a full Parquet
scan over all 29M rows. Measured cost: 2:53 wall-clock for a query
that returns 20 hits, regardless of how few results match. tantivy
itself returns the docids in milliseconds; the slowness is entirely
the Parquet round-trip.

The Apache Arrow team is explicit about this: Parquet is column-scan
optimized, not point-lookup optimized. Bloom filters and page indexes
help marginally for sparse predicates but cannot match a B-tree.

## The pattern that scales

Surveyed production hybrid-search systems all converge on the same
pattern: **search engine returns IDs; separate ID-keyed row store
serves displayable metadata.**

- **public-inbox / lore.kernel.org** — the canonical kernel email
  archive — uses Xapian + `over.sqlite3`. Display fields packed into
  one zstd-compressed BLOB column (`ddd`), keyed by integer docid.
  Handles the same scale we're targeting.
- **lei** — same `over.sqlite3` pattern locally.
- **Quickwit** — colocated 1MB-block ZSTD docstore inside each split.
- **Elasticsearch / OpenSearch** — Lucene `_source` stored fields.
- **Meilisearch** uses LMDB; **Typesense** uses RocksDB.

Adopting the public-inbox `over.db` pattern aligns us with the kernel
community's own infrastructure and is the lowest-risk path.

## Goals

1. **Sub-millisecond point lookup** by `message_id`.
2. **Sub-second filtered scans** for `f:`, `list:`, `since:` (indexed
   columns).
3. **Zero impact on existing tiers** — store, BM25, trigram, tid all
   stay as-is.
4. **Rebuildable** — `over.db` can be regenerated from the metadata
   Parquet (which is itself rebuildable from the compressed store).
5. **Atomic build + swap** — never expose a half-written `over.db`.

## Non-goals

- Replace Parquet. Parquet remains for analytical scans + cold archive.
- Re-ingest from grokmirror.
- Re-build BM25.
- Re-build trigram.
- Change the compressed store (source of truth for bodies).
- Change the MCP tool surface (transparent to callers).

## Schema

```sql
-- One row per message. Indexed columns mirror the most common
-- predicates from the query router (f:, list:, since:, mid:).
-- Everything else lives in `ddd` to avoid column-overhead per row.
CREATE TABLE over (
    rowid           INTEGER PRIMARY KEY AUTOINCREMENT,

    -- Indexed lookup keys
    message_id      TEXT NOT NULL,                -- canonical RFC 822 mid (no <>)
    list            TEXT NOT NULL,                -- e.g. "linux-cifs"
    from_addr       TEXT,                         -- normalized lowercase
    date_unix_ns    INTEGER,                      -- nullable; missing dates = NULL
    in_reply_to     TEXT,                         -- for thread reconstruction
    tid             TEXT,                         -- thread id

    -- Body locator (so callers don't need a separate Parquet scan
    -- to find a message's body in the compressed store)
    body_segment_id INTEGER NOT NULL,
    body_offset     INTEGER NOT NULL,
    body_length     INTEGER NOT NULL,
    body_sha256     TEXT NOT NULL,

    -- Boolean flags worth promoting to columns (cheap; queryable)
    has_patch       INTEGER NOT NULL DEFAULT 0,   -- 0 or 1
    is_cover_letter INTEGER NOT NULL DEFAULT 0,
    series_version  INTEGER,
    series_index    INTEGER,
    series_total    INTEGER,

    -- Patch stats (for activity dashboards)
    files_changed   INTEGER,
    insertions      INTEGER,
    deletions       INTEGER,
    commit_oid      TEXT,

    -- Compressed display blob: zstd(msgpack({
    --     subject_raw, subject_normalized, subject_tags[],
    --     references[], touched_files[], touched_functions[],
    --     signed_off_by[], reviewed_by[], acked_by[], tested_by[],
    --     co_developed_by[], reported_by[], suggested_by[],
    --     helped_by[], assisted_by[], fixes[], link[], closes[],
    --     cc_stable[], trailers_json, from_name
    -- }))
    -- Decoded only when serializing a row to the caller. Never
    -- inspected by the query path.
    ddd             BLOB NOT NULL
);

-- Primary lookup: by canonical message-id. UNIQUE not used because
-- duplicate mids exist across lists (cross-posts) and we want to
-- keep all copies for cross_posted_to[] reconstruction.
CREATE INDEX over_msgid ON over (message_id);

-- f:<addr> queries. case-folded at INSERT time.
CREATE INDEX over_from ON over (from_addr);

-- list:<name> ordered by date for newest-first scans.
CREATE INDEX over_list_date ON over (list, date_unix_ns DESC);

-- since:<ts> queries within a single list.
CREATE INDEX over_date ON over (date_unix_ns DESC);

-- Thread reconstruction. Not always needed — could defer.
CREATE INDEX over_tid ON over (tid);
CREATE INDEX over_reply ON over (in_reply_to);

-- Schema version + bookkeeping
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT INTO meta(key, value) VALUES
    ('schema_version', '1'),
    ('source_tier', 'parquet:metadata/'),
    ('built_at', '');  -- set at build time
```

### Sizing estimate

- 29M rows
- Per row: ~80 bytes indexed columns + ~200-400 bytes ddd (zstd of
  ~1-2 KB raw display payload, typical 4-5x compression for header
  text)
- Plus SQLite indices: ~7 indexes × ~30 bytes each = +6 GB
- **Total estimate: 12-18 GB.** Fits easily on local NVMe.

## Phase plan

### Phase 0 — Decisions ratified (this document)

- ✅ Pattern: public-inbox `over.db`, single SQLite file
- ✅ Schema: hybrid (indexed columns for common predicates + `ddd`
  blob for the rest)
- ✅ Location: `<data_dir>/over.db`
- ✅ Compression: zstd-3 for `ddd` (fast decode; payload is small)
- ✅ Serialization for `ddd`: msgpack (compact, fast, schema-free)
- ✅ Cross-posted message handling: one row per (list, message_id)
  pair; the freshest-wins dedup logic moves into the SELECT path

**Open question:** does `from_addr` need to be indexed
case-insensitively? Public-inbox normalizes-then-indexes. We follow
suit: `LOWER(from_addr)` at INSERT, query path uses lowercase too.

### Phase 1 — `src/over.rs` (new module)

Estimated: 1 day

Tasks:

- **1a.** Add deps: `rusqlite = { version = "0.33", features = ["bundled"] }`,
  `rmp-serde = "1.3"` (msgpack), `zstd = "0.13"` (already present).
- **1b.** Define `OverRow` struct mirroring the column layout (Rust-side).
- **1c.** Define `DddPayload` struct for the BLOB contents.
- **1d.** Implement `OverDb::open(path)` — opens or creates, runs schema
  migration, sets pragma `journal_mode=WAL`, `synchronous=NORMAL`,
  `mmap_size=4GB`, `temp_store=MEMORY`.
- **1e.** Implement `OverDb::insert_batch(rows: &[OverRow])` — single
  transaction, prepared statement, batch size 10k.
- **1f.** Implement `OverDb::get(message_id) -> Option<MessageRow>` —
  single SELECT with index lookup, decodes `ddd` to materialize a
  full `MessageRow`.
- **1g.** Implement `OverDb::get_many(message_ids: &[String]) -> HashMap<String, MessageRow>` —
  uses `IN (?1, ?2, ...)` chunked at SQLite's parameter limit (999).
- **1h.** Implement `OverDb::scan_eq(field: EqField, value: &str, ...)` —
  router predicates: from_addr, list, since.
- **1i.** Unit tests with synthetic data, in-memory `:memory:` DB.

### Phase 2 — `kernel-lore-build-over` binary

Estimated: half day

Tasks:

- **2a.** New bin: `src/bin/build_over.rs`.
- **2b.** CLI: `--data-dir`, `--output` (default `<data_dir>/over.db`),
  `--from-list NAME` (optional, restrict to one list for testing),
  `--batch-size N` (default 10k).
- **2c.** Read all Parquet files via existing `Reader::scan_all`
  (streaming, not load-all). For each row:
  - Extract indexed columns
  - Build DddPayload from remaining fields
  - msgpack + zstd encode → ddd
  - Push into batch buffer; flush at `batch_size`.
- **2d.** Build to a temp file (`over.db.tmp.<run_id>`), then atomic
  rename to `over.db` on success. Index creation happens AFTER all
  inserts (huge speedup; SQLite docs).
- **2e.** Final pragma `optimize`. `vacuum`. Set `meta.built_at`.
- **2f.** Logs: row count, elapsed, MB/sec, final db size.

### Phase 3 — Wire reader to use over.db

Estimated: 1 day

Tasks:

- **3a.** Modify `Reader::new(data_dir)` to lazily open `over.db` if it
  exists. Behind `Option<OverDb>`, so callers without an `over.db` keep
  the old slow Parquet path (graceful fallback).
- **3b.** Replace `Reader::fetch_message(message_id)` body:
  ```rust
  if let Some(over) = &self.over {
      return Ok(over.get(message_id)?);
  }
  // ... existing Parquet scan as fallback
  ```
- **3c.** Replace `Reader::eq(EqField, ...)` body:
  ```rust
  if let Some(over) = &self.over {
      return over.scan_eq(field, value, since_unix_ns, list_filter, limit);
  }
  // ... existing Parquet scan as fallback
  ```
- **3d.** Replace `Reader::prose_search_filtered`'s post-tantivy
  metadata fetch with `over.get_many(top_message_ids)`.
- **3e.** Same pattern for `Reader::patch_search` (after trigram
  candidates).
- **3f.** Same pattern for `Reader::all_rows` when only `list:` filter
  is set — use indexed `over_list_date` scan with LIMIT.
- **3g.** Update `Reader::activity` to use `over_list_date` index.
- **3h.** Update `Reader::series_timeline` to use `over_tid` /
  `over_reply` indexes.

### Phase 4 — Wire ingest to write over.db incrementally

Estimated: 1 day

Tasks:

- **4a.** Modify `ingest_shard_with_bm25` to also append to `over.db`
  in the same per-shard transaction. New parameter `over: Option<&Mutex<OverDb>>`.
- **4b.** In `bin/ingest.rs::main`, open one shared `Mutex<OverDb>`
  (analogous to the shared BM25 writer), pass to all rayon workers.
- **4c.** Atomic visibility: only commit the over.db transaction
  after the per-shard Parquet write succeeds. Generation bump still
  happens once at the end of the run.
- **4d.** Idempotency: `INSERT OR REPLACE` keyed on
  `(message_id, list)` so re-ingests don't double-count cross-posts.

### Phase 5 — Validation

Estimated: half day

Tasks:

- **5a.** Parity test: for 100 random message_ids, compare output of
  old Parquet path vs new over.db path. Must match byte-for-byte on
  every field.
- **5b.** Latency benchmark: run the same 10 queries against both
  paths, table the medians + p95 + p99.
  - Target: `f:<addr>` median < 50ms, p95 < 200ms.
  - Target: `mid:<msg-id>` median < 5ms, p95 < 20ms.
  - Target: `prose_search` (BM25 + over.db hydration) median < 100ms,
    p95 < 500ms.
- **5c.** Memory profile: peak RSS for the same query set.
  - Target: peak RSS for any single query < 500 MB (vs current 2-36 GB).
- **5d.** Concurrent stress: 10 readers + 1 writer (incremental ingest)
  for 5 min. No deadlocks, no corruption, query latency P95 stable.
- **5e.** Correctness for cross-posts: query for a message that exists
  on `lkml` and `netdev`; both rows returned.

### Phase 6 — Documentation + rollout

Estimated: half day

Tasks:

- **6a.** Update `docs/architecture/three-tier-index.md` →
  `four-tier-index.md`; add `over.db` as a tier with the same
  rebuildability contract.
- **6b.** Update `CLAUDE.md` "three-tier index architecture" section
  to reflect the new tier.
- **6c.** New doc `docs/architecture/over-db.md` — schema, query
  patterns, build process, references to public-inbox precedent.
- **6d.** Update `docs/ops/runbook.md` with `kernel-lore-build-over`
  invocation and the 30-min one-pass build expectation.
- **6e.** Update `docs/ops/corpus-coverage.md` disk footprint table
  with over.db size.
- **6f.** Mark Phase 1-4 complete in `TODO.md`.

## Total estimate

~3-4 days of engineering. ~30-45 minutes of one-time over.db build
against the existing 29M-row Parquet metadata (no re-ingest, no
re-BM25, no re-trigram).

## Risk register

| Risk | Mitigation |
|---|---|
| SQLite write contention during incremental ingest | WAL mode + single writer (already serialized via outer flock); readers are non-blocking. |
| `over.db` corrupted by abnormal exit | Atomic build via tempfile+rename; for incremental writes, WAL provides crash safety. |
| Schema migration breaks existing deployments | Schema version in `meta` table; reader checks at open and refuses stale versions. |
| Cross-posted messages double-counted | `INSERT OR REPLACE` keyed on `(message_id, list)`; query layer deduplicates message_ids when caller is "global." |
| over.db size exceeds estimate (>30 GB) | We have 328 GB free on local disk; even 50 GB is fine. |
| Index build time exceeds 30 min | The `CREATE INDEX` happens after all inserts. If too slow, shard the build by list and ATTACH at the end. |
| `from_addr` case-folding loses information | Stored case-folded for the index but the raw value goes into the `ddd` blob, so display preserves original case. |
| msgpack lib choice (`rmp-serde` vs `serde_cbor`) | rmp-serde is the de facto Rust msgpack; well-maintained. CBOR would also work. Locked in §0. |

## Sequencing constraints

- Phases 1, 2, 3 can land independently. Phase 3 requires Phases 1 + 2.
- Phase 4 requires Phase 1.
- Phase 5 requires Phases 2, 3, 4.
- Phase 6 last.

## Out of scope (future work)

- Per-list shards of over.db (currently single file). Easy to add
  later via SQLite ATTACH if file size becomes painful.
- Cross-deployment replication (over.db is per-instance; rebuild
  cheap).
- Adding `over.db`-backed full-text search via FTS5 (we already have
  tantivy; no need).
- Surfacing `over.db` as an MCP resource for analytical SQL queries
  (interesting; deferred).

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
