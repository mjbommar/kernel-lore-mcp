# over.db follow-ups

**Date:** 2026-04-17
**Context:** Plan [`2026-04-17-overdb-metadata-tier.md`](./2026-04-17-overdb-metadata-tier.md)
shipped (Phases 1–6). Validation report at
[`docs/research/2026-04-17-overdb-validation.md`](../research/2026-04-17-overdb-validation.md)
shows every plan target met. This document tracks everything left
on the table — perf, correctness, ops, and known data
inconsistencies — discovered during the build or validation.

Items are ordered by **user-visible impact**, not by ease of fix.

## Hosted-readiness addendum (2026-04-21)

The live full-corpus hosted run on `server6` closed one loop and
opened another:

- The shard-corruption incident was real, and the new
  `kernel-lore-doctor --heal` path fixed it cleanly (16 broken
  shard repos removed; full refetch + ingest completed with
  `failed_shards=0`). The remaining gap is **automatic sync-side
  self-heal**, not further manual-repair tooling.
- The expensive-path posture is now the bigger blocker:
  `lore_regex` on the full corpus timed out even for simple,
  list-scoped patterns, and overload produced the correct
  `query_timeout` + `rate_limited` mix but still proved the tool is
  not public-ready in its current shape.
- Client-side stress also showed a metrics gap: `rate_limited`
  surfaced to callers but did not show up cleanly in `/metrics`,
  and end-to-end request latency inflated under concurrency without
  a matching request-scope histogram on the server side.
- `_cached_corpus_stats` improved from timeout to ~2.2 s once
  warmed, but still needs a generation-bound cache/warm path before
  it is safe on a public box.

These items now roll into `docs/plans/2026-04-20-v0.3.0-plan.md`
Phase 2 (hosted-readiness hardening).

## Performance

### F1. patch_search latency = 68 seconds (SHIPPED 2026-04-19, 6.8x win)

- **Symptom:** `lore_patch_search` MCP tool unusable. Validation §5b
  measured a 68-second response for a 5-result query.
- **Root cause:** `<data_dir>/store/` was a symlink to
  `/nas4/data/kernel-lore-mcp/store/` (NFS). Trigram returned
  candidate message-ids in milliseconds; the confirm step
  decompressed each candidate's zstd-frame body via NFS, paying
  3–10 ms latency per random read.
- **Fix (landed):** rsync the 104 GB store to local NVMe
  (`$HOME/klmcp-store-local`), atomic symlink swap. NAS copy kept
  as a recovery fallback.
- **Measured:** `smb_check_perm_dacl` with `limit=5`: 67 s → 9.8 s
  steady-state (6.8x). Below the plan target of 500 ms — the
  remaining latency is candidate-count × zstd-decompress inside
  the confirm step, a separate bottleneck tracked as a follow-up.
  The NFS portion is gone.
- **Follow-up:** cap candidate count more aggressively on rare-
  identifier queries, or parallelize the confirm decompress loop.

### F2. Non-indexed `eq()` variants fall through to Parquet scan

- **Symptom:** `eq('signed_off_by', addr)`, `eq('touched_files', path)`,
  `eq('fixes', sha)`, `eq('reviewed_by', addr)`, etc. all hit
  `self.scan(...)` and walk every Parquet file. Untimed but
  inferred to be minutes on the 17.6M corpus.
- **Affected fields:** signed_off_by, reviewed_by, acked_by,
  tested_by, co_developed_by, reported_by, suggested_by,
  helped_by, assisted_by, fixes, link, closes, cc_stable,
  touched_files, touched_functions, body_sha256, commit_oid,
  subject_normalized.
- **Root cause:** These all live in the `ddd` blob, not as
  indexed columns. `OverDb::scan_eq` only handles the indexed
  ones (FromAddr, List, MessageId, InReplyTo, Tid).
- **Fix options:**
  - **2a (cheap):** Promote the high-value scalar fields to
    indexed columns: `body_sha256`, `commit_oid`,
    `subject_normalized`. Small schema migration, one-pass UPDATE
    from existing ddd blobs. Restores `eq` parity for these.
  - **2b (medium):** For list-shaped fields (signed_off_by,
    touched_files, fixes), build SQLite "side" tables keyed by
    (value, row_id). Roughly mirrors public-inbox's `xref3` table.
  - **2c (architectural):** Reuse the existing `path_tier`
    Aho-Corasick index for touched_files queries; extend it for
    function names.
- **Effort:** 2a = 1 day. 2b = 2-3 days. 2c = 1-2 days for files,
  more for functions.
- **Acceptance:** All `eq()` variants return in <100 ms p95 on
  the full corpus.

### F3. expand_citation does a Parquet scan even for known Message-IDs

- **Symptom:** `lore_message` MCP tool — citation lookup falls
  through to `self.scan()` even when the token is an obviously
  well-formed Message-ID. Same minute-scale latency as F2.
- **Root cause:** `Reader::expand_citation` was written before
  over.db existed and uses a single scan that handles mid + SHA +
  CVE in one pass.
- **Fix:** Detect token shape early. If `is_message_id_like(token)`
  → `over.get(token)` first; only fall back to scan for SHA prefix
  / CVE substring matching.
- **Effort:** 2 hours.
- **Acceptance:** `expand_citation(mid, ...)` median <10 ms.

### F4. activity by file/function

- **Symptom:** `lore_activity` MCP tool. Untimed but does a
  Parquet scan with a touched_files predicate. Probably
  seconds-to-minutes.
- **Root cause:** touched_files arrays live in the ddd blob, so
  there's no index path that works.
- **Fix:** Same options as F2c (path_tier reuse) for files. For
  functions, either denormalize into a column or build a small
  inverted index in SQLite.
- **Effort:** 1-2 days.
- **Acceptance:** `activity(file/func, since)` median <500 ms.

### F5. series_timeline silently slow when tid is NULL

- **Symptom:** Phase 4 noted: `tid` column in over.db is left
  NULL because `rebuild_tid` writes `tid/tid.parquet` and never
  updates over.db. So `series_timeline` falls back to the slow
  Parquet path.
- **Fix:** Extend `rebuild_tid` (or add a sibling
  `rebuild_tid_in_over`) to UPDATE over.db's `tid` column at the
  end of the cross-corpus pass. The matching message-ids are
  already in hand.
- **Effort:** 2-3 hours.
- **Acceptance:** `series_timeline(mid)` median <50 ms.

### F6. router_search: structured + free-text combo paths

- **Symptom:** Untimed; the router fans out to multiple tiers and
  RRF-merges. Some branches (e.g. `f:bommarito ksmbd`) probably
  still hit Parquet scans inside the eq path.
- **Fix:** Re-audit `src/router.rs::dispatch` after F2/F3/F4 land;
  confirm every branch has a fast path.
- **Effort:** 2 hours of audit + tests.
- **Acceptance:** Mixed-predicate queries median <100 ms p95.

## Correctness

### C1. Cross-post collapse during ingest

- **Symptom:** Validation §5e measured **0 natural cross-posts**
  in the 17.6M-row corpus despite the `over_mid_list` UNIQUE index
  supporting them. Synthetic cross-post probe works correctly,
  proving the read path is fine — the issue is the write path.
- **Root cause hypothesis:** `Reader::scan` applies mtime-DESC
  freshest-wins dedup keyed on `message_id` alone (not
  `(message_id, list)`), collapsing cross-posts before they reach
  over.db.
- **Impact:** `cross_posted_to[]` reconstruction is impossible
  from this corpus. Display is correct (we pick one list); the
  feature is just hollow.
- **Fix:**
  - Audit `Reader::scan`'s dedup key.
  - If confirmed, add a per-list-aware dedup variant for the
    over.db build path.
  - Re-run `kernel-lore-build-over` (~30 min) to materialize
    cross-posts.
- **Effort:** 4 hours (audit + fix + rebuild).

### C2. Validation rename-and-restore protocol crashed the live system

- **Symptom:** During Phase 5, the validation subagent renamed
  `over.db → over.db.parity_bak` to test the Parquet fallback,
  crashed before restoring, left an empty 0-byte over.db. Took
  manual recovery.
- **Fix:** Add a `Reader::new_no_over(data_dir)` API or a
  `KLMCP_DISABLE_OVER=1` env var. Future parity tests compare
  using two Reader instances on the same data dir, no filesystem
  mutation needed.
- **Effort:** 1 hour.

## Operational

### O1. linux-cifs missing from the local corpus

- **Symptom:** `eq list:linux-cifs` returns 0 rows. The user's
  primary list is uncovered.
- **Root cause:** Pre-existing state inconsistency from earlier
  manual data wipes — `state/shards/linux-cifs/0/last_indexed_oid`
  points at a previous run, `metadata/linux-cifs/` is empty,
  incremental ingest skips it as "already done."
- **Fix:**
  ```sh
  rm -rf $KLMCP_DATA_DIR/state/shards/linux-cifs
  kernel-lore-ingest --list linux-cifs --with-over
  ```
- **Effort:** 5 min wall-clock + 5 min compute.
- **Tracked as:** task #112.

### O2. Ingest pipeline writes to NAS by default

- **Symptom:** Default `KLMCP_DATA_DIR=/nas4/...` → store, BM25,
  metadata, trigram all written to NFS. Reads are then catastrophic
  (see F1; same applies to BM25 cold reads).
- **Fix:** Document the local-NVMe deployment shape in
  `docs/ops/runbook.md`. Recommend `data_dir = local NVMe` with
  `shards/` symlinked to NAS for the grokmirror mirror.
- **Effort:** 2 hours of docs + a sample systemd config.

### O3. No incremental over.db rebuild path

- **Symptom:** `kernel-lore-build-over` is full-rebuild only. If
  over.db is corrupted but only one list is bad, you still pay
  the full ~30 min cost.
- **Fix:** Add `--from-list NAME` to `kernel-lore-build-over`
  (already in the CLI per Phase 2; verify it works correctly with
  partial deletes).
- **Effort:** 1 hour to verify + add the partial-delete semantics.

### O4. Re-stress concurrency under post-tuning config

- **Symptom:** Phase 5 §5d measured 382 qps / 305 ms p95 with the
  pre-tuning config (4 GB mmap, single-column from_addr index).
  The post-tuning config (256 MB mmap, composite index) wasn't
  re-stressed.
- **Fix:** Re-run the 60-second 10-thread workload.
- **Effort:** 5 min.
- **Expected:** Median should drop substantially (was dominated by
  slow eq from_addr); p95 should improve.

### O5. CI gate

- **Symptom:** No automated regression test for the lore-scale
  perf targets. We could regress without noticing.
- **Fix:** GH Actions job that builds a 100k-msg fixture, runs
  Phase 5's benchmark suite, asserts each target.
- **Effort:** 1 day (fixture + workflow + targets).

## Validation script reusability

### V1. `ORDER BY RANDOM() LIMIT n` is a full scan

- **Symptom:** Phase 5 sampling took ~1 minute on 17.6M rows.
- **Fix:** Use `WHERE rowid IN (random sample of rowids)` —
  documented gotcha in SQLite community. Sample N integers from
  [1, MAX(rowid)] then point-lookup.
- **Effort:** 15 min in the next benchmark.

## Roadmap rollup (suggested ordering)

| # | Item | Effort | User-visible win |
|---|---|---|---|
| 1 | F1 — store off NFS | 30 min | `patch_search` 68s → ~500 ms |
| 2 | O1 — linux-cifs re-ingest | 10 min | user's main list searchable |
| 3 | F3 — expand_citation fast path | 2 hr | `lore_message` snappy |
| 4 | F5 — tid → over.db | 2 hr | `lore_thread` snappy |
| 5 | F2a — promote scalar fields | 1 day | half the `eq()` variants fast |
| 6 | C1 — cross-post audit | 4 hr | `cross_posted_to[]` works |
| 7 | F4 — activity index | 1-2 days | `lore_activity` snappy |
| 8 | F2b/F2c — list-shaped indexes | 2-3 days | trailer queries fast |
| 9 | O5 — CI gate | 1 day | regression protection |
| 10 | F6 — router re-audit | 2 hr | mixed queries fast |

Doing items 1-4 (~5 hours total) would close the user-visible
perf gap on every common MCP query. Items 5-10 are about
breadth — making more query shapes fast — and CI hygiene.
