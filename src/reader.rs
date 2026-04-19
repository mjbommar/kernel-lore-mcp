//! Metadata tier — read path.
//!
//! Scans all Parquet files under `<data_dir>/metadata/<list>/*.parquet`
//! and exposes small, composable queries the router + MCP tools need:
//!
//!   * `fetch_message(message_id)` — point lookup
//!   * `activity(file|function, since)` — file or function touches over a
//!     date range, grouped by tid (tid null = own group per message)
//!   * `series_timeline(message_id)` — every message with matching
//!     subject_normalized + series_version, ordered by series_index
//!   * `expand_citation(sha_or_mid)` — universal lookup: match
//!     `message_id`, or scan `fixes[]` for a SHA, or scan prose references
//!
//! Scanning discipline: we open every Parquet file under the list dir
//! once per query, apply predicates in-memory (arrow compute kernels),
//! and short-circuit. With per-list directories and row-group stats
//! this is fine at our scale (few hundred MB of Parquet per list).

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use arrow::array::{
    Array, BooleanArray, Int64Array, ListArray, RecordBatch, StringArray, TimestampNanosecondArray,
    UInt32Array, UInt64Array,
};

use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::error::{Error, Result};
use crate::maintainers::MaintainersIndex;
use crate::over::{OverDb, OverDbPool};
use crate::schema as sc;
use crate::state::State;
use crate::timeout::Deadline;

/// Default over.db read-side connection fanout. 3 connections give
/// meaningful concurrency on a 4-vCPU deploy (the `r7g.xlarge` per
/// CLAUDE.md) without the per-connection cache overhead (~200 MB
/// each) piling up. Tunable via `KLMCP_OVER_POOL_SIZE` if a future
/// deployment needs more.
fn over_pool_size() -> usize {
    std::env::var("KLMCP_OVER_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|n| n.max(1))
        .unwrap_or(3)
}

/// One row's worth of metadata, flattened for consumption.
#[derive(Debug, Clone, Default)]
pub struct MessageRow {
    pub message_id: String,
    pub list: String,
    pub shard: String,
    pub commit_oid: String,
    pub from_addr: Option<String>,
    pub from_name: Option<String>,
    pub subject_raw: Option<String>,
    pub subject_normalized: Option<String>,
    pub subject_tags: Vec<String>,
    pub date_unix_ns: Option<i64>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub tid: Option<String>,
    pub series_version: u32,
    pub series_index: Option<u32>,
    pub series_total: Option<u32>,
    pub is_cover_letter: bool,
    pub has_patch: bool,
    pub touched_files: Vec<String>,
    pub touched_functions: Vec<String>,
    pub files_changed: Option<u32>,
    pub insertions: Option<u32>,
    pub deletions: Option<u32>,
    pub signed_off_by: Vec<String>,
    pub reviewed_by: Vec<String>,
    pub acked_by: Vec<String>,
    pub tested_by: Vec<String>,
    pub co_developed_by: Vec<String>,
    pub reported_by: Vec<String>,
    pub fixes: Vec<String>,
    pub link: Vec<String>,
    pub closes: Vec<String>,
    pub cc_stable: Vec<String>,
    pub suggested_by: Vec<String>,
    pub helped_by: Vec<String>,
    pub assisted_by: Vec<String>,
    pub trailers_json: Option<String>,
    pub body_segment_id: u32,
    pub body_offset: u64,
    pub body_length: u64,
    pub body_sha256: String,
    pub schema_version: u32,
}

/// Reader over all Parquet metadata files. Cheap to construct;
/// per-query scans re-open files so we get fresh mmap-backed reads
/// after a writer commit.
///
/// When `<data_dir>/over.db` exists, it is opened lazily at
/// construction time and used for indexed point lookups, equality
/// scans, and post-tantivy / post-trigram hydration. The Parquet
/// scan path is preserved as a graceful fallback for deployments
/// that haven't built `over.db` yet (Phase 3 of the over.db tier).
///
/// `OverDb` wraps a single `rusqlite::Connection`, which is `Send`
/// but not `Sync`. We share it across PyO3-detached worker threads
/// behind an `Arc<Mutex<_>>`. This serializes reads at the
/// SQLite-connection layer; for the latency targets (sub-ms point
/// lookups, sub-100ms BM25 hydration) lock contention is well
/// below the SQLite work itself. If Phase 5 stress tests show the
/// mutex becoming the bottleneck we'll switch to an r2d2 pool.
/// Resolve a MAINTAINERS snapshot from either:
///   1. `$KLMCP_MAINTAINERS_FILE` (explicit absolute path)
///   2. `<data_dir>/MAINTAINERS` (convention: operator drops a snapshot here)
/// Returns `None` when neither exists or parsing fails (logged).
fn load_maintainers(data_dir: &Path) -> Option<Arc<MaintainersIndex>> {
    let env_path = std::env::var_os("KLMCP_MAINTAINERS_FILE").map(std::path::PathBuf::from);
    let candidates: Vec<std::path::PathBuf> = env_path
        .into_iter()
        .chain(std::iter::once(data_dir.join("MAINTAINERS")))
        .collect();
    for path in candidates {
        if !path.exists() {
            continue;
        }
        match MaintainersIndex::parse_file(&path) {
            Ok(idx) => {
                tracing::debug!(
                    path = %path.display(),
                    entries = idx.len(),
                    "Reader: MAINTAINERS index loaded"
                );
                return Some(Arc::new(idx));
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Reader: MAINTAINERS parse failed"
                );
            }
        }
    }
    None
}

/// Hard cap on patch_search candidate union size. A degenerate needle
/// (e.g. a single common trigram, list=None) could otherwise match
/// most of the 17.6M-message corpus and accumulate ~700 MB of
/// message-ids into a HashSet before any confirm step runs.
const MAX_PATCH_CANDIDATES: usize = 100_000;

pub struct Reader {
    data_dir: PathBuf,
    over: Option<Arc<OverDbPool>>,
    /// Per-list read-only Store cache. Patch-search confirmation and
    /// prose-body fetches re-open the same per-list Store on every
    /// query; `Store::open` does `fs::create_dir_all` + `SegmentWriter`
    /// init (reads the dir to find the active segment). Caching
    /// amortizes that across the Reader's lifetime.
    stores: RwLock<HashMap<String, Arc<crate::store::Store>>>,
    /// Lazily-opened BM25 reader. Opening the tantivy `Index` and
    /// constructing an `IndexReader` is hundreds of milliseconds on
    /// a cold OS page cache; doing it on every `prose_search` call
    /// was a hot-path bug. Inner reader holds the shared IndexReader
    /// and a `last_reloaded_generation` atomic — see `bm25::BmReader`.
    bm25: RwLock<Option<Arc<crate::bm25::BmReader>>>,
    /// Parsed MAINTAINERS index. Populated at Reader::new from
    /// `$KLMCP_MAINTAINERS_FILE` or `<data_dir>/MAINTAINERS`. Missing
    /// is fine — tools that need it surface "MAINTAINERS unavailable".
    maintainers: Option<Arc<MaintainersIndex>>,
}

impl Reader {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        let data_dir = data_dir.as_ref().to_owned();
        let over_path = data_dir.join("over.db");
        let over = if over_path.exists() && Self::over_db_is_current(&data_dir) {
            let pool_size = over_pool_size();
            match OverDbPool::open(&over_path, pool_size) {
                Ok(pool) => {
                    tracing::debug!(
                        path = %over_path.display(),
                        pool_size,
                        "Reader: over.db tier enabled"
                    );
                    Some(Arc::new(pool))
                }
                Err(e) => {
                    tracing::warn!(
                        path = %over_path.display(),
                        error = %e,
                        "Reader: over.db present but failed to open pool; falling back to Parquet scan"
                    );
                    None
                }
            }
        } else {
            None
        };
        let maintainers = load_maintainers(&data_dir);
        Self {
            data_dir,
            over,
            stores: RwLock::new(HashMap::new()),
            bm25: RwLock::new(None),
            maintainers,
        }
    }

    /// Expose the parsed MAINTAINERS index for the `lore_maintainer_profile`
    /// tool. `None` when no snapshot is configured.
    pub fn maintainers(&self) -> Option<&MaintainersIndex> {
        self.maintainers.as_deref()
    }

    /// Return a shared `BmReader`, opening (and caching) it on first
    /// call. Subsequent calls return the same handle; each call also
    /// triggers a `maybe_reload` so the reader picks up new segments
    /// whenever the generation file has advanced since the last query.
    fn bm25(&self) -> Result<Arc<crate::bm25::BmReader>> {
        if let Ok(guard) = self.bm25.read()
            && let Some(ref r) = *guard
        {
            let r = Arc::clone(r);
            if let Ok(gen_val) = self.generation() {
                r.maybe_reload(gen_val)?;
            }
            return Ok(r);
        }
        let mut guard = self
            .bm25
            .write()
            .map_err(|_| Error::State("bm25 cache poisoned".to_owned()))?;
        if let Some(ref r) = *guard {
            let r = Arc::clone(r);
            if let Ok(gen_val) = self.generation() {
                r.maybe_reload(gen_val)?;
            }
            return Ok(r);
        }
        let fresh = Arc::new(crate::bm25::BmReader::open(&self.data_dir)?);
        if let Ok(gen_val) = self.generation() {
            fresh.maybe_reload(gen_val)?;
        }
        *guard = Some(Arc::clone(&fresh));
        Ok(fresh)
    }

    /// Return an `Arc<Store>` for `list`, opening it on first access
    /// and caching the handle for subsequent query-path reads. The
    /// Store's internal `SegmentWriter` is unused on this path — read
    /// queries only call `read_at`. Safe to share across threads:
    /// `Store::read_at` opens a fresh `File` per call and doesn't
    /// touch the writer lock.
    fn store_for(&self, list: &str) -> Result<Arc<crate::store::Store>> {
        // Fast path: reader lock, clone out the Arc.
        if let Ok(guard) = self.stores.read()
            && let Some(s) = guard.get(list)
        {
            return Ok(Arc::clone(s));
        }
        // Slow path: upgrade to writer, check-then-insert so a racing
        // second caller doesn't open a duplicate Store.
        let mut guard = self
            .stores
            .write()
            .map_err(|_| Error::State("store cache poisoned".to_owned()))?;
        if let Some(s) = guard.get(list) {
            return Ok(Arc::clone(s));
        }
        let store = Arc::new(crate::store::Store::open(&self.data_dir, list)?);
        guard.insert(list.to_owned(), Arc::clone(&store));
        Ok(store)
    }

    /// Check whether over.db's per-tier generation marker matches the
    /// corpus generation. If ingest wrote Parquet successfully but the
    /// over.db insert_batch failed, the main generation advances while
    /// the over.db marker stays behind — readers MUST bypass over.db
    /// in that window or they'll return silently-incomplete results.
    ///
    /// Returns `true` when it's safe to use over.db:
    ///   * marker exists AND matches (or exceeds) the corpus generation;
    ///   * marker file does NOT exist — a legacy deployment ingested
    ///     before per-tier markers shipped. We honor backward-compat
    ///     and trust over.db; the next ingest with the new code will
    ///     start writing markers and kick in strict checking;
    ///   * the corpus generation is 0 (fresh data_dir, nothing to
    ///     be stale against).
    ///
    /// Returns `false` only when the marker is PRESENT and behind.
    /// That's a positive signal of drift, not a missing/unknown
    /// state. Any read error also returns `false` (fail safe).
    fn over_db_is_current(data_dir: &Path) -> bool {
        let Ok(state) = State::new(data_dir) else {
            return false;
        };
        let corpus_gen = match state.generation() {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(error = %e, "Reader: cannot read corpus generation; disabling over.db");
                return false;
            }
        };
        if corpus_gen == 0 {
            return true;
        }
        match state.tier_generation("over") {
            Ok(None) => {
                // Legacy deployment — no marker file. Trust over.db;
                // operators running the new ingest will get strict
                // marker-based coherence once the first post-upgrade
                // ingest completes.
                true
            }
            Ok(Some(over_gen)) if over_gen >= corpus_gen => true,
            Ok(Some(over_gen)) => {
                tracing::warn!(
                    over_gen,
                    corpus_gen,
                    "Reader: over.db generation behind corpus; disabling over.db until next ingest reconciles"
                );
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, "Reader: cannot read over.generation; disabling over.db");
                false
            }
        }
    }

    /// Borrow the optional over.db handle. `None` when the data dir
    /// has no `over.db` (graceful fallback to the legacy Parquet scan).
    fn over_db(&self) -> Option<&Arc<OverDbPool>> {
        self.over.as_ref()
    }

    /// Run `f` against one of the over.db pool connections. Returns
    /// `None` when the tier is disabled; callers fall through to the
    /// Parquet scan path. A pool mutex-poison is propagated as
    /// `Some(Err(...))` so it surfaces rather than silently skipping.
    fn with_over<T, F>(&self, f: F) -> Option<Result<T>>
    where
        F: FnOnce(&OverDb) -> Result<T>,
    {
        let pool = self.over.as_ref()?;
        Some(pool.with_conn(f))
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Current index generation counter. Mirrors `State::generation()`;
    /// duplicated here so query-path callers don't need the full
    /// ingest-side `State` wrapper.
    pub fn generation(&self) -> Result<u64> {
        let path = self.data_dir.join("state").join("generation");
        match fs::read_to_string(&path) {
            Ok(s) => s
                .trim()
                .parse::<u64>()
                .map_err(|e| crate::error::Error::State(format!("generation parse: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Last-mutation time of the generation file, in nanoseconds since
    /// the Unix epoch (UTC). `None` if the file has never been written
    /// (fresh data_dir).
    pub fn generation_mtime_ns(&self) -> Result<Option<i64>> {
        let path = self.data_dir.join("state").join("generation");
        match fs::metadata(&path) {
            Ok(md) => {
                let mtime = md.modified()?;
                let dur = mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| crate::error::Error::State(format!("mtime pre-epoch: {e}")))?;
                Ok(Some(dur.as_nanos() as i64))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Enumerate every `.parquet` file under `<data_dir>/metadata/`.
    ///
    /// Files are sorted by **mtime descending** so the most recently
    /// written file comes first. This is load-bearing: `scan()`
    /// deduplicates by message_id and keeps the first occurrence, so
    /// mtime-descending guarantees freshest-wins regardless of what
    /// `run_id` the caller passed (run_id is caller-controlled on the
    /// PyO3 surface and only happens to be monotone in the default
    /// CLI path). File mtime is set by `fs::rename` in
    /// `metadata::write_parquet` (the atomic-rename step) and is
    /// monotone with real wall-clock time.
    fn parquet_files(&self) -> Result<Vec<PathBuf>> {
        let root = self.data_dir.join("metadata");
        let mut out: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        if !root.exists() {
            return Ok(Vec::new());
        }
        for list_entry in fs::read_dir(&root)? {
            let list_entry = list_entry?;
            if !list_entry.file_type()?.is_dir() {
                continue;
            }
            for file in fs::read_dir(list_entry.path())? {
                let file = file?;
                let path = file.path();
                if path.extension().and_then(|s| s.to_str()) == Some("parquet") {
                    let mtime = file
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH);
                    out.push((path, mtime));
                }
            }
        }
        // Sort by mtime descending — newest first. Deterministic
        // regardless of run_id naming conventions.
        out.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(out.into_iter().map(|(p, _)| p).collect())
    }

    /// Collect every row in the metadata tier into `out`. Used by the
    /// tid-rebuild pass which needs the full corpus in memory.
    pub fn scan_all(&self, out: &mut Vec<MessageRow>) -> Result<()> {
        self.scan(
            |_| true,
            |r| {
                out.push(r);
                true
            },
        )
    }

    /// Streaming variant of `scan_all`. Invokes `visit` for every row
    /// without ever materializing the full corpus in memory. Honors
    /// the same dedup-by-message_id (mtime-DESC, freshest-wins) and
    /// optional `list` filter the rest of the read path uses.
    ///
    /// `visit` returns `true` to continue, `false` to stop early.
    /// Used by the `kernel-lore-build-over` binary, which would
    /// otherwise OOM trying to hold 29M rows.
    pub fn scan_streaming<V>(&self, list: Option<&str>, visit: V) -> Result<()>
    where
        V: FnMut(MessageRow) -> bool,
    {
        let want_list = list.map(|s| s.to_owned());
        self.scan(
            move |r| match &want_list {
                Some(l) => &r.list == l,
                None => true,
            },
            visit,
        )
    }

    /// Collect all rows with optional list + since filters.
    ///
    /// `limit` caps the returned row count. `None` uses the safety
    /// default of 1M — the uncapped Parquet path would OOM on a
    /// 17.6M-row corpus. Callers should pass `Some(n)` with a tight
    /// bound whenever they have one.
    pub fn all_rows(
        &self,
        list: Option<&str>,
        since_unix_ns: Option<i64>,
        limit: Option<usize>,
    ) -> Result<Vec<MessageRow>> {
        const DEFAULT_CAP: usize = 1_000_000;
        let cap = limit.unwrap_or(DEFAULT_CAP);

        if let Some(l) = list
            && let Some(res) =
                self.with_over(|db| db.scan_eq(EqField::List, l, since_unix_ns, None, cap))
        {
            return res;
        }

        let mut out = Vec::new();
        self.scan(
            |_| true,
            |r| {
                if let Some(l) = list {
                    if r.list != l {
                        return true;
                    }
                }
                if let Some(since) = since_unix_ns {
                    if let Some(d) = r.date_unix_ns {
                        if d < since {
                            return true;
                        }
                    }
                }
                out.push(r);
                out.len() < cap
            },
        )?;
        Ok(out)
    }

    /// Read the tid side-table at `<data_dir>/tid/tid.parquet` into a
    /// `message_id -> tid` map. Returns empty if the side-table
    /// hasn't been built yet.
    ///
    /// **Memory warning:** materializes every (mid, tid) pair —
    /// ~1.8 GB on a 17.6M-row corpus. Intended for tests and debug
    /// tooling only. Production callers should use the `over_tid`
    /// index via `scan_eq(EqField::Tid, ...)`.
    pub fn tid_lookup(&self) -> Result<std::collections::HashMap<String, String>> {
        let path = self.data_dir.join("tid").join("tid.parquet");
        if !path.exists() {
            return Ok(std::collections::HashMap::new());
        }
        let file = File::open(&path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        let reader = builder.build()?;
        let mut out = std::collections::HashMap::new();
        for batch in reader {
            let batch = batch?;
            let mid = downcast_string(&batch, "message_id")?;
            let tid = downcast_string(&batch, "tid")?;
            for i in 0..batch.num_rows() {
                out.insert(mid.value(i).to_owned(), tid.value(i).to_owned());
            }
        }
        Ok(out)
    }

    /// Read the tid side-table propagated_files / propagated_functions
    /// columns into a `message_id -> (files, functions)` map.
    ///
    /// **Memory warning:** same scale hazard as `tid_lookup` — easily
    /// 3-5 GB at 17.6M-row scale. Test/debug only.
    #[allow(clippy::type_complexity)]
    pub fn propagated_lookup(
        &self,
    ) -> Result<std::collections::HashMap<String, (Vec<String>, Vec<String>)>> {
        let path = self.data_dir.join("tid").join("tid.parquet");
        if !path.exists() {
            return Ok(std::collections::HashMap::new());
        }
        let file = File::open(&path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        let reader = builder.build()?;
        let mut out = std::collections::HashMap::new();
        for batch in reader {
            let batch = batch?;
            let mid = downcast_string(&batch, "message_id")?;
            let files = downcast_list(&batch, "propagated_files")?;
            let funcs = downcast_list(&batch, "propagated_functions")?;
            for i in 0..batch.num_rows() {
                out.insert(
                    mid.value(i).to_owned(),
                    (list_strings(&files, i), list_strings(&funcs, i)),
                );
            }
        }
        Ok(out)
    }

    /// Apply `visit` to every row matching `filter`. Short-circuits when
    /// `visit` returns false.
    /// Core scan: walks every Parquet file, deduplicates by message_id.
    ///
    /// Because `parquet_files()` returns files in descending filename
    /// order (newest run_id first), the first occurrence of each
    /// message_id is the freshest. Subsequent duplicates (from
    /// dangling-OID re-walks) are skipped. This makes the "freshest
    /// row wins" contract from the ingest docs enforceable end-to-end
    /// without a separate dedup pass.
    fn scan<F, V>(&self, mut filter: F, mut visit: V) -> Result<()>
    where
        F: FnMut(&MessageRow) -> bool,
        V: FnMut(MessageRow) -> bool,
    {
        let mut seen = std::collections::HashSet::<String>::new();
        for path in self.parquet_files()? {
            let file = File::open(&path)?;
            let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
            let reader = builder.build()?;
            for batch in reader {
                // Request-budget check at batch boundary. A no-op on
                // threads without a DeadlineGuard (tests, offline
                // tools); long-running tool entries install a guard
                // so adversarial scans terminate promptly instead
                // of wedging the Rust thread pool while Python
                // abandons the future.
                crate::timeout::check_request_deadline()?;
                let batch = batch?;
                let rows = materialize_batch(&batch)?;
                for row in rows {
                    if !seen.insert(row.message_id.clone()) {
                        continue; // duplicate — skip
                    }
                    if !filter(&row) {
                        continue;
                    }
                    if !visit(row) {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    /// Point lookup by Message-ID (across all lists).
    pub fn fetch_message(&self, message_id: &str) -> Result<Option<MessageRow>> {
        let needle = strip_angles(message_id).to_owned();
        // over.db: indexed point lookup by message_id (sub-ms typical).
        // Cross-posts collapse to the freshest by date_unix_ns inside
        // OverDb::get, matching the mtime-DESC + dedup behavior the
        // legacy Parquet scan provided.
        //
        // Fall-through contract: a `Some(Ok(None))` from over.db means
        // "row not in over.db", not "row not in the corpus". A partial
        // ingest (Parquet success + over.db insert failure) leaves
        // rows visible in Parquet but absent from over.db. Return the
        // over.db hit when one exists; otherwise fall through to the
        // Parquet scan so we don't silently swallow real rows.
        if let Some(res) = self.with_over(|db| db.get(&needle)) {
            match res? {
                Some(row) => return Ok(Some(row)),
                None => {
                    // Miss — fall through. The Reader-open check guards
                    // against long-lived staleness; this guards against
                    // a per-row inconsistency within a single generation.
                }
            }
        }
        let mut found: Option<MessageRow> = None;
        self.scan(
            |r| r.message_id == needle,
            |r| {
                found = Some(r);
                false
            },
        )?;
        Ok(found)
    }

    /// Return every row whose `touched_files` or `touched_functions`
    /// matches, with optional date lower-bound. `file` and `function`
    /// are ANDed when both are Some.
    pub fn activity(
        &self,
        file: Option<&str>,
        function: Option<&str>,
        since_unix_ns: Option<i64>,
        list: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        let f_path = file.map(str::to_owned);
        let f_func = function.map(str::to_owned);
        let list_filter = list.map(str::to_owned);

        // over.db: when scoped to a single list (the dominant query
        // shape from `lore_activity` / router), use the
        // `over_list_date` index to pull just that list's rows in
        // date-DESC order, then filter on touched_files / touched_functions
        // (which live in the zstd-compressed ddd blob — decoded as part
        // of MessageRow materialization, no extra round trips).
        //
        // We over-fetch to compensate for in-memory predicate selectivity:
        // most rows in a list don't touch any given file. The 4096 cap
        // matches the maximum reasonable activity window without
        // pulling enough rows to blow query budget.
        if let Some(l) = &list_filter
            && let Some(res) = self.with_over(|db| {
                let scan_limit = limit.saturating_mul(64).max(4_096);
                db.scan_eq(EqField::List, l, since_unix_ns, None, scan_limit)
            })
        {
            let rows = res?;
            let mut out: Vec<MessageRow> = rows
                .into_iter()
                .filter(|r| {
                    if let Some(ref p) = f_path
                        && !r.touched_files.iter().any(|x| x == p)
                    {
                        return false;
                    }
                    if let Some(ref fn_) = f_func
                        && !r.touched_functions.iter().any(|x| x == fn_)
                    {
                        return false;
                    }
                    true
                })
                .take(limit)
                .collect();
            // scan_eq already returned date-DESC; re-sort defensively
            // in case future filtering changes the order.
            out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
            return Ok(out);
        }

        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref lst) = list_filter {
                    if &r.list != lst {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                if let Some(ref p) = f_path {
                    if !r.touched_files.iter().any(|x| x == p) {
                        return false;
                    }
                }
                if let Some(ref fn_) = f_func {
                    if !r.touched_functions.iter().any(|x| x == fn_) {
                        return false;
                    }
                }
                true
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        // Newest first.
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }

    /// Given any message-id in a series, return all sibling messages
    /// sharing the same normalized subject, ordered by
    /// `(series_version, series_index)`.
    pub fn series_timeline(&self, message_id: &str) -> Result<Vec<MessageRow>> {
        let Some(seed) = self.fetch_message(message_id)? else {
            return Ok(Vec::new());
        };
        let Some(subj) = seed.subject_normalized.clone() else {
            return Ok(vec![seed]);
        };
        let list = seed.list.clone();
        let from = seed.from_addr.clone();

        // over.db fast path: use the indexed `tid` column directly.
        // After rebuild_tid backfill, seed.tid is populated and the
        // over_tid index makes sibling-lookup O(thread size). The
        // previous implementation called `tid_lookup()`, which loads
        // the entire 17.6M-entry tid.parquet into a HashMap (~1.8 GB)
        // on every query.
        if let Some(seed_tid) = seed.tid.clone().filter(|t| !t.is_empty())
            && let Some(res) = self.with_over(|db| {
                db.scan_eq(EqField::Tid, &seed_tid, None, None, 10_000)
            })
        {
            let siblings = res?;
            let mut out: Vec<MessageRow> = siblings
                .into_iter()
                .filter(|r| {
                    r.list == list
                        && r.subject_normalized.as_deref() == Some(subj.as_str())
                        && r.from_addr == from
                })
                .collect();
            out.sort_by_key(|r| (r.series_version, r.series_index.unwrap_or(0)));
            return Ok(out);
        }
        // Falls through if over.db's tid column hasn't been backfilled
        // yet (rebuild_tid hasn't run). Legacy Parquet scan still works.

        let mut out = Vec::new();
        self.scan(
            |r| {
                r.list == list
                    && r.subject_normalized.as_deref() == Some(subj.as_str())
                    && r.from_addr == from
            },
            |r| {
                out.push(r);
                true
            },
        )?;
        out.sort_by_key(|r| (r.series_version, r.series_index.unwrap_or(0)));
        Ok(out)
    }

    // ---- low-level retrieval primitives (Phase 7) -------------------
    //
    // Each method below is one well-defined query against one tier.
    // The MCP layer exposes them as composable tools; agents stack
    // them themselves rather than us inventing higher-order workflows
    // for every new question.

    /// Exact-equality scan over one structured metadata column.
    ///
    /// `field` selects the column; `value` is matched verbatim
    /// (case-sensitive). For list-shaped columns (`touched_files`,
    /// `signed_off_by`, ...), the row matches if `value` appears in
    /// the list. `since_unix_ns` and `list` are global filters.
    pub fn eq(
        &self,
        field: EqField,
        value: &str,
        since_unix_ns: Option<i64>,
        list_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        // over.db indexed routes: MessageId, FromAddr, List, InReplyTo,
        // Tid. For everything else we leave the Parquet path in place
        // (over.db's sequential scan would also work, but the plan
        // explicitly defers it to keep this change small + bisectable).
        if eq_field_is_over_indexed(field)
            && let Some(res) = self.with_over(|db| {
                db.scan_eq(field, value, since_unix_ns, list_filter, limit)
            })
        {
            return res;
        }

        let value_owned = value.to_owned();
        let list_owned = list_filter.map(str::to_owned);
        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned {
                    if &r.list != l {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                eq_field_matches(field, r, &value_owned)
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }

    /// `WHERE field IN (values)` — set-membership over one column.
    pub fn in_list(
        &self,
        field: EqField,
        values: &[String],
        since_unix_ns: Option<i64>,
        list_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        let want: std::collections::HashSet<String> = values.iter().cloned().collect();
        let list_owned = list_filter.map(str::to_owned);
        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned {
                    if &r.list != l {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                want.iter().any(|v| eq_field_matches(field, r, v))
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }

    /// Aggregate counts over the same predicate language as `eq`.
    /// Single full scan; cheap relative to materializing rows.
    pub fn count(
        &self,
        field: EqField,
        value: &str,
        since_unix_ns: Option<i64>,
        list_filter: Option<&str>,
    ) -> Result<CountSummary> {
        let value_owned = value.to_owned();
        let list_owned = list_filter.map(str::to_owned);
        let mut summary = CountSummary::default();
        let mut authors: std::collections::HashSet<String> = std::collections::HashSet::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned {
                    if &r.list != l {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                eq_field_matches(field, r, &value_owned)
            },
            |r| {
                summary.count += 1;
                if let Some(addr) = &r.from_addr {
                    authors.insert(addr.clone());
                }
                if let Some(d) = r.date_unix_ns {
                    summary.earliest_unix_ns = match summary.earliest_unix_ns {
                        Some(e) => Some(e.min(d)),
                        None => Some(d),
                    };
                    summary.latest_unix_ns = match summary.latest_unix_ns {
                        Some(l) => Some(l.max(d)),
                        None => Some(d),
                    };
                }
                true
            },
        )?;
        summary.distinct_authors = authors.len() as u64;
        Ok(summary)
    }

    /// Aggregate profile for messages authored by `addr`.
    ///
    /// Samples the most recent `limit` messages where `from_addr`
    /// matches (via the indexed over.db path — sub-millisecond lookup,
    /// fast for even the most prolific kernel authors). All trailer
    /// aggregation happens in memory from the decoded MessageRow
    /// payloads, so numbers are honest for the sampled window.
    ///
    /// For a prolific author (e.g. gregkh at ~500k messages) a
    /// sample of 10 000 gives a representative view of recent
    /// activity without loading the whole history.
    pub fn author_profile(
        &self,
        addr: &str,
        list_filter: Option<&str>,
        since_unix_ns: Option<i64>,
        limit: usize,
    ) -> Result<AuthorProfile> {
        self.author_profile_inner(addr, list_filter, since_unix_ns, limit, false, 0)
    }

    /// Same as `author_profile`, but optionally ALSO aggregates rows
    /// where `addr` appears in any trailer (reviewed_by / acked_by /
    /// ...) on someone else's patch. The expanded view matches what
    /// a full-text search on lore would show; the cost is one extra
    /// Parquet scan bounded by `mention_limit`.
    pub fn author_profile_extended(
        &self,
        addr: &str,
        list_filter: Option<&str>,
        since_unix_ns: Option<i64>,
        limit: usize,
        include_mentions: bool,
        mention_limit: usize,
    ) -> Result<AuthorProfile> {
        self.author_profile_inner(
            addr,
            list_filter,
            since_unix_ns,
            limit,
            include_mentions,
            mention_limit,
        )
    }

    fn author_profile_inner(
        &self,
        addr: &str,
        list_filter: Option<&str>,
        since_unix_ns: Option<i64>,
        limit: usize,
        include_mentions: bool,
        mention_limit: usize,
    ) -> Result<AuthorProfile> {
        let mut rows = self.eq(EqField::FromAddr, addr, since_unix_ns, list_filter, limit)?;
        let authored_count = rows.len() as u64;
        let limit_hit = (rows.len() >= limit) && limit > 0;

        // Dedup set keyed by message_id so mentions don't double-count
        // rows we already have as authored.
        let mut seen: std::collections::HashSet<String> =
            rows.iter().map(|r| r.message_id.clone()).collect();
        let mut mentions_count: u64 = 0;
        let mut mention_limit_hit = false;

        if include_mentions && mention_limit > 0 {
            let mention_rows = self.trailer_mentions(
                addr,
                list_filter,
                since_unix_ns,
                mention_limit,
            )?;
            mention_limit_hit = mention_rows.len() >= mention_limit;
            for r in mention_rows {
                if seen.insert(r.message_id.clone()) {
                    rows.push(r);
                    mentions_count += 1;
                }
            }
        }

        let sampled = rows.len() as u64;

        let mut out = AuthorProfile {
            addr_queried: addr.to_owned(),
            sampled,
            limit_hit: limit_hit || mention_limit_hit,
            authored_count,
            mention_count: mentions_count,
            ..AuthorProfile::default()
        };

        let mut subject_set: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut subsystems: HashMap<String, SubsystemBucket> = HashMap::new();

        for row in &rows {
            if row.has_patch {
                out.patches_with_content += 1;
            }
            if row.is_cover_letter {
                out.cover_letters += 1;
            }
            if !row.fixes.is_empty() {
                out.with_fixes_trailer += 1;
                out.own_trailers.fixes_issued += 1;
            }
            if !row.signed_off_by.is_empty() {
                out.own_trailers.signed_off_by_present += 1;
            }
            if !row.reviewed_by.is_empty() {
                out.received_trailers.reviewed_by += 1;
            }
            if !row.acked_by.is_empty() {
                out.received_trailers.acked_by += 1;
            }
            if !row.tested_by.is_empty() {
                out.received_trailers.tested_by += 1;
            }
            if !row.co_developed_by.is_empty() {
                out.received_trailers.co_developed_by += 1;
            }
            if !row.reported_by.is_empty() {
                out.received_trailers.reported_by += 1;
            }
            if !row.cc_stable.is_empty() {
                out.received_trailers.cc_stable += 1;
            }
            if let Some(s) = row.subject_normalized.as_deref() {
                subject_set.insert(s.to_owned());
            }
            if let Some(d) = row.date_unix_ns {
                out.oldest_unix_ns = Some(out.oldest_unix_ns.map_or(d, |e| e.min(d)));
                out.newest_unix_ns = Some(out.newest_unix_ns.map_or(d, |e| e.max(d)));
            }

            let sub = subsystems
                .entry(row.list.clone())
                .or_insert_with(|| SubsystemBucket {
                    list: row.list.clone(),
                    ..SubsystemBucket::default()
                });
            sub.patches += 1;
            if let Some(d) = row.date_unix_ns {
                sub.oldest_unix_ns = Some(sub.oldest_unix_ns.map_or(d, |e| e.min(d)));
                sub.newest_unix_ns = Some(sub.newest_unix_ns.map_or(d, |e| e.max(d)));
            }
        }

        out.unique_subjects = subject_set.len() as u64;
        let mut subs: Vec<SubsystemBucket> = subsystems.into_values().collect();
        subs.sort_by_key(|s| std::cmp::Reverse(s.patches));
        out.subsystems = subs;

        Ok(out)
    }

    /// Find messages that MENTION `addr` in any common trailer
    /// (signed_off_by, reviewed_by, acked_by, tested_by,
    /// co_developed_by, reported_by, suggested_by, helped_by,
    /// assisted_by). One Parquet scan — not one per trailer kind.
    ///
    /// `author_profile(include_mentions=true)` uses this to expand
    /// beyond "messages you authored" to "messages that name you".
    /// Inherently O(corpus) without a reverse-trailer index; narrow
    /// with `list_filter` + `since_unix_ns` when possible.
    pub fn trailer_mentions(
        &self,
        addr: &str,
        list_filter: Option<&str>,
        since_unix_ns: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        let needle_lc = addr.to_ascii_lowercase();
        let list_owned = list_filter.map(str::to_owned);
        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned
                    && &r.list != l
                {
                    return false;
                }
                if let Some(t) = since_unix_ns
                    && r.date_unix_ns.is_some_and(|d| d < t)
                {
                    return false;
                }
                any_trailer_contains_email(r, &needle_lc)
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }

    /// Cross-reference a kernel path against the MAINTAINERS file and
    /// observed lore activity. Declared = MAINTAINERS M:/R: for the
    /// subsystem(s) claiming this path. Observed = who has actually
    /// reviewed / acked / tested patches touching this path inside
    /// the window. The gap between the two (stale_declared,
    /// active_unlisted) is the most useful signal.
    pub fn maintainer_profile(
        &self,
        path: &str,
        window_days: u32,
        activity_limit: usize,
    ) -> Result<MaintainerProfile> {
        let mut out = MaintainerProfile {
            path_queried: path.to_owned(),
            maintainers_available: self.maintainers.is_some(),
            ..MaintainerProfile::default()
        };

        if let Some(idx) = self.maintainers.as_deref() {
            let hits = idx.lookup(path);
            for entry in hits {
                out.declared.push(DeclaredEntry {
                    name: entry.name.clone(),
                    status: entry.status.clone(),
                    lists: entry.lists.clone(),
                    maintainers: entry.maintainers.clone(),
                    reviewers: entry.reviewers.clone(),
                    depth: entry.depth() as u32,
                });
            }
        }

        // Gather observed activity on this path within the window.
        // `activity` routes through the indexed list-date scan when
        // a list_filter is present; without one it does a metadata
        // scan filtered on touched_files (post-filter). Good enough
        // for a single-path profile.
        let since_unix_ns = if window_days > 0 {
            let secs = (window_days as u64) * 24 * 3600;
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(i64::MAX);
            Some(now_ns - (secs as i64) * 1_000_000_000)
        } else {
            None
        };
        let rows = self.activity(Some(path), None, since_unix_ns, None, activity_limit)?;
        out.sampled_patches = rows.len() as u64;

        // Aggregate observed trailer counts per address.
        let mut observed: HashMap<String, ObservedAddr> = HashMap::new();
        let record = |map: &mut HashMap<String, ObservedAddr>,
                      raw: &str,
                      kind: TrailerKind,
                      date_ns: Option<i64>| {
            let email = extract_email(raw);
            if email.is_empty() {
                return;
            }
            let entry = map.entry(email.clone()).or_insert_with(|| ObservedAddr {
                email,
                ..Default::default()
            });
            match kind {
                TrailerKind::ReviewedBy => entry.reviewed_by += 1,
                TrailerKind::AckedBy => entry.acked_by += 1,
                TrailerKind::TestedBy => entry.tested_by += 1,
                TrailerKind::SignedOffBy => entry.signed_off_by += 1,
            }
            if let Some(d) = date_ns {
                entry.last_seen_unix_ns =
                    Some(entry.last_seen_unix_ns.map_or(d, |e| e.max(d)));
            }
        };
        for row in &rows {
            for r in &row.reviewed_by {
                record(&mut observed, r, TrailerKind::ReviewedBy, row.date_unix_ns);
            }
            for a in &row.acked_by {
                record(&mut observed, a, TrailerKind::AckedBy, row.date_unix_ns);
            }
            for t in &row.tested_by {
                record(&mut observed, t, TrailerKind::TestedBy, row.date_unix_ns);
            }
            for s in &row.signed_off_by {
                record(&mut observed, s, TrailerKind::SignedOffBy, row.date_unix_ns);
            }
        }

        // Declared address set (normalized to email-only lowercase).
        let mut declared_emails: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for entry in &out.declared {
            for s in entry.maintainers.iter().chain(entry.reviewers.iter()) {
                let e = extract_email(s);
                if !e.is_empty() {
                    declared_emails.insert(e);
                }
            }
        }

        // Stale: declared addresses with ZERO observed activity in window.
        let mut stale: Vec<String> = Vec::new();
        for addr in &declared_emails {
            if !observed.contains_key(addr) {
                stale.push(addr.clone());
            }
        }
        stale.sort();
        out.stale_declared = stale;

        // Active-unlisted: observed reviewers NOT in the declared set.
        // Rank by reviews + acks (tests and sobs are weaker signal).
        let mut unlisted: Vec<ObservedAddr> = observed
            .values()
            .filter(|o| !declared_emails.contains(&o.email))
            .cloned()
            .collect();
        unlisted.sort_by_key(|o| std::cmp::Reverse(o.reviewed_by + o.acked_by));
        unlisted.truncate(20);
        out.active_unlisted = unlisted;

        // All observed (top by activity) for the full picture.
        let mut all_observed: Vec<ObservedAddr> = observed.into_values().collect();
        all_observed.sort_by_key(|o| {
            std::cmp::Reverse(o.reviewed_by + o.acked_by + o.tested_by)
        });
        all_observed.truncate(50);
        out.observed = all_observed;

        Ok(out)
    }

    /// Case-insensitive byte substring scan over `subject_raw`.
    /// Cheap because subjects are short; one full metadata scan.
    pub fn substr_subject(
        &self,
        needle: &str,
        list_filter: Option<&str>,
        since_unix_ns: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        let needle_lc = needle.to_ascii_lowercase();
        let list_owned = list_filter.map(str::to_owned);
        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned {
                    if &r.list != l {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                r.subject_raw
                    .as_ref()
                    .map(|s| s.to_ascii_lowercase().contains(&needle_lc))
                    .unwrap_or(false)
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }

    /// Substring scan inside one named trailer column.
    ///
    /// `name` is the trailer kind (case-insensitive): "fixes", "link",
    /// "reviewed-by", "acked-by", "tested-by", "signed-off-by",
    /// "co-developed-by", "reported-by", "closes", "cc-stable".
    /// `value_substring` is matched case-insensitively against any
    /// value in the column.
    pub fn substr_trailers(
        &self,
        name: &str,
        value_substring: &str,
        list_filter: Option<&str>,
        since_unix_ns: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        let name_lc = name.to_ascii_lowercase();
        let needle_lc = value_substring.to_ascii_lowercase();
        let list_owned = list_filter.map(str::to_owned);
        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned {
                    if &r.list != l {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                trailer_matches(r, &name_lc, &needle_lc)
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }

    /// DFA-only regex scan over one of {subject_raw, from_addr,
    /// body_prose, patch}. `body_prose` and `patch` require fetching
    /// the body from the compressed store; subject + from are scanned
    /// straight from the metadata tier.
    ///
    /// `anchor_required=true` rejects patterns starting with `.*` —
    /// keeps the trigram filter (when we add it) honest. v0.5 fully
    /// scans, so anchoring is policy not performance.
    pub fn regex(
        &self,
        field: RegexField,
        pattern: &str,
        anchor_required: bool,
        list_filter: Option<&str>,
        since_unix_ns: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        if anchor_required && (pattern.starts_with(".*") || pattern.starts_with("^.*")) {
            return Err(crate::error::Error::RegexComplexity(
                "anchored-only mode rejected leading `.*` — narrow the pattern \
                 (prefix anchor, list:/since: filter) or pass anchor_required=false"
                    .to_owned(),
            ));
        }
        // DFA build via regex-automata. Reject non-DFA-able patterns
        // (backrefs, lookaround) by using the dense::DFA::new builder
        // which only supports a safe subset.
        use regex_automata::dfa::dense;
        use regex_automata::util::syntax;
        let dfa = dense::DFA::builder()
            .syntax(syntax::Config::new().unicode(false).utf8(false))
            .build(pattern)
            .map_err(|e| {
                crate::error::Error::RegexComplexity(format!(
                    "pattern not DFA-buildable (backrefs / lookaround / size limit): {e}"
                ))
            })?;

        let list_owned = list_filter.map(str::to_owned);
        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned {
                    if &r.list != l {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                match field {
                    RegexField::Subject => r
                        .subject_raw
                        .as_deref()
                        .map(|s| dfa_search(&dfa, s.as_bytes()))
                        .unwrap_or(false),
                    RegexField::FromAddr => r
                        .from_addr
                        .as_deref()
                        .map(|s| dfa_search(&dfa, s.as_bytes()))
                        .unwrap_or(false),
                    RegexField::Prose | RegexField::Patch => true, // confirm via body fetch below
                }
            },
            |r| {
                out.push(r);
                out.len() < limit * 4 // gather extra for body confirm pass
            },
        )?;

        if matches!(field, RegexField::Prose | RegexField::Patch) {
            let mut confirmed = Vec::with_capacity(out.len());
            for row in out {
                if let Some(body) = self.fetch_body(&row.message_id)? {
                    let bytes = match field {
                        RegexField::Patch => extract_patch_bytes(&body),
                        RegexField::Prose => extract_prose_bytes(&body),
                        _ => unreachable!(),
                    };
                    if let Some(b) = bytes {
                        if dfa_search(&dfa, &b) {
                            confirmed.push(row);
                        }
                    }
                }
                if confirmed.len() >= limit {
                    break;
                }
            }
            confirmed.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
            return Ok(confirmed);
        }

        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        out.truncate(limit);
        Ok(out)
    }

    /// Generalized message-vs-message diff. `mode` selects the view:
    /// `Patch` (just the diff payload), `Prose` (body minus patch
    /// minus quoted reply / sig), or `Raw` (verbatim RFC822 bytes).
    pub fn diff(&self, a: &str, b: &str, mode: DiffMode) -> Result<DiffResult> {
        let row_a = self
            .fetch_message(a)?
            .ok_or_else(|| crate::error::Error::State(format!("message_id {a:?} not found")))?;
        let row_b = self
            .fetch_message(b)?
            .ok_or_else(|| crate::error::Error::State(format!("message_id {b:?} not found")))?;
        let body_a = self
            .fetch_body(a)?
            .ok_or_else(|| crate::error::Error::State(format!("body for {a:?} missing")))?;
        let body_b = self
            .fetch_body(b)?
            .ok_or_else(|| crate::error::Error::State(format!("body for {b:?} missing")))?;
        let text_a = match mode {
            DiffMode::Raw => decode_lossy(&body_a),
            DiffMode::Patch => decode_lossy(&extract_patch_bytes(&body_a).unwrap_or_default()),
            DiffMode::Prose => decode_lossy(&extract_prose_bytes(&body_a).unwrap_or_default()),
        };
        let text_b = match mode {
            DiffMode::Raw => decode_lossy(&body_b),
            DiffMode::Patch => decode_lossy(&extract_patch_bytes(&body_b).unwrap_or_default()),
            DiffMode::Prose => decode_lossy(&extract_prose_bytes(&body_b).unwrap_or_default()),
        };
        Ok(DiffResult {
            row_a,
            row_b,
            text_a,
            text_b,
        })
    }

    /// Walk the reply graph from any starting message_id and return
    /// every message in the same conversation, ordered by date.
    /// Bounded by `max_messages` so a runaway thread can't OOM the
    /// server.
    pub fn thread(&self, message_id: &str, max_messages: usize) -> Result<Vec<MessageRow>> {
        // Not-in-corpus short-circuit. Without this, a bogus mid falls
        // through to `thread_via_parquet_scan` and burns ~5 s (the
        // request timeout) scanning every Parquet file for a mid that
        // isn't there. `fetch_message` is a single indexed lookup.
        let Some(seed) = self.fetch_message(message_id)? else {
            return Ok(Vec::new());
        };

        // Fast path: one indexed `scan_eq(Tid, ...)` against over.db.
        // `rebuild_tid` backfills the `tid` column for every row, so
        // after a rebuild, "all messages in the thread" is a single
        // B-tree lookup on `over_tid`. Mirrors the series_timeline fix.
        if let Some(seed_tid) = seed.tid.as_deref().filter(|t| !t.is_empty())
            && let Some(res) = self.with_over(|db| {
                db.scan_eq(EqField::Tid, seed_tid, None, None, max_messages)
            })
        {
            let mut rows = res?;
            rows.sort_by_key(|r| r.date_unix_ns.unwrap_or(i64::MIN));
            return Ok(rows);
        }

        // Fallback: Parquet-scan BFS. Used when over.db is absent or
        // `rebuild_tid` hasn't backfilled tids yet (fresh deployment).
        // The seed exists, so the scan is bounded to one real thread,
        // not an open-ended hunt for a non-existent mid.
        self.thread_via_parquet_scan(message_id, max_messages)
    }

    fn thread_via_parquet_scan(
        &self,
        message_id: &str,
        max_messages: usize,
    ) -> Result<Vec<MessageRow>> {
        use std::collections::{HashSet, VecDeque};
        let needle = strip_angles(message_id).to_owned();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::from([needle]);
        let mut collected: Vec<MessageRow> = Vec::new();

        // Defense in depth: Python already enforces a 5 s wall-clock
        // cap via asyncio.wait_for, but that can't cancel the Rust
        // thread — it just abandons the future, leaking a `std::thread`
        // worker until Rust returns naturally. A pathological fresh-
        // deployment thread walk (no over.db tid backfill yet + a
        // seed mid with a long reply chain) could pin a worker for
        // the whole scan. The deadline check between BFS iterations
        // bounds that to ~5 s.
        let deadline = Deadline::new(crate::router::query_wall_clock_ms());

        while let Some(mid) = queue.pop_front() {
            deadline.check()?;
            if visited.contains(&mid) || collected.len() >= max_messages {
                continue;
            }
            visited.insert(mid.clone());
            let mut new_relations: Vec<String> = Vec::new();
            self.scan(
                |r| {
                    r.message_id == mid
                        || r.in_reply_to.as_deref() == Some(mid.as_str())
                        || r.references.iter().any(|p| p == &mid)
                },
                |r| {
                    if r.message_id == mid {
                        if let Some(parent) = r.in_reply_to.as_deref() {
                            if !parent.is_empty() {
                                new_relations.push(parent.to_owned());
                            }
                        }
                        for p in &r.references {
                            if !p.is_empty() {
                                new_relations.push(p.clone());
                            }
                        }
                    } else {
                        new_relations.push(r.message_id.clone());
                    }
                    collected.push(r);
                    collected.len() < max_messages
                },
            )?;
            for relation in new_relations {
                if !visited.contains(&relation) {
                    queue.push_back(relation);
                }
            }
        }

        let mut seen = HashSet::new();
        collected.retain(|r| seen.insert(r.message_id.clone()));
        collected.sort_by_key(|r| r.date_unix_ns.unwrap_or(i64::MIN));
        Ok(collected)
    }

    /// Free-text BM25 search over prose (body minus patch) +
    /// subject_normalized. Returns ranked hits with their scores.
    ///
    /// Phrase queries (`"..."`) are rejected — positions are off by
    /// design. Use `patch_search` for literal substrings in code.
    pub fn prose_search(&self, query: &str, limit: usize) -> Result<Vec<(MessageRow, f32)>> {
        self.prose_search_filtered(query, None, limit)
    }

    /// Like `prose_search` but with an optional tantivy-side list
    /// filter. When `list_filter` is set, tantivy only scores
    /// documents from that list, eliminating false negatives from
    /// post-filter starvation under corpus skew.
    pub fn prose_search_filtered(
        &self,
        query: &str,
        list_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(MessageRow, f32)>> {
        let bm = self.bm25()?;
        let top = bm.search_filtered(query, list_filter, limit)?;
        if top.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: std::collections::HashMap<String, f32> =
            top.iter().map(|(m, s)| (m.clone(), *s)).collect();

        // Hot path: tantivy returned ~limit doc ids in milliseconds;
        // over.db hydrates them in milliseconds via a chunked
        // `WHERE message_id IN (...)` lookup. The legacy path here
        // did a full Parquet scan (~3 minutes on the 29M-row corpus)
        // which is the bug Phase 3 exists to fix.
        //
        // Miss-fallback: any mid that tantivy returned but over.db
        // doesn't have (partial-ingest drift) falls through to a
        // single Parquet scan that filters just for those missing
        // mids. Bounded: |missing| ≤ |top| ≤ limit.
        let ids: Vec<String> = top.iter().map(|(m, _)| m.clone()).collect();
        let mut rows: Vec<MessageRow> = Vec::new();
        let mut missing: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(res) = self.with_over(|db| db.get_many(&ids)) {
            let map = res?;
            for mid in &ids {
                match map.get(mid) {
                    Some(row) => rows.push(row.clone()),
                    None => {
                        missing.insert(mid.clone());
                    }
                }
            }
        } else {
            missing.extend(ids.iter().cloned());
        }
        if !missing.is_empty() {
            self.scan(
                |r| missing.contains(&r.message_id),
                |r| {
                    rows.push(r);
                    true
                },
            )?;
        }
        let mut scored: Vec<(MessageRow, f32)> = rows
            .into_iter()
            .filter_map(|r| wanted.get(&r.message_id).map(|s| (r, *s)))
            .collect();
        scored.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    /// Substring search over patch content via the trigram tier,
    /// confirmed against the decompressed body.
    ///
    /// `needle` is a literal byte string. Matches use byte-exact
    /// comparison (no case folding). Returns at most `limit` rows
    /// newest-first.
    ///
    /// `list` (optional) restricts both the trigram segments scanned
    /// and the metadata lookup to one list.
    pub fn patch_search(
        &self,
        needle: &str,
        list: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let lists = match list {
            Some(l) => vec![l.to_owned()],
            None => list_trigram_lists(&self.data_dir)?,
        };

        let mut candidates: std::collections::HashSet<String> = std::collections::HashSet::new();
        'outer: for lst in &lists {
            for seg_dir in crate::trigram::list_segments(&self.data_dir, lst)? {
                let seg = crate::trigram::SegmentReader::open(&seg_dir)?;
                for mid in seg.candidates_for_substring(needle.as_bytes()) {
                    candidates.insert(mid.to_owned());
                    if candidates.len() >= MAX_PATCH_CANDIDATES {
                        break 'outer;
                    }
                }
            }
        }
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        if candidates.len() >= MAX_PATCH_CANDIDATES {
            return Err(Error::QueryParse(format!(
                "patch_search: needle {needle:?} matches too many candidates \
                 (>{MAX_PATCH_CANDIDATES}); narrow with list: or a longer substring"
            )));
        }

        let list_filter = list.map(str::to_owned);
        let needle_bytes = needle.as_bytes().to_owned();
        let hits = self.hydrate_candidates(&candidates, list_filter.as_deref())?;

        // Confirm: decompress + byte-scan. Dropping ambiguous hits.
        let mut confirmed = Vec::new();
        for row in hits {
            let store = self.store_for(&row.list)?;
            let body = store.read_at(row.body_segment_id, row.body_offset)?;
            if memchr::memmem::find(&body, &needle_bytes).is_some() {
                confirmed.push(row);
            }
        }

        confirmed.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        confirmed.truncate(limit);
        Ok(confirmed)
    }

    /// Hydrate a set of trigram-candidate message_ids into full
    /// `MessageRow`s, preferring over.db's indexed IN-lookup when
    /// available and falling back to the single-pass Parquet scan
    /// otherwise. Applies `list_filter` at the over.db layer via a
    /// post-hydration filter (cheap: we already have the row in memory)
    /// or at the scan predicate for the Parquet path.
    fn hydrate_candidates(
        &self,
        candidates: &std::collections::HashSet<String>,
        list_filter: Option<&str>,
    ) -> Result<Vec<MessageRow>> {
        let want_list = list_filter.map(str::to_owned);
        let mut hits: Vec<MessageRow> = Vec::new();
        let mut missing: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        if let Some(res) = self.with_over(|db| {
            let ids: Vec<String> = candidates.iter().cloned().collect();
            db.get_many(&ids)
        }) {
            let map = res?;
            for mid in candidates {
                match map.get(mid) {
                    Some(row) => {
                        // Apply list_filter at the over.db layer.
                        if want_list.as_deref().is_none_or(|l| row.list == l) {
                            hits.push(row.clone());
                        }
                    }
                    None => {
                        missing.insert(mid.clone());
                    }
                }
            }
        } else {
            missing.extend(candidates.iter().cloned());
        }

        // Miss-fallback: any candidate mid not in over.db falls through
        // to a Parquet scan for just the missing IDs. Bounded to |missing|.
        if !missing.is_empty() {
            let list_owned = want_list.clone();
            self.scan(
                |r| {
                    if let Some(ref lst) = list_owned
                        && &r.list != lst
                    {
                        return false;
                    }
                    missing.contains(&r.message_id)
                },
                |r| {
                    hits.push(r);
                    true
                },
            )?;
        }
        Ok(hits)
    }

    /// Like `patch_search` but with optional edit-distance tolerance.
    /// When `fuzzy_edits == 0`, behaves identically to `patch_search`.
    /// When `fuzzy_edits > 0`, the confirmation step uses
    /// `triple_accel::levenshtein_search` to find approximate matches.
    pub fn patch_search_fuzzy(
        &self,
        needle: &str,
        list: Option<&str>,
        limit: usize,
        fuzzy_edits: u32,
    ) -> Result<Vec<MessageRow>> {
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let lists = match list {
            Some(l) => vec![l.to_owned()],
            None => list_trigram_lists(&self.data_dir)?,
        };

        let mut candidates: std::collections::HashSet<String> = std::collections::HashSet::new();
        'outer: for lst in &lists {
            for seg_dir in crate::trigram::list_segments(&self.data_dir, lst)? {
                let seg = crate::trigram::SegmentReader::open(&seg_dir)?;
                for mid in seg.candidates_for_substring_fuzzy(needle.as_bytes(), fuzzy_edits) {
                    candidates.insert(mid.to_owned());
                    if candidates.len() >= MAX_PATCH_CANDIDATES {
                        break 'outer;
                    }
                }
            }
        }
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        if candidates.len() >= MAX_PATCH_CANDIDATES {
            return Err(Error::QueryParse(format!(
                "patch_search_fuzzy: needle {needle:?} matches too many candidates \
                 (>{MAX_PATCH_CANDIDATES}); narrow with list: or a longer substring"
            )));
        }

        let list_filter = list.map(str::to_owned);
        let needle_bytes = needle.as_bytes().to_owned();
        let hits = self.hydrate_candidates(&candidates, list_filter.as_deref())?;

        let mut confirmed = Vec::new();
        for row in hits {
            let store = self.store_for(&row.list)?;
            let body = store.read_at(row.body_segment_id, row.body_offset)?;
            let is_match = if fuzzy_edits == 0 {
                memchr::memmem::find(&body, &needle_bytes).is_some()
            } else {
                triple_accel::levenshtein::levenshtein_search_simd_with_opts(
                    &needle_bytes,
                    &body,
                    fuzzy_edits,
                    triple_accel::SearchType::Best,
                    triple_accel::levenshtein::LEVENSHTEIN_COSTS,
                    false,
                )
                .next()
                .is_some()
            };
            if is_match {
                confirmed.push(row);
            }
        }

        confirmed.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        confirmed.truncate(limit);
        Ok(confirmed)
    }

    /// Fetch the raw uncompressed body bytes for a given message-id.
    ///
    /// Does a point lookup over the metadata tier to find the
    /// `(list, segment_id, offset)` crosswalk, then a streaming
    /// zstd-decode from the compressed store.
    pub fn fetch_body(&self, message_id: &str) -> Result<Option<Vec<u8>>> {
        let Some(row) = self.fetch_message(message_id)? else {
            return Ok(None);
        };
        // body_offset from the metadata tier is the byte offset into the
        // compressed segment file. Segment id is derived per-list from the
        // active-segment scan during ingest; the first N-GB of data lives in
        // segment 0, which is what we write at v0.5 (no rollover in tests).
        // TODO(phase-2): add `segment_id` column to metadata so we aren't
        // relying on this convention on the reader side.
        let store = self.store_for(&row.list)?;
        let body = store.read_at(row.body_segment_id, row.body_offset)?;
        Ok(Some(body))
    }

    /// Universal lookup: message-id exact, or SHA in `fixes[]`, or CVE in
    /// subject. Returns up to `limit` rows, newest first.
    pub fn expand_citation(&self, token: &str, limit: usize) -> Result<Vec<MessageRow>> {
        let needle = strip_angles(token).to_owned();
        let sha_like = is_sha_prefix(&needle);
        let cve_like = is_cve_id(&needle);

        // Fast path: a token that looks like a Message-ID (not SHA, not
        // CVE) is a point lookup against over.db's indexed message_id
        // column. The legacy scan-all path below was minute-scale on a
        // 17M-row corpus; over.db is microseconds.
        //
        // We only bypass the scan when `sha_like` and `cve_like` are
        // both false — for SHA queries we still need to walk `fixes[]`
        // (a ddd-blob field) and for CVE queries we need a substring
        // match on `subject_raw`. Neither has an over.db fast path yet
        // (filed as F2 in the over.db follow-ups doc).
        if !sha_like && !cve_like && self.over_db().is_some()
            && let Some(res) = self.with_over(|db| db.get(&needle))
            && let Some(row) = res?
        {
            return Ok(vec![row]);
        }

        let mut out: Vec<MessageRow> = Vec::new();
        self.scan(
            |r| {
                if r.message_id == needle {
                    return true;
                }
                if sha_like && r.fixes.iter().any(|f| f.contains(&needle)) {
                    return true;
                }
                if cve_like {
                    if let Some(subj) = &r.subject_raw {
                        if subj.contains(&needle) {
                            return true;
                        }
                    }
                }
                false
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }
}

/// Equality-targetable column. The PyO3 wrapper maps Python strings
/// to these variants; downstream code stays type-safe.
#[derive(Debug, Clone, Copy)]
pub enum EqField {
    MessageId,
    List,
    FromAddr,
    InReplyTo,
    Tid,
    CommitOid,
    BodySha256,
    SubjectNormalized,
    /// list-shaped columns: row matches if value appears in the list.
    TouchedFile,
    TouchedFunction,
    Reference,
    SubjectTag,
    SignedOffBy,
    ReviewedBy,
    AckedBy,
    TestedBy,
    CoDevelopedBy,
    ReportedBy,
    Fixes,
    Link,
    Closes,
    CcStable,
}

impl EqField {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "message_id" => EqField::MessageId,
            "list" => EqField::List,
            "from_addr" => EqField::FromAddr,
            "in_reply_to" => EqField::InReplyTo,
            "tid" => EqField::Tid,
            "commit_oid" => EqField::CommitOid,
            "body_sha256" => EqField::BodySha256,
            "subject_normalized" => EqField::SubjectNormalized,
            "touched_files" | "touched_file" => EqField::TouchedFile,
            "touched_functions" | "touched_function" => EqField::TouchedFunction,
            "references" | "reference" => EqField::Reference,
            "subject_tags" | "subject_tag" | "tag" => EqField::SubjectTag,
            "signed_off_by" => EqField::SignedOffBy,
            "reviewed_by" => EqField::ReviewedBy,
            "acked_by" => EqField::AckedBy,
            "tested_by" => EqField::TestedBy,
            "co_developed_by" => EqField::CoDevelopedBy,
            "reported_by" => EqField::ReportedBy,
            "fixes" => EqField::Fixes,
            "link" => EqField::Link,
            "closes" => EqField::Closes,
            "cc_stable" => EqField::CcStable,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RegexField {
    Subject,
    FromAddr,
    Prose,
    Patch,
}

impl RegexField {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "subject_raw" | "subject" => RegexField::Subject,
            "from_addr" | "from" => RegexField::FromAddr,
            "body_prose" | "prose" => RegexField::Prose,
            "patch" => RegexField::Patch,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum DiffMode {
    Patch,
    Prose,
    Raw,
}

impl DiffMode {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "patch" => DiffMode::Patch,
            "prose" => DiffMode::Prose,
            "raw" => DiffMode::Raw,
            _ => return None,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct CountSummary {
    pub count: u64,
    pub distinct_authors: u64,
    pub earliest_unix_ns: Option<i64>,
    pub latest_unix_ns: Option<i64>,
}

/// Aggregate profile for one `from_addr`. Built from the most-recent
/// `sample_limit` messages authored by that address (via the indexed
/// over.db from_addr path).
///
/// Scope note: this is bounded to messages they AUTHORED. Tools that
/// want "how many patches did this person REVIEW" (i.e. their address
/// appeared in someone else's `reviewed_by` trailer) need a separate
/// trailer-index table — deferred; current over.db has no reverse
/// lookup on trailer arrays.
#[derive(Debug, Default, Clone)]
pub struct AuthorProfile {
    pub addr_queried: String,
    /// How many rows the aggregation walked. Capped by `limit`.
    pub sampled: u64,
    /// Subset of `sampled` where the author was the From: address.
    pub authored_count: u64,
    /// Subset of `sampled` that came from `include_mentions` —
    /// rows where the author appeared in a trailer on someone
    /// else's patch. Always 0 when `include_mentions=false`.
    pub mention_count: u64,
    /// `true` when `sampled == limit`; caller may be looking at a
    /// recent-only slice of a prolific author's history.
    pub limit_hit: bool,
    pub oldest_unix_ns: Option<i64>,
    pub newest_unix_ns: Option<i64>,
    pub patches_with_content: u64,
    pub cover_letters: u64,
    pub unique_subjects: u64,
    pub with_fixes_trailer: u64,
    pub subsystems: Vec<SubsystemBucket>,
    /// Trailers visible on messages THIS author sent (i.e. what
    /// their own patches carry).
    pub own_trailers: OwnTrailerStats,
    /// Trailers OTHERS added to this author's patches in replies
    /// that went into the final series version. Counts are
    /// patch-granularity: "N of my patches got at least one
    /// Reviewed-by" — NOT total Reviewed-by-count across replies.
    pub received_trailers: ReceivedTrailerStats,
}

#[derive(Debug, Default, Clone)]
pub struct SubsystemBucket {
    pub list: String,
    pub patches: u64,
    pub oldest_unix_ns: Option<i64>,
    pub newest_unix_ns: Option<i64>,
}

#[derive(Debug, Default, Clone)]
pub struct OwnTrailerStats {
    /// Count of this author's messages that carry at least one `Signed-off-by:`.
    pub signed_off_by_present: u64,
    /// Messages that cite `Fixes:` (i.e. the author wrote a bug fix).
    pub fixes_issued: u64,
}

#[derive(Debug, Default, Clone)]
pub struct ReceivedTrailerStats {
    pub reviewed_by: u64,
    pub acked_by: u64,
    pub tested_by: u64,
    pub co_developed_by: u64,
    pub reported_by: u64,
    pub cc_stable: u64,
}

/// Which trailer kind an observed address was seen on — used by
/// `maintainer_profile` when aggregating declared-vs-observed.
#[derive(Copy, Clone, Debug)]
enum TrailerKind {
    ReviewedBy,
    AckedBy,
    TestedBy,
    SignedOffBy,
}

#[derive(Debug, Default, Clone)]
pub struct ObservedAddr {
    pub email: String,
    pub reviewed_by: u64,
    pub acked_by: u64,
    pub tested_by: u64,
    pub signed_off_by: u64,
    pub last_seen_unix_ns: Option<i64>,
}

#[derive(Debug, Default, Clone)]
pub struct DeclaredEntry {
    pub name: String,
    pub status: Option<String>,
    pub lists: Vec<String>,
    pub maintainers: Vec<String>,
    pub reviewers: Vec<String>,
    pub depth: u32,
}

#[derive(Debug, Default, Clone)]
pub struct MaintainerProfile {
    pub path_queried: String,
    /// False when no MAINTAINERS snapshot was configured — in that
    /// case `declared` is empty; only observed activity is meaningful.
    pub maintainers_available: bool,
    /// MAINTAINERS entries whose F:/N: matched the path. Sorted by
    /// depth descending (most specific first).
    pub declared: Vec<DeclaredEntry>,
    /// Observed top-N addresses that reviewed / acked / tested /
    /// signed-off patches touching this path in the window.
    pub observed: Vec<ObservedAddr>,
    /// Addresses in M:/R: of matching MAINTAINERS entries that had
    /// ZERO observed activity in the window. "stale_declared".
    pub stale_declared: Vec<String>,
    /// Observed addresses NOT declared in MAINTAINERS — ranked by
    /// review + ack count.
    pub active_unlisted: Vec<ObservedAddr>,
    /// How many messages matched the path-in-window query.
    pub sampled_patches: u64,
}

/// True when `addr_lc` (pre-lowercased email) appears in any of
/// the common trailer arrays of `row`. Used by `trailer_mentions`.
/// We compare via lowercase substring so "Alice <ALICE@X>" matches
/// the canonical "alice@x" form.
fn any_trailer_contains_email(row: &MessageRow, addr_lc: &str) -> bool {
    let lists: [&[String]; 9] = [
        &row.signed_off_by,
        &row.reviewed_by,
        &row.acked_by,
        &row.tested_by,
        &row.co_developed_by,
        &row.reported_by,
        &row.suggested_by,
        &row.helped_by,
        &row.assisted_by,
    ];
    for l in lists {
        for t in l {
            if t.to_ascii_lowercase().contains(addr_lc) {
                return true;
            }
        }
    }
    false
}

/// Extract `user@host` from a "Name <email>" style string. Returns
/// empty when the pattern doesn't match. Case-folded to match the
/// lowercased indexing in over.db / ddd payloads.
fn extract_email(s: &str) -> String {
    let start = match s.rfind('<') {
        Some(i) => i + 1,
        None => {
            // Bare email — take first whitespace-delimited token
            // that contains `@`.
            return s
                .split_whitespace()
                .find(|tok| tok.contains('@'))
                .map(|tok| tok.trim_matches(|c: char| !c.is_ascii_graphic()).to_ascii_lowercase())
                .unwrap_or_default();
        }
    };
    let end = match s[start..].find('>') {
        Some(i) => start + i,
        None => return String::new(),
    };
    let email = &s[start..end];
    if email.contains('@') {
        email.to_ascii_lowercase()
    } else {
        String::new()
    }
}

pub struct DiffResult {
    pub row_a: MessageRow,
    pub row_b: MessageRow,
    pub text_a: String,
    pub text_b: String,
}

/// True when `OverDb::scan_eq` has a dedicated index for the field.
/// Mirrors the match arms inside `OverDb::scan_eq`. Kept here so the
/// Reader can short-circuit through over.db without the dispatch
/// touching the slower sequential scan path inside the OverDb module.
fn eq_field_is_over_indexed(field: EqField) -> bool {
    matches!(
        field,
        EqField::MessageId
            | EqField::FromAddr
            | EqField::List
            | EqField::InReplyTo
            | EqField::Tid
    )
}

fn eq_field_matches(field: EqField, r: &MessageRow, value: &str) -> bool {
    match field {
        EqField::MessageId => r.message_id == value,
        EqField::List => r.list == value,
        EqField::FromAddr => r.from_addr.as_deref() == Some(value),
        EqField::InReplyTo => r.in_reply_to.as_deref() == Some(value),
        EqField::Tid => r.tid.as_deref() == Some(value),
        EqField::CommitOid => r.commit_oid == value,
        EqField::BodySha256 => r.body_sha256 == value,
        EqField::SubjectNormalized => r.subject_normalized.as_deref() == Some(value),
        EqField::TouchedFile => r.touched_files.iter().any(|x| x == value),
        EqField::TouchedFunction => r.touched_functions.iter().any(|x| x == value),
        EqField::Reference => r.references.iter().any(|x| x == value),
        EqField::SubjectTag => r.subject_tags.iter().any(|x| x == value),
        EqField::SignedOffBy => r.signed_off_by.iter().any(|x| x.contains(value)),
        EqField::ReviewedBy => r.reviewed_by.iter().any(|x| x.contains(value)),
        EqField::AckedBy => r.acked_by.iter().any(|x| x.contains(value)),
        EqField::TestedBy => r.tested_by.iter().any(|x| x.contains(value)),
        EqField::CoDevelopedBy => r.co_developed_by.iter().any(|x| x.contains(value)),
        EqField::ReportedBy => r.reported_by.iter().any(|x| x.contains(value)),
        EqField::Fixes => r.fixes.iter().any(|x| x.contains(value)),
        EqField::Link => r.link.iter().any(|x| x.contains(value)),
        EqField::Closes => r.closes.iter().any(|x| x.contains(value)),
        EqField::CcStable => r.cc_stable.iter().any(|x| x.contains(value)),
    }
}

fn trailer_matches(r: &MessageRow, name_lc: &str, needle_lc: &str) -> bool {
    let bag: &[String] = match name_lc {
        "fixes" => &r.fixes,
        "link" => &r.link,
        "closes" => &r.closes,
        "cc-stable" | "cc_stable" => &r.cc_stable,
        "signed-off-by" | "signed_off_by" => &r.signed_off_by,
        "reviewed-by" | "reviewed_by" => &r.reviewed_by,
        "acked-by" | "acked_by" => &r.acked_by,
        "tested-by" | "tested_by" => &r.tested_by,
        "co-developed-by" | "co_developed_by" => &r.co_developed_by,
        "reported-by" | "reported_by" => &r.reported_by,
        _ => return false,
    };
    bag.iter()
        .any(|v| v.to_ascii_lowercase().contains(needle_lc))
}

fn dfa_search(dfa: &regex_automata::dfa::dense::DFA<Vec<u32>>, haystack: &[u8]) -> bool {
    use regex_automata::Input;
    use regex_automata::dfa::Automaton;
    dfa.try_search_fwd(&Input::new(haystack))
        .ok()
        .flatten()
        .is_some()
}

fn extract_patch_bytes(body: &[u8]) -> Option<Vec<u8>> {
    // Find first "\ndiff --git " or leading "diff --git ".
    let needle = b"\ndiff --git ";
    if body.starts_with(b"diff --git ") {
        return Some(body.to_vec());
    }
    let pos = memchr::memmem::find(body, needle)?;
    Some(body[pos + 1..].to_vec())
}

fn extract_prose_bytes(body: &[u8]) -> Option<Vec<u8>> {
    let needle = b"\ndiff --git ";
    let end = if body.starts_with(b"diff --git ") {
        0
    } else {
        memchr::memmem::find(body, needle).unwrap_or(body.len())
    };
    if end == 0 {
        return Some(Vec::new());
    }
    Some(body[..end].to_vec())
}

fn decode_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn list_trigram_lists(data_dir: &Path) -> Result<Vec<String>> {
    let root = data_dir.join("trigram");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                out.push(name.to_owned());
            }
        }
    }
    out.sort();
    Ok(out)
}

// ---- internals ----

fn strip_angles(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(s)
}

fn is_sha_prefix(s: &str) -> bool {
    s.len() >= 7 && s.len() <= 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_cve_id(s: &str) -> bool {
    // CVE-YYYY-NNNN(+)
    let Some(tail) = s.strip_prefix("CVE-") else {
        return false;
    };
    let mut parts = tail.split('-');
    let (Some(y), Some(n), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    y.len() == 4
        && y.chars().all(|c| c.is_ascii_digit())
        && n.len() >= 4
        && n.chars().all(|c| c.is_ascii_digit())
}

/// Convert an Arrow RecordBatch into owned `MessageRow`s.
///
/// This is the one place we map column names → indices. If you add a
/// column to the schema, add it here.
#[allow(clippy::needless_borrow)]
fn materialize_batch(batch: &RecordBatch) -> Result<Vec<MessageRow>> {
    let schema = batch.schema();
    let get = |name: &str| -> Result<&dyn Array> {
        let idx = schema
            .index_of(name)
            .map_err(|e| Error::State(format!("missing column {name}: {e}")))?;
        Ok(batch.column(idx).as_ref())
    };

    let message_id = get(sc::COL_MESSAGE_ID)?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| Error::State("message_id not utf8".to_owned()))?;
    let list = downcast_string(batch, sc::COL_LIST)?;
    let shard = downcast_string(batch, sc::COL_SHARD)?;
    let commit_oid = downcast_string(batch, sc::COL_COMMIT_OID)?;
    let from_addr = downcast_string(batch, sc::COL_FROM_ADDR)?;
    let from_name = downcast_string(batch, sc::COL_FROM_NAME)?;
    let subject_raw = downcast_string(batch, sc::COL_SUBJECT_RAW)?;
    let subject_normalized = downcast_string(batch, sc::COL_SUBJECT_NORMALIZED)?;
    let subject_tags = downcast_list(batch, sc::COL_SUBJECT_TAGS)?;
    let date = batch
        .column(schema.index_of(sc::COL_DATE).unwrap())
        .as_any()
        .downcast_ref::<TimestampNanosecondArray>()
        .cloned();
    let in_reply_to = downcast_string(batch, sc::COL_IN_REPLY_TO)?;
    let references = downcast_list(batch, sc::COL_REFERENCES)?;
    let tid = downcast_string_opt(batch, sc::COL_TID);
    let series_version = downcast_u32(batch, sc::COL_SERIES_VERSION)?;
    let series_index = downcast_u32(batch, sc::COL_SERIES_INDEX)?;
    let series_total = downcast_u32(batch, sc::COL_SERIES_TOTAL)?;
    let is_cover_letter = downcast_bool(batch, sc::COL_IS_COVER_LETTER)?;
    let has_patch = downcast_bool(batch, sc::COL_HAS_PATCH)?;
    let touched_files = downcast_list(batch, sc::COL_TOUCHED_FILES)?;
    let touched_functions = downcast_list(batch, sc::COL_TOUCHED_FUNCTIONS)?;
    let files_changed = downcast_u32(batch, sc::COL_FILES_CHANGED)?;
    let insertions = downcast_u32(batch, sc::COL_INSERTIONS)?;
    let deletions = downcast_u32(batch, sc::COL_DELETIONS)?;
    let signed_off_by = downcast_list(batch, sc::COL_SIGNED_OFF_BY)?;
    let reviewed_by = downcast_list(batch, sc::COL_REVIEWED_BY)?;
    let acked_by = downcast_list(batch, sc::COL_ACKED_BY)?;
    let tested_by = downcast_list(batch, sc::COL_TESTED_BY)?;
    let co_developed_by = downcast_list(batch, sc::COL_CO_DEVELOPED_BY)?;
    let reported_by = downcast_list(batch, sc::COL_REPORTED_BY)?;
    let fixes = downcast_list(batch, sc::COL_FIXES)?;
    let link_trailers = downcast_list(batch, sc::COL_LINK)?;
    let closes = downcast_list(batch, sc::COL_CLOSES)?;
    let cc_stable = downcast_list(batch, sc::COL_CC_STABLE)?;
    let suggested_by = downcast_list_opt(batch, sc::COL_SUGGESTED_BY);
    let helped_by = downcast_list_opt(batch, sc::COL_HELPED_BY);
    let assisted_by = downcast_list_opt(batch, sc::COL_ASSISTED_BY);
    let trailers_json = downcast_string_opt(batch, sc::COL_TRAILERS_JSON);
    // body_segment_id was added after v0.1.0; older Parquet files
    // lack it. Default to segment 0 for backward compat.
    let body_segment_id = downcast_u32_opt(batch, sc::COL_BODY_SEGMENT_ID);
    let body_offset = downcast_u64(batch, sc::COL_BODY_OFFSET)?;
    let body_length = downcast_u64(batch, sc::COL_BODY_LENGTH)?;
    let body_sha256 = downcast_string(batch, sc::COL_BODY_SHA256)?;
    let schema_version = downcast_u32(batch, sc::COL_SCHEMA_VERSION)?;

    let mut rows = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        rows.push(MessageRow {
            message_id: message_id.value(i).to_owned(),
            list: list.value(i).to_owned(),
            shard: shard.value(i).to_owned(),
            commit_oid: commit_oid.value(i).to_owned(),
            from_addr: opt_string(&from_addr, i),
            from_name: opt_string(&from_name, i),
            subject_raw: opt_string(&subject_raw, i),
            subject_normalized: opt_string(&subject_normalized, i),
            subject_tags: list_strings(&subject_tags, i),
            date_unix_ns: date.as_ref().filter(|a| !a.is_null(i)).map(|a| a.value(i)),
            in_reply_to: opt_string(&in_reply_to, i),
            references: list_strings(&references, i),
            tid: tid.as_ref().and_then(|a| {
                if a.is_null(i) {
                    None
                } else {
                    Some(a.value(i).to_owned())
                }
            }),
            series_version: series_version.value(i),
            series_index: if series_index.is_null(i) {
                None
            } else {
                Some(series_index.value(i))
            },
            series_total: if series_total.is_null(i) {
                None
            } else {
                Some(series_total.value(i))
            },
            is_cover_letter: is_cover_letter.value(i),
            has_patch: has_patch.value(i),
            touched_files: list_strings(&touched_files, i),
            touched_functions: list_strings(&touched_functions, i),
            files_changed: if files_changed.is_null(i) {
                None
            } else {
                Some(files_changed.value(i))
            },
            insertions: if insertions.is_null(i) {
                None
            } else {
                Some(insertions.value(i))
            },
            deletions: if deletions.is_null(i) {
                None
            } else {
                Some(deletions.value(i))
            },
            signed_off_by: list_strings(&signed_off_by, i),
            reviewed_by: list_strings(&reviewed_by, i),
            acked_by: list_strings(&acked_by, i),
            tested_by: list_strings(&tested_by, i),
            co_developed_by: list_strings(&co_developed_by, i),
            reported_by: list_strings(&reported_by, i),
            fixes: list_strings(&fixes, i),
            link: list_strings(&link_trailers, i),
            closes: list_strings(&closes, i),
            cc_stable: list_strings(&cc_stable, i),
            suggested_by: suggested_by
                .as_ref()
                .map(|a| list_strings(a, i))
                .unwrap_or_default(),
            helped_by: helped_by
                .as_ref()
                .map(|a| list_strings(a, i))
                .unwrap_or_default(),
            assisted_by: assisted_by
                .as_ref()
                .map(|a| list_strings(a, i))
                .unwrap_or_default(),
            trailers_json: trailers_json.as_ref().and_then(|a| {
                if a.is_null(i) {
                    None
                } else {
                    Some(a.value(i).to_owned())
                }
            }),
            body_segment_id: body_segment_id.as_ref().map(|a| a.value(i)).unwrap_or(0),
            body_offset: body_offset.value(i),
            body_length: body_length.value(i),
            body_sha256: body_sha256.value(i).to_owned(),
            schema_version: schema_version.value(i),
        });
    }
    Ok(rows)
}

fn downcast_string<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|e| Error::State(format!("missing column {name}: {e}")))?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| Error::State(format!("column {name} not utf8")))
}

/// Returns `None` when the column doesn't exist (backward compat for
/// columns added after v0.1.0).
fn downcast_string_opt(batch: &RecordBatch, name: &str) -> Option<StringArray> {
    let idx = batch.schema().index_of(name).ok()?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .cloned()
}

/// Returns `None` when the column doesn't exist (backward compat).
fn downcast_list_opt(batch: &RecordBatch, name: &str) -> Option<ListArray> {
    let idx = batch.schema().index_of(name).ok()?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<ListArray>()
        .cloned()
}

fn downcast_bool(batch: &RecordBatch, name: &str) -> Result<BooleanArray> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|e| Error::State(format!("missing column {name}: {e}")))?;
    Ok(batch
        .column(idx)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| Error::State(format!("column {name} not bool")))?
        .clone())
}

fn downcast_u32(batch: &RecordBatch, name: &str) -> Result<UInt32Array> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|e| Error::State(format!("missing column {name}: {e}")))?;
    Ok(batch
        .column(idx)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .ok_or_else(|| Error::State(format!("column {name} not u32")))?
        .clone())
}

/// Like `downcast_u32` but returns `None` when the column doesn't
/// exist (backward compat for columns added after v0.1.0).
fn downcast_u32_opt(batch: &RecordBatch, name: &str) -> Option<UInt32Array> {
    let idx = batch.schema().index_of(name).ok()?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .cloned()
}

fn downcast_u64(batch: &RecordBatch, name: &str) -> Result<UInt64Array> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|e| Error::State(format!("missing column {name}: {e}")))?;
    Ok(batch
        .column(idx)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or_else(|| Error::State(format!("column {name} not u64")))?
        .clone())
}

fn downcast_list(batch: &RecordBatch, name: &str) -> Result<ListArray> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|e| Error::State(format!("missing column {name}: {e}")))?;
    Ok(batch
        .column(idx)
        .as_any()
        .downcast_ref::<ListArray>()
        .ok_or_else(|| Error::State(format!("column {name} not list")))?
        .clone())
}

fn opt_string(arr: &StringArray, i: usize) -> Option<String> {
    if arr.is_null(i) {
        None
    } else {
        Some(arr.value(i).to_owned())
    }
}

fn list_strings(list: &ListArray, i: usize) -> Vec<String> {
    if list.is_null(i) {
        return Vec::new();
    }
    let values = list.value(i);
    let Some(s) = values.as_any().downcast_ref::<StringArray>() else {
        return Vec::new();
    };
    (0..s.len())
        .filter(|j| !s.is_null(*j))
        .map(|j| s.value(j).to_owned())
        .collect()
}

// Pedantic unused import silencer; kept for future needs.
#[allow(dead_code)]
fn _int64_unused(_: Int64Array) {}

#[allow(dead_code)]
fn _map_unused(_: HashMap<String, String>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::ingest_shard;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    fn make_synthetic_shard(shard_dir: &Path, messages: &[&[u8]]) {
        let run = |args: &[&str], cwd: &Path| {
            let out = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "tester")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "tester")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let work = tempdir().unwrap();
        run(&["init", "-q", "-b", "master", "."], work.path());
        for (i, msg) in messages.iter().enumerate() {
            fs::write(work.path().join("m"), msg).unwrap();
            run(&["add", "m"], work.path());
            run(&["commit", "-q", "-m", &format!("m{i}")], work.path());
        }
        if shard_dir.exists() {
            fs::remove_dir_all(shard_dir).unwrap();
        }
        run(
            &[
                "clone",
                "--bare",
                "-q",
                work.path().to_str().unwrap(),
                shard_dir.to_str().unwrap(),
            ],
            Path::new("/"),
        );
    }

    fn sample_corpus() -> Vec<Vec<u8>> {
        vec![
            b"From: Alice <alice@example.com>\r\n\
Subject: [PATCH v3 1/2] ksmbd: tighten ACL bounds\r\n\
Date: Mon, 14 Apr 2026 12:00:00 +0000\r\n\
Message-ID: <m1@x>\r\n\
\r\n\
Prose here.\r\n\
Fixes: deadbeef01234567 (\"ksmbd: initial ACL handling\")\r\n\
Reviewed-by: Carol <carol@example.com>\r\n\
Signed-off-by: Alice <alice@example.com>\r\n\
---\r\n\
diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c\r\n\
--- a/fs/smb/server/smbacl.c\r\n\
+++ b/fs/smb/server/smbacl.c\r\n\
@@ -1,1 +1,2 @@ int smb_check_perm_dacl(struct ksmbd_conn *c)\r\n\
 a\r\n\
+b\r\n"
                .to_vec(),
            b"From: Alice <alice@example.com>\r\n\
Subject: [PATCH v3 2/2] ksmbd: follow-up\r\n\
Date: Mon, 14 Apr 2026 12:05:00 +0000\r\n\
Message-ID: <m2@x>\r\n\
In-Reply-To: <m1@x>\r\n\
\r\n\
More prose.\r\n\
Signed-off-by: Alice <alice@example.com>\r\n\
---\r\n\
diff --git a/fs/smb/server/smb2pdu.c b/fs/smb/server/smb2pdu.c\r\n\
--- a/fs/smb/server/smb2pdu.c\r\n\
+++ b/fs/smb/server/smb2pdu.c\r\n\
@@ -1,1 +1,2 @@ int smb2_create(struct ksmbd_conn *c)\r\n\
 a\r\n\
+b\r\n"
                .to_vec(),
        ]
    }

    fn ingest_sample(data: &Path) {
        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let msgs = sample_corpus();
        let msg_refs: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
        make_synthetic_shard(&shard_dir, &msg_refs);
        ingest_shard(data, &shard_dir, "linux-cifs", "0", "run-0001").unwrap();
    }

    #[test]
    fn fetch_message_roundtrip() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let m = r.fetch_message("m1@x").unwrap().unwrap();
        assert_eq!(m.message_id, "m1@x");
        assert_eq!(m.list, "linux-cifs");
        assert!(m.has_patch);
        assert_eq!(m.series_version, 3);
        assert_eq!(m.series_index, Some(1));
        assert!(
            m.reviewed_by
                .iter()
                .any(|s| s.contains("carol@example.com"))
        );
        assert!(
            m.touched_files
                .iter()
                .any(|s| s == "fs/smb/server/smbacl.c")
        );
        assert!(
            m.touched_functions
                .iter()
                .any(|s| s == "smb_check_perm_dacl")
        );
    }

    #[test]
    fn activity_by_file() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .activity(Some("fs/smb/server/smbacl.c"), None, None, None, 50)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m1@x");

        let none = r
            .activity(Some("no/such/file.c"), None, None, None, 50)
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn activity_by_function() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .activity(None, Some("smb2_create"), None, None, 50)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m2@x");
    }

    #[test]
    fn series_timeline_groups_versions() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        // Both messages are from the same series "ksmbd: tighten ACL bounds"
        // only if subject_normalized matches. They don't here (follow-up has
        // a different subject); so series_timeline("m1@x") should return
        // exactly m1@x.
        let rows = r.series_timeline("m1@x").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m1@x");
    }

    /// thread() fast path: when over.db has `tid` backfilled (as
    /// rebuild_tid does in production), the BFS falls away and we
    /// resolve the thread in a single indexed scan_eq. Without this
    /// wiring, thread() did a full Parquet scan per node — minutes
    /// per call on a 17.6M-row corpus.
    #[test]
    fn thread_uses_over_tid_when_present() {
        use crate::over::{DddPayload, OverDb, OverRow};
        let dir = tempdir().unwrap();
        let over_path = dir.path().join("over.db");
        let mut db = OverDb::open(&over_path).unwrap();

        // Three messages in the same thread (shared tid = "root@x").
        // Plus one unrelated message on a different tid that must
        // NOT appear in the result.
        let mk = |mid: &str,
                  tid: &str,
                  date: i64,
                  in_reply_to: Option<&str>|
         -> OverRow {
            OverRow {
                message_id: mid.to_owned(),
                list: "linux-cifs".to_owned(),
                from_addr: Some("a@b".to_owned()),
                date_unix_ns: Some(date),
                in_reply_to: in_reply_to.map(str::to_owned),
                tid: Some(tid.to_owned()),
                body_segment_id: 0,
                body_offset: 0,
                body_length: 1,
                body_sha256: "sha".to_owned(),
                has_patch: false,
                is_cover_letter: false,
                series_version: None,
                series_index: None,
                series_total: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                commit_oid: None,
                ddd: DddPayload {
                    subject_raw: Some("thread test".to_owned()),
                    subject_normalized: Some("thread test".to_owned()),
                    ..Default::default()
                },
            }
        };
        db.insert_batch(&[
            mk("root@x", "root@x", 1_000, None),
            mk("reply1@x", "root@x", 2_000, Some("root@x")),
            mk("reply2@x", "root@x", 3_000, Some("reply1@x")),
            mk("unrelated@x", "other@x", 4_000, None),
        ])
        .unwrap();
        drop(db);

        // No Parquet metadata exists — the fast path is the ONLY way
        // to produce any rows. If thread() silently falls back to the
        // BFS, the test returns empty and fails.
        let reader = Reader::new(dir.path());
        let rows = reader.thread("reply2@x", 10).unwrap();
        let mids: std::collections::HashSet<&str> =
            rows.iter().map(|r| r.message_id.as_str()).collect();
        assert_eq!(rows.len(), 3, "expected all 3 thread members, got {mids:?}");
        assert!(mids.contains("root@x"));
        assert!(mids.contains("reply1@x"));
        assert!(mids.contains("reply2@x"));
        assert!(!mids.contains("unrelated@x"));

        // Ordered by date ascending.
        assert_eq!(rows[0].message_id, "root@x");
        assert_eq!(rows[1].message_id, "reply1@x");
        assert_eq!(rows[2].message_id, "reply2@x");

        // max_messages bound is respected.
        let capped = reader.thread("reply2@x", 2).unwrap();
        assert_eq!(capped.len(), 2);
    }

    /// thread() on a mid that isn't in the corpus must short-circuit
    /// via the indexed fetch_message lookup — never fall through to
    /// the Parquet-scan BFS (which would burn ~5 s looking for a
    /// nonexistent mid, triggering the request-timeout cap).
    /// An already-expired deadline installed before a scan causes
    /// the very first batch boundary to return `QueryTimeout` —
    /// which turns into a clean Err instead of the worker wedging
    /// while asyncio abandons the future on the Python side.
    #[test]
    fn scan_honors_expired_request_deadline() {
        use crate::timeout::{Deadline, DeadlineGuard};
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());

        // Install a deadline that's already 10 seconds overdue. The
        // first batch check inside scan() must short-circuit with
        // Error::QueryTimeout. Without the deadline wiring, substr_subject
        // completes; with it, it Errs.
        let _guard = DeadlineGuard::install({
            // Manually synthesize an expired Deadline via a 0-ms
            // budget — by the time scan() runs even the first check
            // Instant::now() - start exceeds 0.
            Deadline::new(0)
        });
        let err = r.substr_subject("ksmbd", None, None, 100).unwrap_err();
        match err {
            crate::error::Error::QueryTimeout { .. } => {}
            other => panic!("expected QueryTimeout, got {other:?}"),
        }
    }

    #[test]
    fn thread_on_missing_mid_returns_empty_without_parquet_scan() {
        use crate::over::OverDb;
        let dir = tempdir().unwrap();
        // Bring up an over.db so the fast path is available.
        let _ = OverDb::open(&dir.path().join("over.db")).unwrap();
        // No metadata/ directory exists — if the code falls back to
        // thread_via_parquet_scan, `parquet_files()` returns empty and
        // we get the same answer but via a slow detour. The real
        // regression guard is the latency: if a future refactor
        // re-enables the fallback on missing-mid, a full-corpus
        // production instance would time out, not a synthetic one.
        let reader = Reader::new(dir.path());
        let start = std::time::Instant::now();
        let rows = reader
            .thread("<definitely-not-real@nowhere.invalid>", 50)
            .unwrap();
        let elapsed = start.elapsed();
        assert!(rows.is_empty());
        // On an empty corpus this is trivially fast, but the assertion
        // is about shape, not speed: the path must be the indexed
        // fetch_message -> None branch, confirmed by returning empty
        // immediately. (Kept a loose latency check so grossly wrong
        // refactors still fail loudly in CI.)
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "thread() on missing mid took {elapsed:?} — expected short-circuit"
        );
    }

    #[test]
    fn expand_citation_sha_hit() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r.expand_citation("deadbeef01234567", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m1@x");
    }

    #[test]
    fn expand_citation_mid_hit() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r.expand_citation("<m2@x>", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m2@x");
    }

    #[test]
    fn eq_by_from_addr_returns_only_matching_rows() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .eq(EqField::FromAddr, "alice@example.com", None, None, 50)
            .unwrap();
        let mids: std::collections::HashSet<&str> =
            rows.iter().map(|r| r.message_id.as_str()).collect();
        assert!(mids.contains("m1@x"));
        assert!(mids.contains("m2@x"));
    }

    #[test]
    fn eq_on_touched_files_set_membership() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .eq(
                EqField::TouchedFile,
                "fs/smb/server/smb2pdu.c",
                None,
                None,
                50,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m2@x");
    }

    #[test]
    fn in_list_unions_multiple_values() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .in_list(
                EqField::TouchedFile,
                &[
                    "fs/smb/server/smbacl.c".to_owned(),
                    "fs/smb/server/smb2pdu.c".to_owned(),
                ],
                None,
                None,
                50,
            )
            .unwrap();
        let mids: std::collections::HashSet<&str> =
            rows.iter().map(|r| r.message_id.as_str()).collect();
        assert_eq!(mids, ["m1@x", "m2@x"].into_iter().collect());
    }

    #[test]
    fn extract_email_variants() {
        assert_eq!(
            extract_email("Greg KH <gregkh@linuxfoundation.org>"),
            "gregkh@linuxfoundation.org"
        );
        assert_eq!(
            extract_email("gregkh@linuxfoundation.org"),
            "gregkh@linuxfoundation.org"
        );
        assert_eq!(extract_email("just a name"), "");
        assert_eq!(extract_email("<no@lt@at>"), "no@lt@at");
    }

    #[test]
    fn maintainer_profile_cross_references_declared_vs_observed() {
        use std::io::Write;
        let d = tempdir().unwrap();
        ingest_sample(d.path());

        // Drop a tiny MAINTAINERS at the conventional location.
        let mpath = d.path().join("MAINTAINERS");
        let mut f = std::fs::File::create(&mpath).unwrap();
        writeln!(
            f,
            "KSMBD\nM:\tAlice <alice@example.com>\nR:\tDavid Dormant <david@example.com>\nL:\tlinux-cifs@vger.kernel.org\nS:\tMaintained\nF:\tfs/smb/server/"
        )
        .unwrap();
        drop(f);

        let reader = Reader::new(d.path());
        let profile = reader
            .maintainer_profile("fs/smb/server/smbacl.c", 365, 500)
            .unwrap();
        assert!(profile.maintainers_available);
        assert_eq!(profile.declared.len(), 1);
        assert_eq!(profile.declared[0].name, "KSMBD");
        assert!(profile.sampled_patches >= 1);
        // Alice is both the declared maintainer AND the sample
        // corpus author → NOT stale (she's "observed" via SOBs).
        assert!(
            !profile.stale_declared.iter().any(|a| a == "alice@example.com"),
            "alice is active; shouldn't be flagged stale. Got: {:?}",
            profile.stale_declared
        );
        // David Dormant is declared but never appears in the fixture →
        // must be flagged stale.
        assert!(
            profile.stale_declared.iter().any(|a| a == "david@example.com"),
            "david is declared-but-never-seen; should be stale. Got: {:?}",
            profile.stale_declared
        );
    }

    #[test]
    fn maintainer_profile_without_maintainers_file_is_empty_but_ok() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let reader = Reader::new(d.path());
        // No MAINTAINERS dropped into data_dir.
        let profile = reader
            .maintainer_profile("fs/smb/server/smbacl.c", 365, 500)
            .unwrap();
        assert!(!profile.maintainers_available);
        assert!(profile.declared.is_empty());
        // Observed activity still populated from over.db / parquet scan.
        assert!(profile.sampled_patches >= 1);
    }

    #[test]
    fn author_profile_aggregates_from_fixture() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let prof = r
            .author_profile("alice@example.com", None, None, 1000)
            .unwrap();
        // Two messages from Alice in the sample corpus.
        assert_eq!(prof.addr_queried, "alice@example.com");
        assert_eq!(prof.sampled, 2);
        assert!(!prof.limit_hit);
        assert_eq!(prof.subsystems.len(), 1);
        assert_eq!(prof.subsystems[0].list, "linux-cifs");
        assert_eq!(prof.subsystems[0].patches, 2);
        // Both patches have content (has_patch=true).
        assert_eq!(prof.patches_with_content, 2);
        // At least one patch in the fixture carries a Fixes: trailer.
        assert!(prof.with_fixes_trailer >= 1);
        assert!(prof.oldest_unix_ns.is_some());
        assert!(prof.newest_unix_ns.is_some());
    }

    #[test]
    fn author_profile_limit_hit_flag() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        // Ask for just 1 row out of 2 → expect limit_hit=true.
        let prof = r.author_profile("alice@example.com", None, None, 1).unwrap();
        assert_eq!(prof.sampled, 1);
        assert!(prof.limit_hit);
    }

    #[test]
    fn author_profile_include_mentions_finds_trailer_rows() {
        // The sample corpus has alice@example.com authoring 2 patches
        // with carol@example.com in Reviewed-by on at least one.
        // Default scope: carol has 0 authored, 0 mentions.
        // Extended scope: carol picks up the Reviewed-by rows.
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());

        let default_scope = r
            .author_profile("carol@example.com", None, None, 1000)
            .unwrap();
        assert_eq!(default_scope.sampled, 0);
        assert_eq!(default_scope.authored_count, 0);
        assert_eq!(default_scope.mention_count, 0);

        let extended = r
            .author_profile_extended(
                "carol@example.com",
                None,
                None,
                1000,
                true,
                500,
            )
            .unwrap();
        assert!(
            extended.mention_count >= 1,
            "extended scope must pick up reviewed_by mentions; got {}",
            extended.mention_count
        );
        assert_eq!(
            extended.authored_count, 0,
            "carol still shouldn't be counted as author"
        );
        assert_eq!(
            extended.sampled,
            extended.authored_count + extended.mention_count
        );
    }

    #[test]
    fn author_profile_returns_empty_for_unknown_addr() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let prof = r
            .author_profile("nobody@nowhere.invalid", None, None, 100)
            .unwrap();
        assert_eq!(prof.sampled, 0);
        assert_eq!(prof.subsystems.len(), 0);
        assert!(prof.oldest_unix_ns.is_none());
    }

    #[test]
    fn count_returns_summary_stats() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let s = r
            .count(EqField::FromAddr, "alice@example.com", None, None)
            .unwrap();
        assert_eq!(s.count, 2);
        assert_eq!(s.distinct_authors, 1);
        assert!(s.earliest_unix_ns.is_some());
        assert!(s.latest_unix_ns.is_some());
    }

    #[test]
    fn substr_subject_case_insensitive() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r.substr_subject("ksmbd", None, None, 50).unwrap();
        assert!(rows.iter().any(|r| r.message_id == "m1@x"));
        assert!(rows.iter().any(|r| r.message_id == "m2@x"));
        // Uppercase needle still hits.
        let rows = r.substr_subject("KSMBD", None, None, 50).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn substr_trailers_finds_via_fixes_substring() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .substr_trailers("fixes", "deadbeef", None, None, 50)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m1@x");

        // Unknown trailer name returns empty without erroring.
        assert!(
            r.substr_trailers("nonsense", "x", None, None, 5)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn regex_subject_anchored() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .regex(
                RegexField::Subject,
                r"\[PATCH v3 1/2\]",
                false,
                None,
                None,
                10,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m1@x");
    }

    #[test]
    fn regex_rejects_unanchored_dotstar_when_required() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let err = r
            .regex(RegexField::Subject, ".*ksmbd.*", true, None, None, 10)
            .unwrap_err();
        match err {
            crate::error::Error::RegexComplexity(_) => {}
            other => panic!("wrong err: {other:?}"),
        }
    }

    #[test]
    fn regex_patch_field_confirms_via_body() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .regex(
                RegexField::Patch,
                r"smb_check_perm_dacl\(",
                false,
                None,
                None,
                10,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m1@x");
    }

    #[test]
    fn diff_patch_mode_returns_both_views() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let res = r.diff("m1@x", "m2@x", DiffMode::Patch).unwrap();
        assert_eq!(res.row_a.message_id, "m1@x");
        assert_eq!(res.row_b.message_id, "m2@x");
        assert!(res.text_a.starts_with("diff --git "));
        assert!(res.text_b.starts_with("diff --git "));
    }

    #[test]
    fn patch_search_finds_function_name() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        // m1 contains `smb_check_perm_dacl` in its hunk header; m2
        // contains `smb2_create`. Both are in patch bodies, so both
        // should be findable.
        let m1 = r.patch_search("smb_check_perm_dacl", None, 10).unwrap();
        assert_eq!(m1.len(), 1);
        assert_eq!(m1[0].message_id, "m1@x");

        let m2 = r.patch_search("smb2_create", None, 10).unwrap();
        assert_eq!(m2.len(), 1);
        assert_eq!(m2[0].message_id, "m2@x");

        let none = r.patch_search("never_appears_anywhere", None, 10).unwrap();
        assert!(none.is_empty());
    }

    /// End-to-end test that the over.db tier wires through `Reader`.
    ///
    /// We hand-build an `over.db` with two synthetic rows in the
    /// tempdir BEFORE constructing the Reader. There is no Parquet
    /// metadata at all — so anything the Reader returns must have
    /// come from the over.db indexed path. That makes this both a
    /// unit test for the wiring and a regression guard against
    /// silent fallback to the legacy Parquet scan.
    #[test]
    fn reader_uses_over_db_when_present() {
        use crate::over::{DddPayload, OverDb, OverRow};

        let dir = tempdir().unwrap();
        let over_path = dir.path().join("over.db");

        let mut db = OverDb::open(&over_path).unwrap();
        let row_a = OverRow {
            message_id: "over-test-a@x".to_owned(),
            list: "linux-cifs".to_owned(),
            from_addr: Some("Reviewer@Example.COM".to_owned()),
            date_unix_ns: Some(1_700_000_000_000_000_000),
            in_reply_to: None,
            tid: None,
            body_segment_id: 0,
            body_offset: 0,
            body_length: 42,
            body_sha256: "abc".to_owned(),
            has_patch: true,
            is_cover_letter: false,
            series_version: Some(1),
            series_index: Some(1),
            series_total: Some(2),
            files_changed: Some(3),
            insertions: Some(10),
            deletions: Some(2),
            commit_oid: Some("oidA".to_owned()),
            ddd: DddPayload {
                subject_raw: Some("[PATCH 1/2] over-db wiring".to_owned()),
                subject_normalized: Some("over-db wiring".to_owned()),
                subject_tags: vec!["PATCH".to_owned()],
                from_name: Some("Reviewer".to_owned()),
                from_addr_original_case: Some("Reviewer@Example.COM".to_owned()),
                shard: Some("0".to_owned()),
                ..Default::default()
            },
        };
        let row_b = OverRow {
            message_id: "over-test-b@x".to_owned(),
            list: "linux-cifs".to_owned(),
            from_addr: Some("other@example.com".to_owned()),
            date_unix_ns: Some(1_700_000_000_500_000_000),
            in_reply_to: Some("over-test-a@x".to_owned()),
            tid: None,
            body_segment_id: 0,
            body_offset: 100,
            body_length: 13,
            body_sha256: "def".to_owned(),
            has_patch: false,
            is_cover_letter: false,
            series_version: None,
            series_index: None,
            series_total: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            commit_oid: None,
            ddd: DddPayload {
                subject_raw: Some("Re: [PATCH 1/2] over-db wiring".to_owned()),
                subject_normalized: Some("over-db wiring".to_owned()),
                from_addr_original_case: Some("other@example.com".to_owned()),
                shard: Some("0".to_owned()),
                ..Default::default()
            },
        };
        db.insert_batch(&[row_a, row_b]).unwrap();
        drop(db);

        // No metadata/ dir exists. If the Reader falls back to Parquet
        // (the bug Phase 3 exists to fix), every call below returns
        // empty. The over.db path must produce real rows.
        let reader = Reader::new(dir.path());

        let got = reader
            .fetch_message("over-test-a@x")
            .unwrap()
            .expect("fetch_message must hit over.db");
        assert_eq!(got.message_id, "over-test-a@x");
        assert_eq!(got.list, "linux-cifs");
        // Original-case from the ddd blob, not the lowercased index col.
        assert_eq!(got.from_addr.as_deref(), Some("Reviewer@Example.COM"));
        assert_eq!(got.subject_raw.as_deref(), Some("[PATCH 1/2] over-db wiring"));

        // Indexed eq scan: case-folded mid-case query should still hit.
        let by_from = reader
            .eq(EqField::FromAddr, "reviewer@example.com", None, None, 10)
            .unwrap();
        assert_eq!(by_from.len(), 1);
        assert_eq!(by_from[0].message_id, "over-test-a@x");

        // Indexed list scan, ordered date-DESC.
        let by_list = reader
            .eq(EqField::List, "linux-cifs", None, None, 10)
            .unwrap();
        assert_eq!(by_list.len(), 2);
        assert_eq!(by_list[0].message_id, "over-test-b@x");
        assert_eq!(by_list[1].message_id, "over-test-a@x");

        // all_rows(list:_) routes through over.db's list-date index.
        let all = reader
            .all_rows(Some("linux-cifs"), None, Some(100))
            .unwrap();
        assert_eq!(all.len(), 2);
    }

    // ---- Tier-consistency fixes (#1+#2) ----------------------------
    //
    // The three tests below lock in the hybrid fix documented in the
    // research brief:
    //   * per-tier generation marker guards against long-lived over.db
    //     drift at Reader-open time;
    //   * per-row fallback from over.db MISS to Parquet scan guards
    //     against within-generation partial drift (e.g. a shard's
    //     over.db INSERT failed while Parquet succeeded).

    /// Reader must refuse to open over.db when its per-tier generation
    /// marker is behind the corpus generation. Without this guard,
    /// readers silently return stale/incomplete results from a
    /// known-inconsistent over.db.
    #[test]
    fn reader_disables_over_db_when_marker_behind() {
        use crate::over::OverDb;
        use crate::state::State;

        let dir = tempdir().unwrap();
        let state = State::new(dir.path()).unwrap();

        // Simulate: corpus generation advanced to 5, but the over.db
        // marker is stuck at 3 (e.g. a shard's over.db write failed).
        std::fs::write(dir.path().join("state").join("generation"), "5\n").unwrap();
        state.set_tier_generation("over", 3).unwrap();
        // Open an over.db so the file exists on disk.
        let _ = OverDb::open(&dir.path().join("over.db")).unwrap();

        let reader = Reader::new(dir.path());
        // over.db is present on disk but must NOT be used by Reader
        // because the marker says it's stale.
        assert!(
            reader.over_db().is_none(),
            "Reader opened stale over.db; marker=3 vs corpus=5"
        );
    }

    /// Reader must use over.db when the marker is current.
    #[test]
    fn reader_uses_over_db_when_marker_current() {
        use crate::over::OverDb;
        use crate::state::State;

        let dir = tempdir().unwrap();
        let state = State::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("state").join("generation"), "5\n").unwrap();
        state.set_tier_generation("over", 5).unwrap();
        let _ = OverDb::open(&dir.path().join("over.db")).unwrap();

        let reader = Reader::new(dir.path());
        assert!(
            reader.over_db().is_some(),
            "Reader disabled over.db despite marker being current"
        );
    }

    /// Backward-compat: a legacy deployment has a corpus generation
    /// advanced past 0 but never wrote per-tier marker files. The
    /// Reader must NOT disable over.db in that case — the missing
    /// marker is "pre-upgrade state", not "known drift". Strict
    /// checking kicks in only after the first post-upgrade ingest
    /// writes the marker.
    #[test]
    fn reader_uses_over_db_when_marker_absent_but_corpus_nonzero() {
        use crate::over::OverDb;

        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("state")).unwrap();
        std::fs::write(dir.path().join("state").join("generation"), "17\n").unwrap();
        // Deliberately do NOT write state/over.generation.
        let _ = OverDb::open(&dir.path().join("over.db")).unwrap();

        let reader = Reader::new(dir.path());
        assert!(
            reader.over_db().is_some(),
            "Reader must trust over.db on a legacy deployment (corpus advanced, no tier marker yet)"
        );
    }

    /// Repeated Store opens for the same list resolve to the same
    /// underlying cached handle. Not a perf test (hard to measure
    /// without a bench harness on this box) — an identity check that
    /// pins the cache contract.
    #[test]
    fn store_cache_returns_same_handle() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let reader = Reader::new(d.path());
        let a = reader.store_for("linux-cifs").unwrap();
        let b = reader.store_for("linux-cifs").unwrap();
        assert!(Arc::ptr_eq(&a, &b), "store cache returned a new Store");
    }

    /// fetch_message MUST fall through to Parquet on an over.db miss.
    /// This is the core correctness fix: before, an over.db `Ok(None)`
    /// return caused fetch_message to return Ok(None) itself, even
    /// when the row was sitting in a Parquet file — silently dropping
    /// real corpus rows inside the partial-ingest window.
    #[test]
    fn fetch_message_falls_through_to_parquet_on_over_db_miss() {
        use crate::over::OverDb;
        use crate::state::State;

        // Build Parquet via the normal ingest path. This writes
        // over.db rows too if we were to pass one through — but
        // ingest_shard (the non-over variant) does not. So Parquet
        // has rows, over.db is empty. That's our "partial" scenario.
        let d = tempdir().unwrap();
        ingest_sample(d.path());

        // Create an EMPTY over.db at the expected location, and mark
        // it current so the Reader decides to use it.
        let state = State::new(d.path()).unwrap();
        let corpus_gen = state.generation().unwrap();
        let _empty_db = OverDb::open(&d.path().join("over.db")).unwrap();
        drop(_empty_db);
        state.set_tier_generation("over", corpus_gen).unwrap();

        let reader = Reader::new(d.path());
        assert!(
            reader.over_db().is_some(),
            "sanity: Reader should have opened over.db in this scenario"
        );

        // over.db knows about zero rows. Without the fix this returns
        // Ok(None) and the test would fail. With the fix the fallback
        // Parquet scan kicks in and finds the real row.
        let got = reader
            .fetch_message("m1@x")
            .unwrap()
            .expect("fetch_message must fall through to Parquet on over.db miss");
        assert_eq!(got.message_id, "m1@x");
    }
}
