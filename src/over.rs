//! `over.db` — SQLite-backed metadata tier modeled on
//! public-inbox's `over.sqlite3`.
//!
//! Indexed columns mirror the most common router predicates
//! (`mid:`, `f:`, `list:`, `since:`, `tid:`, `in_reply_to:`).
//! Everything else (display fields, trailers, touched-files lists, …)
//! lives in a single zstd-compressed msgpack BLOB column (`ddd`).
//! That keeps the row width small for fast index scans while still
//! letting `get()` return a fully-materialized `MessageRow` without
//! a Parquet round-trip.
//!
//! See `docs/plans/2026-04-17-overdb-metadata-tier.md` for the full
//! design rationale.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::reader::{EqField, MessageRow};

/// Bumped whenever the SQL schema or the `ddd` payload format
/// changes in a way that requires a rebuild. Persisted in the
/// `meta` table; mismatched DBs refuse to open.
pub const SCHEMA_VERSION: i64 = 1;

/// zstd compression level for `ddd`. Decode latency dominates over
/// build cost (we encode once, decode many times); 3 hits the sweet
/// spot for header-text payloads (~4-5x ratio).
const DDD_ZSTD_LEVEL: i32 = 3;

/// SQLite parameter limit (default `SQLITE_LIMIT_VARIABLE_NUMBER`
/// is 999 in the bundled build). We chunk `IN (?,?,…)` lookups
/// at this size.
const SQLITE_PARAM_LIMIT: usize = 999;

/// Rust-side mirror of the `over` table row layout. Owned fields
/// (no borrows) so callers can construct rows from any source and
/// pass them straight into `insert_batch`.
#[derive(Debug, Clone, PartialEq)]
pub struct OverRow {
    pub message_id: String,
    pub list: String,
    /// MUST be lowercased by the caller. `insert_batch` lowercases
    /// defensively, but constructing this with mixed case in the
    /// indexed column would mask bugs in the producer.
    pub from_addr: Option<String>,
    pub date_unix_ns: Option<i64>,
    pub in_reply_to: Option<String>,
    pub tid: Option<String>,

    pub body_segment_id: i64,
    pub body_offset: i64,
    pub body_length: i64,
    pub body_sha256: String,

    pub has_patch: bool,
    pub is_cover_letter: bool,
    pub series_version: Option<i64>,
    pub series_index: Option<i64>,
    pub series_total: Option<i64>,

    pub files_changed: Option<i64>,
    pub insertions: Option<i64>,
    pub deletions: Option<i64>,
    pub commit_oid: Option<String>,

    /// Display-only fields, encoded into the BLOB column.
    pub ddd: DddPayload,
}

/// Display payload — everything not promoted to an indexed column.
/// Encoded as zstd(msgpack(self)) and stored in `over.ddd`.
///
/// `from_addr_original_case` preserves the as-received casing for
/// display; the indexed `over.from_addr` column holds the
/// case-folded version (we lowercase at INSERT time so the index
/// can serve case-insensitive lookups without LIKE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DddPayload {
    pub subject_raw: Option<String>,
    pub subject_normalized: Option<String>,
    pub subject_tags: Vec<String>,
    pub references: Vec<String>,
    pub touched_files: Vec<String>,
    pub touched_functions: Vec<String>,
    pub signed_off_by: Vec<String>,
    pub reviewed_by: Vec<String>,
    pub acked_by: Vec<String>,
    pub tested_by: Vec<String>,
    pub co_developed_by: Vec<String>,
    pub reported_by: Vec<String>,
    pub suggested_by: Vec<String>,
    pub helped_by: Vec<String>,
    pub assisted_by: Vec<String>,
    pub fixes: Vec<String>,
    pub link: Vec<String>,
    pub closes: Vec<String>,
    pub cc_stable: Vec<String>,
    pub trailers_json: Option<String>,
    pub from_name: Option<String>,
    pub from_addr_original_case: Option<String>,
    pub shard: Option<String>,
}

/// One row of per-list corpus stats. Produced by `per_list_stats`
/// and surfaced through the `stats://coverage` MCP resource.
#[derive(Debug, Clone, PartialEq)]
pub struct PerListStats {
    pub list: String,
    pub rows: u64,
    pub earliest_date_unix_ns: Option<i64>,
    pub latest_date_unix_ns: Option<i64>,
}

/// Owning handle around a rusqlite `Connection` plus the bookkeeping
/// we want to keep with it (path, schema version checks).
pub struct OverDb {
    conn: Connection,
}

/// Read-side connection fanout. `rusqlite::Connection` is `!Sync`,
/// but SQLite in WAL mode lets N independent connections read the
/// same file concurrently with zero lock contention. The old
/// `Arc<Mutex<OverDb>>` serialized every query on a single
/// connection; this structure opens a small fixed number of
/// connections up front and picks an uncontended one per query.
///
/// Design notes:
///   * Fixed pool, N connections opened eagerly at Reader startup.
///     Cheap amortized — each connection costs ~200 MB cache header
///     space, but SQLite's mmap and OS page cache are shared.
///   * `with_conn` probes each slot with `try_lock`; on all-busy it
///     blocks on a round-robin target. No condition variables, no
///     spin loops, no dependencies.
///   * Not a general-purpose pool — no idle timeout, no max-lifetime,
///     no health check. The MCP server is single-process and dies on
///     unrecoverable SQLite errors anyway; growth/shrink would add
///     complexity with no observable benefit for our shape of load.
pub struct OverDbPool {
    conns: Vec<std::sync::Mutex<OverDb>>,
    next: std::sync::atomic::AtomicUsize,
}

impl OverDbPool {
    /// Open `size` independent read-side connections. Each runs the
    /// full `OverDb::open` sequence (WAL + pragmas + schema-version
    /// check). `size` must be > 0.
    pub fn open(path: &Path, size: usize) -> Result<Self> {
        let size = size.max(1);
        let mut conns = Vec::with_capacity(size);
        for _ in 0..size {
            conns.push(std::sync::Mutex::new(OverDb::open(path)?));
        }
        Ok(Self {
            conns,
            next: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Run `f` against one of the pool connections. First tries each
    /// slot non-blocking; falls back to blocking on a round-robin
    /// target when every connection is in use.
    pub fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&OverDb) -> Result<T>,
    {
        for conn in &self.conns {
            if let Ok(guard) = conn.try_lock() {
                return f(&guard);
            }
        }
        let idx = self
            .next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % self.conns.len();
        let guard = self.conns[idx]
            .lock()
            .map_err(|_| Error::State("over.db pool mutex poisoned".to_owned()))?;
        f(&guard)
    }

    pub fn size(&self) -> usize {
        self.conns.len()
    }
}

impl OverDb {
    /// Open or create `over.db` at `path`. On creation, runs the
    /// schema migration. On open of an existing DB, verifies that
    /// `meta.schema_version` matches `SCHEMA_VERSION` and returns
    /// `Error::State` otherwise.
    pub fn open(path: &Path) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        let conn = Connection::open_with_flags(path, flags)?;
        Self::configure(&conn)?;
        Self::migrate(&conn)?;
        Self::create_indexes_in(&conn)?;
        Self::verify_schema_version(&conn)?;
        Ok(Self { conn })
    }

    /// Bulk-load constructor: opens (or creates) the DB, runs the
    /// table-only migration, and DEFERS index creation. Build paths
    /// must call `create_indexes()` after the final `insert_batch` —
    /// CREATE INDEX over a populated table is dramatically faster
    /// than maintaining indices through millions of INSERTs (see
    /// SQLite docs: "bulk loads"). Without that final call the DB is
    /// missing its indexes and `OverDb::open()` on it would still
    /// idempotently create them, but a build-time crash will leave a
    /// half-indexed file behind — that's why we write to a tempfile
    /// and atomic-rename only on success.
    pub fn open_for_bulk_load(path: &Path) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        let conn = Connection::open_with_flags(path, flags)?;
        Self::configure(&conn)?;
        Self::migrate(&conn)?;
        // Bulk-load tweaks: trade durability for build throughput.
        // The whole build is in a tempfile that is atomically renamed
        // only on success, so a crash here leaves no trace in the
        // production over.db.
        conn.pragma_update(None, "synchronous", "OFF")?;
        conn.pragma_update(None, "journal_mode", "MEMORY")?;
        Self::verify_schema_version(&conn)?;
        Ok(Self { conn })
    }

    /// Build the indexes declared in the schema. Idempotent (every
    /// CREATE INDEX uses IF NOT EXISTS). Call once after `insert_batch`
    /// loops finish in the build binary.
    pub fn create_indexes(&self) -> Result<()> {
        Self::create_indexes_in(&self.conn)
    }

    /// Run `PRAGMA optimize` then `VACUUM`. Cheap on a fresh build;
    /// reclaims space and updates stats so the query planner picks
    /// the right index.
    pub fn finalize(&self) -> Result<()> {
        // optimize must run before vacuum: it rewrites stat1 entries
        // that vacuum will then compact.
        self.conn.pragma_update(None, "optimize", "")?;
        self.conn.execute_batch("VACUUM;")?;
        Ok(())
    }

    /// Set `meta.built_at` to the supplied ISO 8601 timestamp.
    pub fn set_built_at(&self, ts: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('built_at', ?1)",
            [ts],
        )?;
        Ok(())
    }

    /// Total row count. Cheap; used by the build binary for the final
    /// "wrote N rows" log line.
    pub fn row_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM over", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Coverage stats by list — one row per mailing list the corpus
    /// has data for, with the window of dates we hold and the row
    /// count. Powers the `stats://coverage` MCP resource and the
    /// `lore_corpus_stats` tool so agents can ask "what IS in here,
    /// and how fresh per list" without a private channel.
    ///
    /// Walks `over` GROUP BY list. The composite `over_list_date`
    /// index lets the planner stream the group-by without a sort;
    /// measured ~1 s on the 17.7M-row klmcp-local corpus. Deliberately
    /// does NOT compute DISTINCT from_addr per list — that would
    /// double the cost and callers who care can ask separately.
    pub fn per_list_stats(&self) -> Result<Vec<PerListStats>> {
        let mut stmt = self.conn.prepare(
            "SELECT list, COUNT(*), MIN(date_unix_ns), MAX(date_unix_ns) \
             FROM over GROUP BY list ORDER BY list ASC",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(PerListStats {
                list: r.get(0)?,
                rows: r.get::<_, i64>(1)? as u64,
                earliest_date_unix_ns: r.get(2)?,
                latest_date_unix_ns: r.get(3)?,
            });
        }
        Ok(out)
    }

    /// Test-only constructor that uses an in-memory database.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        Self::migrate(&conn)?;
        Self::create_indexes_in(&conn)?;
        Self::verify_schema_version(&conn)?;
        Ok(Self { conn })
    }

    fn configure(conn: &Connection) -> Result<()> {
        // mmap_size: 256 MB. Earlier we used 4 GB but a Phase 5
        // benchmark showed it pushed reader peak RSS to 1.75 GB
        // (failed the <500 MB target) without measurable latency
        // gain — point lookups touch a tiny working set. The build
        // binary overrides this in open_for_bulk_load.
        // cache_size: -200_000 = 200 MB (negative = absolute bytes).
        // synchronous=NORMAL is safe under WAL and gives ~3x write
        // throughput vs FULL.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "mmap_size", 268_435_456_i64)?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "cache_size", -200_000_i64)?;
        Ok(())
    }

    fn migrate(conn: &Connection) -> Result<()> {
        // Tables only — indexes are split out into `create_indexes_in`
        // so the build binary can defer them until after bulk-load.
        // The unique (message_id, list) index is also deferred; the
        // build binary trusts upstream dedup (Reader::scan) and creates
        // it at finalize time. INSERT OR REPLACE during the build is a
        // no-op on a fresh table, so the missing constraint is fine.
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS over (
                rowid               INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id          TEXT    NOT NULL,
                list                TEXT    NOT NULL,
                from_addr           TEXT,
                date_unix_ns        INTEGER,
                in_reply_to         TEXT,
                tid                 TEXT,
                body_segment_id     INTEGER NOT NULL,
                body_offset         INTEGER NOT NULL,
                body_length         INTEGER NOT NULL,
                body_sha256         TEXT    NOT NULL,
                has_patch           INTEGER NOT NULL DEFAULT 0,
                is_cover_letter     INTEGER NOT NULL DEFAULT 0,
                series_version      INTEGER,
                series_index        INTEGER,
                series_total        INTEGER,
                files_changed       INTEGER,
                insertions          INTEGER,
                deletions           INTEGER,
                commit_oid          TEXT,
                subject_normalized  TEXT,
                ddd                 BLOB    NOT NULL
            );

            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            -- Side table for list-shaped trailer fields. One row per
            -- (trailer kind, extracted email, source over row). Keeping
            -- every list-shaped trailer in one table (rather than one
            -- per kind) lets us add new kinds by writing rows with a
            -- new `kind` string — no further schema migrations.
            --
            -- `email` is the lowercased user@host extracted from the
            -- raw trailer line ("Signed-off-by: Name <email>" → email).
            -- Entries preserving the full trailer text live in the
            -- `over.ddd` blob; this table is purely an index surface.
            CREATE TABLE IF NOT EXISTS over_trailer_email (
                kind       TEXT NOT NULL,
                email      TEXT NOT NULL,
                message_id TEXT NOT NULL,
                list       TEXT NOT NULL,
                PRIMARY KEY (kind, email, message_id, list)
            );

            -- Side table for the list-shaped `touched_files` field.
            -- One row per (path, source over row). Same shape as the
            -- trailer-email table but keyed by file path instead of
            -- email, because touched_files is a distinct semantic
            -- axis and mixing them in one "kind" table would muddy
            -- schema comments + widen the primary-key collisions.
            CREATE TABLE IF NOT EXISTS over_touched_file (
                path       TEXT NOT NULL,
                message_id TEXT NOT NULL,
                list       TEXT NOT NULL,
                PRIMARY KEY (path, message_id, list)
            );
            "#,
        )?;

        // Seed schema_version exactly once. INSERT OR IGNORE keeps
        // re-opens cheap.
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;

        // Idempotent in-place migrations for older over.db files.
        // SQLite has no `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`,
        // so we probe PRAGMA table_info. Keep these cheap — they run
        // on every open.
        if !column_exists(conn, "over", "subject_normalized")? {
            conn.execute_batch("ALTER TABLE over ADD COLUMN subject_normalized TEXT;")?;
        }
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('source_tier', 'parquet:metadata/')",
            [],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('built_at', '')",
            [],
        )?;
        Ok(())
    }

    fn create_indexes_in(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE INDEX IF NOT EXISTS over_msgid      ON over (message_id);
            -- Composite (from_addr, date_unix_ns DESC) lets popular-author
            -- queries (gregkh, kuba, ...) pull the newest N matches via
            -- index-order traversal instead of materializing 10k+ rows then
            -- sorting. Phase 5 measured 5.4s p95 with single-column index;
            -- this drops it under 50ms.
            CREATE INDEX IF NOT EXISTS over_from_date  ON over (from_addr, date_unix_ns DESC);
            CREATE INDEX IF NOT EXISTS over_list_date  ON over (list, date_unix_ns DESC);
            CREATE INDEX IF NOT EXISTS over_date       ON over (date_unix_ns DESC);
            CREATE INDEX IF NOT EXISTS over_tid        ON over (tid);
            CREATE INDEX IF NOT EXISTS over_reply      ON over (in_reply_to);

            -- body_sha256 / commit_oid are already scalar columns on
            -- every row; indexing them promotes eq() on these fields
            -- from a full ddd-decode sequential scan to a point lookup.
            -- body_sha256 is NOT NULL; commit_oid is nullable (non-
            -- patches have no mainline OID) so partial-index it to keep
            -- the index file small.
            CREATE INDEX IF NOT EXISTS over_body_sha256 ON over (body_sha256);
            CREATE INDEX IF NOT EXISTS over_commit_oid  ON over (commit_oid)
                WHERE commit_oid IS NOT NULL;

            -- Partial index on populated rows only: backfill of existing
            -- over.db files is an explicit operation (kernel-lore-backfill-
            -- over subject_normalized) so the column may legitimately be
            -- NULL on older rows. Partial indexes keep the on-disk size
            -- proportional to the populated fraction.
            CREATE INDEX IF NOT EXISTS over_subject_normalized
                ON over (subject_normalized)
                WHERE subject_normalized IS NOT NULL;

            -- Reverse lookup on the trailer side table for cascading
            -- DELETE on REPLACE. The (kind, email, ...) primary key
            -- already supports the forward lookup used by scan_eq.
            CREATE INDEX IF NOT EXISTS over_trailer_email_mid_list
                ON over_trailer_email (message_id, list);
            CREATE INDEX IF NOT EXISTS over_touched_file_mid_list
                ON over_touched_file (message_id, list);

            -- (message_id, list) is the natural identity key. Cross-posts
            -- legitimately share message_id across lists, so we cannot
            -- make message_id alone UNIQUE. INSERT OR REPLACE on this
            -- index gives us re-ingest idempotency.
            CREATE UNIQUE INDEX IF NOT EXISTS over_mid_list ON over (message_id, list);
            "#,
        )?;
        Ok(())
    }

    fn verify_schema_version(conn: &Connection) -> Result<()> {
        let v: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        let v = v.ok_or_else(|| Error::State("over.db missing schema_version".into()))?;
        let parsed: i64 = v
            .parse()
            .map_err(|e| Error::State(format!("schema_version not an integer ({v}): {e}")))?;
        if parsed != SCHEMA_VERSION {
            return Err(Error::State(format!(
                "over.db schema_version {parsed} != expected {SCHEMA_VERSION}; rebuild required"
            )));
        }
        Ok(())
    }

    /// Insert (or replace) a batch of rows in a single transaction.
    /// Idempotent on `(message_id, list)` via the unique index.
    pub fn insert_batch(&mut self, rows: &[OverRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        Self::insert_batch_in_tx(&tx, rows)?;
        tx.commit()?;
        Ok(())
    }

    /// Bulk-update the `tid` column for many message_ids. Used by
    /// `tid::rebuild` to backfill the cross-corpus thread-id mapping
    /// into over.db after the side-table is built. Cross-posts
    /// (multiple rows with the same message_id) all get the same tid,
    /// which is correct.
    ///
    /// Strategy: chunk into batches of CHUNK rows; each batch is its
    /// own transaction. Between batches, force a WAL checkpoint so
    /// the WAL stays bounded. A single-transaction UPDATE on 17.6M
    /// rows generated an 11+ GB WAL (page-level journaling rewrites
    /// most of the table) and never finished within tolerance.
    ///
    /// Returns the number of rows updated.
    pub fn update_tids(&mut self, mid_to_tid: &[(String, String)]) -> Result<u64> {
        if mid_to_tid.is_empty() {
            return Ok(0);
        }
        const CHUNK: usize = 50_000;
        let mut total: u64 = 0;
        for batch in mid_to_tid.chunks(CHUNK) {
            let tx = self.conn.transaction()?;
            {
                let mut stmt =
                    tx.prepare("UPDATE over SET tid = ?1 WHERE message_id = ?2")?;
                for (mid, tid) in batch {
                    total += stmt.execute(rusqlite::params![tid, mid])? as u64;
                }
            }
            tx.commit()?;
            // Force WAL → main DB merge so the WAL doesn't accumulate
            // across batches. PASSIVE checkpoint is non-blocking and
            // bounded; what we care about is that the WAL doesn't grow
            // unboundedly while readers may also be holding snapshots.
            self.conn
                .pragma_update(None, "wal_checkpoint", "PASSIVE")?;
        }
        Ok(total)
    }

    /// Fill the `subject_normalized` column in-place for rows where
    /// it's currently NULL by decoding the `ddd` blob. Used to
    /// migrate over.db files built before subject_normalized was
    /// promoted to a column, without paying the full ~30 min rebuild
    /// cost of `kernel-lore-build-over`.
    ///
    /// Chunked writes with WAL checkpoints between batches, following
    /// the pattern established by `update_tids` to keep the WAL
    /// bounded. Returns the count of rows updated.
    pub fn backfill_subject_normalized(&mut self) -> Result<u64> {
        const CHUNK: usize = 50_000;
        // Rowid-cursor walk so rows whose decoded subject_normalized is
        // legitimately None don't get revisited on the next iteration
        // (WHERE subject_normalized IS NULL would loop forever on
        // those). Forward-only: we never revisit, so the cost is one
        // full pass over the table.
        let mut cursor: i64 = 0;
        let mut total: u64 = 0;
        loop {
            let mut pending: Vec<(i64, Option<String>)> = Vec::with_capacity(CHUNK);
            let mut last_rowid: i64 = cursor;
            {
                let mut stmt = self.conn.prepare(
                    "SELECT rowid, ddd FROM over \
                     WHERE rowid > ?1 AND subject_normalized IS NULL \
                     ORDER BY rowid ASC LIMIT ?2",
                )?;
                let mut rows = stmt.query(rusqlite::params![cursor, CHUNK as i64])?;
                while let Some(r) = rows.next()? {
                    let rowid: i64 = r.get(0)?;
                    let blob: Vec<u8> = r.get(1)?;
                    let subj = match decode_ddd(&blob) {
                        Ok(p) => p.subject_normalized,
                        Err(_) => None,
                    };
                    pending.push((rowid, subj));
                    last_rowid = rowid;
                }
            }
            if pending.is_empty() {
                break;
            }
            let tx = self.conn.transaction()?;
            {
                let mut stmt = tx.prepare(
                    "UPDATE over SET subject_normalized = ?1 \
                     WHERE rowid = ?2 AND subject_normalized IS NULL",
                )?;
                for (rowid, subj) in &pending {
                    if subj.is_some() {
                        total += stmt.execute(rusqlite::params![subj, rowid])? as u64;
                    }
                }
            }
            tx.commit()?;
            self.conn
                .pragma_update(None, "wal_checkpoint", "PASSIVE")?;
            cursor = last_rowid;
        }
        Ok(total)
    }

    /// Populate `over_touched_file` for every existing row by
    /// decoding its `ddd` blob and expanding `touched_files`.
    /// Idempotent — DELETE before INSERT per (message_id, list).
    /// Same rowid-cursor chunked pattern as the other backfills.
    pub fn backfill_touched_files(&mut self) -> Result<u64> {
        const CHUNK: usize = 50_000;
        let mut cursor: i64 = 0;
        let mut total: u64 = 0;
        loop {
            let mut pending: Vec<(String, String, Vec<String>)> = Vec::with_capacity(CHUNK);
            let mut last_rowid = cursor;
            {
                let mut stmt = self.conn.prepare(
                    "SELECT rowid, message_id, list, ddd FROM over \
                     WHERE rowid > ?1 ORDER BY rowid ASC LIMIT ?2",
                )?;
                let mut rows = stmt.query(rusqlite::params![cursor, CHUNK as i64])?;
                while let Some(r) = rows.next()? {
                    let rowid: i64 = r.get(0)?;
                    let mid: String = r.get(1)?;
                    let list: String = r.get(2)?;
                    let blob: Vec<u8> = r.get(3)?;
                    let paths = decode_ddd(&blob)
                        .map(|p| p.touched_files)
                        .unwrap_or_default();
                    pending.push((mid, list, paths));
                    last_rowid = rowid;
                }
            }
            if pending.is_empty() {
                break;
            }
            let tx = self.conn.transaction()?;
            {
                let mut del = tx.prepare(
                    "DELETE FROM over_touched_file \
                     WHERE message_id = ?1 AND list = ?2",
                )?;
                let mut ins = tx.prepare(
                    "INSERT OR IGNORE INTO over_touched_file \
                        (path, message_id, list) \
                     VALUES (?1, ?2, ?3)",
                )?;
                for (mid, list, paths) in &pending {
                    del.execute(rusqlite::params![mid, list])?;
                    for path in paths {
                        if !path.is_empty() {
                            total += ins.execute(rusqlite::params![path, mid, list])?
                                as u64;
                        }
                    }
                }
            }
            tx.commit()?;
            self.conn
                .pragma_update(None, "wal_checkpoint", "PASSIVE")?;
            cursor = last_rowid;
        }
        Ok(total)
    }

    /// Populate `over_trailer_email` for every existing row by
    /// decoding its `ddd` blob and extracting signed-off-by emails.
    /// Idempotent — DELETEs before INSERTing per (message_id, list).
    /// Chunked 50k rows per tx with WAL checkpoints. Returns the
    /// number of side-table rows written.
    pub fn backfill_trailer_emails(&mut self) -> Result<u64> {
        const CHUNK: usize = 50_000;
        let mut cursor: i64 = 0;
        let mut total: u64 = 0;
        loop {
            let mut pending: Vec<(String, String, DddPayload)> = Vec::with_capacity(CHUNK);
            let mut last_rowid = cursor;
            {
                let mut stmt = self.conn.prepare(
                    "SELECT rowid, message_id, list, ddd FROM over \
                     WHERE rowid > ?1 ORDER BY rowid ASC LIMIT ?2",
                )?;
                let mut rows = stmt.query(rusqlite::params![cursor, CHUNK as i64])?;
                while let Some(r) = rows.next()? {
                    let rowid: i64 = r.get(0)?;
                    let mid: String = r.get(1)?;
                    let list: String = r.get(2)?;
                    let blob: Vec<u8> = r.get(3)?;
                    let ddd = decode_ddd(&blob).unwrap_or_default();
                    pending.push((mid, list, ddd));
                    last_rowid = rowid;
                }
            }
            if pending.is_empty() {
                break;
            }
            let tx = self.conn.transaction()?;
            {
                let mut del = tx.prepare(
                    "DELETE FROM over_trailer_email \
                     WHERE message_id = ?1 AND list = ?2",
                )?;
                let mut ins = tx.prepare(
                    "INSERT OR IGNORE INTO over_trailer_email \
                        (kind, email, message_id, list) \
                     VALUES (?1, ?2, ?3, ?4)",
                )?;
                for (mid, list, ddd) in &pending {
                    del.execute(rusqlite::params![mid, list])?;
                    for (kind, raws) in trailer_email_sources(ddd) {
                        for raw in raws {
                            let email = crate::reader::extract_email(raw);
                            if !email.is_empty() {
                                total += ins.execute(rusqlite::params![
                                    kind, email, mid, list
                                ])? as u64;
                            }
                        }
                    }
                }
            }
            tx.commit()?;
            self.conn
                .pragma_update(None, "wal_checkpoint", "PASSIVE")?;
            cursor = last_rowid;
        }
        Ok(total)
    }

    fn insert_batch_in_tx(tx: &Transaction<'_>, rows: &[OverRow]) -> Result<()> {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO over (
                message_id, list, from_addr, date_unix_ns, in_reply_to, tid,
                body_segment_id, body_offset, body_length, body_sha256,
                has_patch, is_cover_letter,
                series_version, series_index, series_total,
                files_changed, insertions, deletions, commit_oid,
                subject_normalized, ddd
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6,
                ?7, ?8, ?9, ?10,
                ?11, ?12,
                ?13, ?14, ?15,
                ?16, ?17, ?18, ?19,
                ?20, ?21
            )",
        )?;
        // INSERT OR REPLACE on `over` drops and re-adds rows, so any
        // pre-existing side-table rows for the same (message_id, list)
        // are now stale and need to go. We delete first (scoped by
        // message_id + list, spanning every kind), then re-populate
        // from the fresh ddd payload kind-by-kind.
        let mut tr_del_all = tx.prepare(
            "DELETE FROM over_trailer_email \
             WHERE message_id = ?1 AND list = ?2",
        )?;
        let mut tr_ins = tx.prepare(
            "INSERT OR IGNORE INTO over_trailer_email \
                (kind, email, message_id, list) \
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut tf_del = tx.prepare(
            "DELETE FROM over_touched_file \
             WHERE message_id = ?1 AND list = ?2",
        )?;
        let mut tf_ins = tx.prepare(
            "INSERT OR IGNORE INTO over_touched_file \
                (path, message_id, list) \
             VALUES (?1, ?2, ?3)",
        )?;
        for row in rows {
            // Defensive lowercase: the indexed column is the lookup
            // surface, and `f:Foo@Bar` from the router resolves
            // through the same lower(value) path.
            let from_addr_lc = row.from_addr.as_deref().map(str::to_ascii_lowercase);
            let blob = encode_ddd(&row.ddd)?;
            stmt.execute(rusqlite::params![
                row.message_id,
                row.list,
                from_addr_lc,
                row.date_unix_ns,
                row.in_reply_to,
                row.tid,
                row.body_segment_id,
                row.body_offset,
                row.body_length,
                row.body_sha256,
                row.has_patch as i64,
                row.is_cover_letter as i64,
                row.series_version,
                row.series_index,
                row.series_total,
                row.files_changed,
                row.insertions,
                row.deletions,
                row.commit_oid,
                row.ddd.subject_normalized.as_deref(),
                blob,
            ])?;

            tr_del_all.execute(rusqlite::params![row.message_id, row.list])?;
            for (kind, raws) in trailer_email_sources(&row.ddd) {
                for raw in raws {
                    let email = crate::reader::extract_email(raw);
                    if !email.is_empty() {
                        tr_ins.execute(rusqlite::params![
                            kind,
                            email,
                            row.message_id,
                            row.list
                        ])?;
                    }
                }
            }

            // touched_files side table — same REPLACE discipline.
            tf_del.execute(rusqlite::params![row.message_id, row.list])?;
            for path in &row.ddd.touched_files {
                if !path.is_empty() {
                    tf_ins.execute(rusqlite::params![
                        path,
                        row.message_id,
                        row.list
                    ])?;
                }
            }
        }
        Ok(())
    }

    /// Point lookup by canonical message-id. If multiple rows match
    /// (cross-posts), returns the freshest by `date_unix_ns`.
    pub fn get(&self, message_id: &str) -> Result<Option<MessageRow>> {
        let mut stmt = self.conn.prepare_cached(SELECT_COLS_BASE_WHERE_MID)?;
        let mut best: Option<MessageRow> = None;
        let mut rows = stmt.query([message_id])?;
        while let Some(r) = rows.next()? {
            let mr = row_to_message(r)?;
            best = Some(match best.take() {
                Some(prev) => freshest(prev, mr),
                None => mr,
            });
        }
        Ok(best)
    }

    /// Batched point lookup. Returns one entry per *distinct*
    /// canonical message-id; cross-posts collapse to the freshest.
    pub fn get_many(&self, message_ids: &[String]) -> Result<HashMap<String, MessageRow>> {
        let mut out: HashMap<String, MessageRow> = HashMap::with_capacity(message_ids.len());
        if message_ids.is_empty() {
            return Ok(out);
        }
        for chunk in message_ids.chunks(SQLITE_PARAM_LIMIT) {
            let placeholders = (1..=chunk.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "{base} WHERE message_id IN ({placeholders})",
                base = SELECT_COLS_BASE
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query(params_from_iter(chunk.iter()))?;
            while let Some(r) = rows.next()? {
                let mr = row_to_message(r)?;
                let key = mr.message_id.clone();
                match out.remove(&key) {
                    Some(prev) => {
                        out.insert(key, freshest(prev, mr));
                    }
                    None => {
                        out.insert(key, mr);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Indexed scan by equality predicate, returning rows ordered
    /// newest-first.
    ///
    /// Supported `field` values use a dedicated index:
    ///   * `FromAddr`         — `over_from`, value lowercased
    ///   * `List`             — `over_list_date`
    ///   * `MessageId`        — `over_msgid` (delegates to `get()`)
    ///   * `InReplyTo`        — `over_reply`
    ///   * `Tid`              — `over_tid`
    ///
    /// Other variants fall through to a sequential scan over `ddd`
    /// (works, but slow — log a warning so we know to add an index).
    pub fn scan_eq(
        &self,
        field: EqField,
        value: &str,
        since_unix_ns: Option<i64>,
        list_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        if let EqField::MessageId = field {
            // get() already returns the freshest cross-post; wrap
            // into Vec for API uniformity. since/list filters apply.
            if let Some(mr) = self.get(value)? {
                if filters_ok(&mr, since_unix_ns, list_filter) {
                    return Ok(vec![mr]);
                }
            }
            return Ok(Vec::new());
        }

        // Side-table join path for list-shaped trailer fields. The
        // value is the user@host piece of a trailer line; the side
        // table was populated at insert time with one row per kind
        // per email per source row.
        if let Some(kind) = trailer_kind(field) {
            return self.scan_eq_via_trailer_email(
                kind,
                &value.to_ascii_lowercase(),
                since_unix_ns,
                list_filter,
                limit,
            );
        }

        // touched_files side-table fast path. Case-sensitive exact
        // match because kernel paths are case-sensitive (`fs/smb/...`
        // != `fs/SMB/...`).
        if matches!(field, EqField::TouchedFile) {
            return self.scan_eq_via_touched_file(
                value,
                since_unix_ns,
                list_filter,
                limit,
            );
        }

        let (where_clause, primary): (&str, String) = match field {
            EqField::FromAddr => ("from_addr = ?1", value.to_ascii_lowercase()),
            EqField::List => ("list = ?1", value.to_string()),
            EqField::InReplyTo => ("in_reply_to = ?1", value.to_string()),
            EqField::Tid => ("tid = ?1", value.to_string()),
            EqField::BodySha256 => ("body_sha256 = ?1", value.to_string()),
            EqField::CommitOid => ("commit_oid = ?1", value.to_string()),
            EqField::SubjectNormalized => {
                ("subject_normalized = ?1", value.to_string())
            }
            _ => {
                tracing::warn!(
                    field = ?field,
                    "scan_eq on non-indexed field; falling back to sequential scan"
                );
                return self.scan_eq_sequential(field, value, since_unix_ns, list_filter, limit);
            }
        };

        let mut sql = format!(
            "{base} WHERE {where_clause}",
            base = SELECT_COLS_BASE,
            where_clause = where_clause
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(primary)];
        let mut next_idx = 2_usize;
        if let Some(since) = since_unix_ns {
            sql.push_str(&format!(" AND date_unix_ns >= ?{next_idx}"));
            params.push(Box::new(since));
            next_idx += 1;
        }
        // List filter is redundant when the predicate is already
        // List, but cheap to apply and keeps callers' logic uniform.
        if let Some(list) = list_filter
            && !matches!(field, EqField::List)
        {
            sql.push_str(&format!(" AND list = ?{next_idx}"));
            params.push(Box::new(list.to_string()));
            next_idx += 1;
        }
        sql.push_str(&format!(
            " ORDER BY date_unix_ns DESC LIMIT ?{next_idx}"
        ));
        params.push(Box::new(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut rows = stmt.query(param_refs.as_slice())?;
        let mut out = Vec::with_capacity(limit.min(1024));
        while let Some(r) = rows.next()? {
            out.push(row_to_message(r)?);
        }
        Ok(out)
    }

    /// Indexed path for `eq('touched_files', <path>)`. JOIN against
    /// `over_touched_file` (populated at insert time) rebuilds full
    /// MessageRows in date-DESC order — same shape as the trailer-
    /// email fast path.
    fn scan_eq_via_touched_file(
        &self,
        path: &str,
        since_unix_ns: Option<i64>,
        list_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        let mut sql = String::from(
            "SELECT \
                o.message_id, o.list, o.from_addr, o.date_unix_ns, o.in_reply_to, o.tid, \
                o.body_segment_id, o.body_offset, o.body_length, o.body_sha256, \
                o.has_patch, o.is_cover_letter, o.series_version, o.series_index, o.series_total, \
                o.files_changed, o.insertions, o.deletions, o.commit_oid, o.ddd \
             FROM over_touched_file t \
             INNER JOIN over o \
                ON o.message_id = t.message_id AND o.list = t.list \
             WHERE t.path = ?1",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(path.to_string())];
        let mut next_idx = 2_usize;
        if let Some(since) = since_unix_ns {
            sql.push_str(&format!(" AND o.date_unix_ns >= ?{next_idx}"));
            params.push(Box::new(since));
            next_idx += 1;
        }
        if let Some(list) = list_filter {
            sql.push_str(&format!(" AND o.list = ?{next_idx}"));
            params.push(Box::new(list.to_string()));
            next_idx += 1;
        }
        sql.push_str(&format!(
            " ORDER BY o.date_unix_ns DESC LIMIT ?{next_idx}"
        ));
        params.push(Box::new(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut rows = stmt.query(param_refs.as_slice())?;
        let mut out = Vec::with_capacity(limit.min(1024));
        while let Some(r) = rows.next()? {
            out.push(row_to_message(r)?);
        }
        Ok(out)
    }

    /// Indexed path for list-shaped trailer fields whose extracted
    /// email was materialized into `over_trailer_email` at insert
    /// time. Joins the side table back against `over` to rebuild
    /// full MessageRows while walking in date-DESC order.
    fn scan_eq_via_trailer_email(
        &self,
        kind: &str,
        email_lc: &str,
        since_unix_ns: Option<i64>,
        list_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        let mut sql = String::from(
            "SELECT \
                o.message_id, o.list, o.from_addr, o.date_unix_ns, o.in_reply_to, o.tid, \
                o.body_segment_id, o.body_offset, o.body_length, o.body_sha256, \
                o.has_patch, o.is_cover_letter, o.series_version, o.series_index, o.series_total, \
                o.files_changed, o.insertions, o.deletions, o.commit_oid, o.ddd \
             FROM over_trailer_email t \
             INNER JOIN over o \
                ON o.message_id = t.message_id AND o.list = t.list \
             WHERE t.kind = ?1 AND t.email = ?2",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(kind.to_string()), Box::new(email_lc.to_string())];
        let mut next_idx = 3_usize;
        if let Some(since) = since_unix_ns {
            sql.push_str(&format!(" AND o.date_unix_ns >= ?{next_idx}"));
            params.push(Box::new(since));
            next_idx += 1;
        }
        if let Some(list) = list_filter {
            sql.push_str(&format!(" AND o.list = ?{next_idx}"));
            params.push(Box::new(list.to_string()));
            next_idx += 1;
        }
        sql.push_str(&format!(
            " ORDER BY o.date_unix_ns DESC LIMIT ?{next_idx}"
        ));
        params.push(Box::new(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut rows = stmt.query(param_refs.as_slice())?;
        let mut out = Vec::with_capacity(limit.min(1024));
        while let Some(r) = rows.next()? {
            out.push(row_to_message(r)?);
        }
        Ok(out)
    }

    fn scan_eq_sequential(
        &self,
        field: EqField,
        value: &str,
        since_unix_ns: Option<i64>,
        list_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        // Cheap envelope-side pre-filter; payload-side fields require
        // ddd decode, which is the expensive step.
        let mut sql = String::from(SELECT_COLS_BASE);
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let mut where_added = false;
        let mut next_idx = 1_usize;
        if let Some(since) = since_unix_ns {
            sql.push_str(&format!(" WHERE date_unix_ns >= ?{next_idx}"));
            params.push(Box::new(since));
            where_added = true;
            next_idx += 1;
        }
        if let Some(list) = list_filter {
            sql.push_str(if where_added { " AND" } else { " WHERE" });
            sql.push_str(&format!(" list = ?{next_idx}"));
            params.push(Box::new(list.to_string()));
            next_idx += 1;
        }
        sql.push_str(" ORDER BY date_unix_ns DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut rows = stmt.query(param_refs.as_slice())?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let mr = row_to_message(r)?;
            if message_matches_field(&mr, field, value) {
                out.push(mr);
                if out.len() >= limit {
                    break;
                }
            }
        }
        let _ = next_idx;
        Ok(out)
    }
}

/// Single source of truth mapping ddd trailer list fields to the
/// `over_trailer_email.kind` tokens they populate. Iterating this
/// keeps `insert_batch_in_tx` and `backfill_trailer_emails` in sync
/// by construction — adding a new kind means appending one tuple
/// plus a new `trailer_kind` match arm.
fn trailer_email_sources(ddd: &DddPayload) -> [(&'static str, &Vec<String>); 6] {
    [
        ("signed_off_by", &ddd.signed_off_by),
        ("reviewed_by", &ddd.reviewed_by),
        ("acked_by", &ddd.acked_by),
        ("tested_by", &ddd.tested_by),
        ("co_developed_by", &ddd.co_developed_by),
        ("reported_by", &ddd.reported_by),
    ]
}

/// Map an `EqField` to the `over_trailer_email.kind` token used by
/// the side-table side-indexed fast path. Returns `None` for fields
/// that aren't list-shaped-trailer-by-email.
fn trailer_kind(field: EqField) -> Option<&'static str> {
    match field {
        EqField::SignedOffBy => Some("signed_off_by"),
        EqField::ReviewedBy => Some("reviewed_by"),
        EqField::AckedBy => Some("acked_by"),
        EqField::TestedBy => Some("tested_by"),
        EqField::CoDevelopedBy => Some("co_developed_by"),
        EqField::ReportedBy => Some("reported_by"),
        _ => None,
    }
}

/// PRAGMA table_info probe for idempotent ALTER TABLE ADD COLUMN
/// migrations. Returns true when `table.column` exists.
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table});"))?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let name: String = r.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn filters_ok(mr: &MessageRow, since: Option<i64>, list_filter: Option<&str>) -> bool {
    if let Some(s) = since
        && mr.date_unix_ns.unwrap_or(i64::MIN) < s
    {
        return false;
    }
    if let Some(l) = list_filter
        && mr.list != l
    {
        return false;
    }
    true
}

fn freshest(a: MessageRow, b: MessageRow) -> MessageRow {
    let ad = a.date_unix_ns.unwrap_or(i64::MIN);
    let bd = b.date_unix_ns.unwrap_or(i64::MIN);
    if bd > ad { b } else { a }
}

fn message_matches_field(mr: &MessageRow, field: EqField, value: &str) -> bool {
    match field {
        EqField::CommitOid => mr.commit_oid == value,
        EqField::BodySha256 => mr.body_sha256 == value,
        EqField::SubjectNormalized => mr.subject_normalized.as_deref() == Some(value),
        EqField::TouchedFile => mr.touched_files.iter().any(|s| s == value),
        EqField::TouchedFunction => mr.touched_functions.iter().any(|s| s == value),
        EqField::Reference => mr.references.iter().any(|s| s == value),
        EqField::SubjectTag => mr.subject_tags.iter().any(|s| s == value),
        EqField::SignedOffBy => mr.signed_off_by.iter().any(|s| s == value),
        EqField::ReviewedBy => mr.reviewed_by.iter().any(|s| s == value),
        EqField::AckedBy => mr.acked_by.iter().any(|s| s == value),
        EqField::TestedBy => mr.tested_by.iter().any(|s| s == value),
        EqField::CoDevelopedBy => mr.co_developed_by.iter().any(|s| s == value),
        EqField::ReportedBy => mr.reported_by.iter().any(|s| s == value),
        EqField::Fixes => mr.fixes.iter().any(|s| s == value),
        EqField::Link => mr.link.iter().any(|s| s == value),
        EqField::Closes => mr.closes.iter().any(|s| s == value),
        EqField::CcStable => mr.cc_stable.iter().any(|s| s == value),
        // Indexed paths handle these.
        EqField::MessageId
        | EqField::List
        | EqField::FromAddr
        | EqField::InReplyTo
        | EqField::Tid => false,
    }
}

// Column projection shared by every SELECT path. Order matches
// `row_to_message` indices below.
const SELECT_COLS_BASE: &str = "SELECT \
        message_id, list, from_addr, date_unix_ns, in_reply_to, tid, \
        body_segment_id, body_offset, body_length, body_sha256, \
        has_patch, is_cover_letter, series_version, series_index, series_total, \
        files_changed, insertions, deletions, commit_oid, ddd \
        FROM over";

const SELECT_COLS_BASE_WHERE_MID: &str = "SELECT \
        message_id, list, from_addr, date_unix_ns, in_reply_to, tid, \
        body_segment_id, body_offset, body_length, body_sha256, \
        has_patch, is_cover_letter, series_version, series_index, series_total, \
        files_changed, insertions, deletions, commit_oid, ddd \
        FROM over WHERE message_id = ?1";

fn row_to_message(r: &rusqlite::Row<'_>) -> Result<MessageRow> {
    let message_id: String = r.get(0)?;
    let list: String = r.get(1)?;
    let from_addr_lc: Option<String> = r.get(2)?;
    let date_unix_ns: Option<i64> = r.get(3)?;
    let in_reply_to: Option<String> = r.get(4)?;
    let tid: Option<String> = r.get(5)?;
    let body_segment_id: i64 = r.get(6)?;
    let body_offset: i64 = r.get(7)?;
    let body_length: i64 = r.get(8)?;
    let body_sha256: String = r.get(9)?;
    let has_patch: i64 = r.get(10)?;
    let is_cover_letter: i64 = r.get(11)?;
    let series_version: Option<i64> = r.get(12)?;
    let series_index: Option<i64> = r.get(13)?;
    let series_total: Option<i64> = r.get(14)?;
    let files_changed: Option<i64> = r.get(15)?;
    let insertions: Option<i64> = r.get(16)?;
    let deletions: Option<i64> = r.get(17)?;
    let commit_oid: Option<String> = r.get(18)?;
    let blob: Vec<u8> = r.get(19)?;

    let ddd = decode_ddd(&blob)?;

    // Display path prefers the original-case from_addr from the
    // payload; the indexed lowercase form is the fallback so we
    // never lose the field entirely.
    let from_addr = ddd.from_addr_original_case.clone().or(from_addr_lc);

    Ok(MessageRow {
        message_id,
        list,
        shard: ddd.shard.clone().unwrap_or_default(),
        commit_oid: commit_oid.unwrap_or_default(),
        from_addr,
        from_name: ddd.from_name.clone(),
        subject_raw: ddd.subject_raw.clone(),
        subject_normalized: ddd.subject_normalized.clone(),
        subject_tags: ddd.subject_tags.clone(),
        date_unix_ns,
        in_reply_to,
        references: ddd.references.clone(),
        tid,
        series_version: series_version.unwrap_or(0) as u32,
        series_index: series_index.map(|v| v as u32),
        series_total: series_total.map(|v| v as u32),
        is_cover_letter: is_cover_letter != 0,
        has_patch: has_patch != 0,
        touched_files: ddd.touched_files.clone(),
        touched_functions: ddd.touched_functions.clone(),
        files_changed: files_changed.map(|v| v as u32),
        insertions: insertions.map(|v| v as u32),
        deletions: deletions.map(|v| v as u32),
        signed_off_by: ddd.signed_off_by.clone(),
        reviewed_by: ddd.reviewed_by.clone(),
        acked_by: ddd.acked_by.clone(),
        tested_by: ddd.tested_by.clone(),
        co_developed_by: ddd.co_developed_by.clone(),
        reported_by: ddd.reported_by.clone(),
        fixes: ddd.fixes.clone(),
        link: ddd.link.clone(),
        closes: ddd.closes.clone(),
        cc_stable: ddd.cc_stable.clone(),
        suggested_by: ddd.suggested_by.clone(),
        helped_by: ddd.helped_by.clone(),
        assisted_by: ddd.assisted_by.clone(),
        trailers_json: ddd.trailers_json.clone(),
        body_segment_id: body_segment_id as u32,
        body_offset: body_offset as u64,
        body_length: body_length as u64,
        body_sha256,
        schema_version: SCHEMA_VERSION as u32,
    })
}

fn encode_ddd(payload: &DddPayload) -> Result<Vec<u8>> {
    let raw = rmp_serde::to_vec_named(payload)?;
    let compressed = zstd::stream::encode_all(raw.as_slice(), DDD_ZSTD_LEVEL)?;
    Ok(compressed)
}

fn decode_ddd(blob: &[u8]) -> Result<DddPayload> {
    let raw = zstd::stream::decode_all(blob)?;
    let payload: DddPayload = rmp_serde::from_slice(&raw)?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_row(mid: &str, list: &str, date: i64, from: &str) -> OverRow {
        OverRow {
            message_id: mid.to_string(),
            list: list.to_string(),
            from_addr: Some(from.to_string()),
            date_unix_ns: Some(date),
            in_reply_to: None,
            tid: Some(format!("tid-{mid}")),
            body_segment_id: 1,
            body_offset: 0,
            body_length: 100,
            body_sha256: "sha-".to_string() + mid,
            has_patch: false,
            is_cover_letter: false,
            series_version: Some(1),
            series_index: None,
            series_total: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            commit_oid: Some("oid-".to_string() + mid),
            ddd: DddPayload {
                subject_raw: Some(format!("subj for {mid}")),
                subject_normalized: Some(format!("subj for {mid}")),
                subject_tags: vec!["PATCH".to_string()],
                references: vec![],
                touched_files: vec!["fs/foo.c".to_string()],
                touched_functions: vec!["foo_init".to_string()],
                signed_off_by: vec!["A. Person <a@example.com>".to_string()],
                reviewed_by: vec![],
                acked_by: vec![],
                tested_by: vec![],
                co_developed_by: vec![],
                reported_by: vec![],
                suggested_by: vec![],
                helped_by: vec![],
                assisted_by: vec![],
                fixes: vec![],
                link: vec![],
                closes: vec![],
                cc_stable: vec![],
                trailers_json: None,
                from_name: Some("A. Person".to_string()),
                from_addr_original_case: Some(from.to_string()),
                shard: Some("shard0".to_string()),
            },
        }
    }

    #[test]
    fn open_creates_schema_and_reopen_works() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("over.db");
        {
            let _db = OverDb::open(&p).unwrap();
        }
        // Reopen: should not fail or re-migrate destructively.
        let db = OverDb::open(&p).unwrap();
        let v: String = db
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn schema_version_mismatch_errors() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("over.db");
        {
            let db = OverDb::open(&p).unwrap();
            db.conn
                .execute(
                    "UPDATE meta SET value = '99' WHERE key = 'schema_version'",
                    [],
                )
                .unwrap();
        }
        match OverDb::open(&p) {
            Ok(_) => panic!("expected schema_version mismatch error"),
            Err(Error::State(msg)) => assert!(msg.contains("schema_version")),
            Err(other) => panic!("expected State error, got {other:?}"),
        }
    }

    #[test]
    fn insert_get_single_round_trip() {
        let mut db = OverDb::open_in_memory().unwrap();
        let row = sample_row("<a@b>", "lkml", 1_000, "Foo@EXAMPLE.com");
        db.insert_batch(std::slice::from_ref(&row)).unwrap();
        let got = db.get("<a@b>").unwrap().expect("row missing");
        assert_eq!(got.message_id, "<a@b>");
        assert_eq!(got.list, "lkml");
        // Display preserves original casing.
        assert_eq!(got.from_addr.as_deref(), Some("Foo@EXAMPLE.com"));
        assert_eq!(got.subject_raw.as_deref(), Some("subj for <a@b>"));
        assert_eq!(got.touched_files, vec!["fs/foo.c".to_string()]);
        assert_eq!(got.commit_oid, "oid-<a@b>");
    }

    #[test]
    fn insert_get_one_hundred_rows() {
        let mut db = OverDb::open_in_memory().unwrap();
        let rows: Vec<_> = (0..100)
            .map(|i| {
                sample_row(
                    &format!("<m{i}@x>"),
                    "lkml",
                    1_000 + i as i64,
                    "user@example.com",
                )
            })
            .collect();
        db.insert_batch(&rows).unwrap();
        for i in 0..100 {
            let mid = format!("<m{i}@x>");
            let got = db.get(&mid).unwrap().expect("row missing");
            assert_eq!(got.message_id, mid);
            assert_eq!(got.date_unix_ns, Some(1_000 + i as i64));
        }
    }

    #[test]
    fn get_many_partial_hits() {
        let mut db = OverDb::open_in_memory().unwrap();
        let rows: Vec<_> = (0..50)
            .map(|i| sample_row(&format!("<m{i}@x>"), "lkml", 1_000 + i as i64, "u@x"))
            .collect();
        db.insert_batch(&rows).unwrap();

        let mut ids: Vec<String> = (0..50).map(|i| format!("<m{i}@x>")).collect();
        ids.push("<missing1@x>".to_string());
        ids.push("<missing2@x>".to_string());

        let got = db.get_many(&ids).unwrap();
        assert_eq!(got.len(), 50);
        for i in 0..50 {
            let mid = format!("<m{i}@x>");
            assert!(got.contains_key(&mid), "missing {mid}");
        }
        assert!(!got.contains_key("<missing1@x>"));
    }

    #[test]
    fn scan_eq_from_addr_lowercases() {
        let mut db = OverDb::open_in_memory().unwrap();
        let rows = vec![
            sample_row("<a1@x>", "lkml", 1_000, "Foo@Example.COM"),
            sample_row("<a2@x>", "lkml", 2_000, "FOO@EXAMPLE.COM"),
            sample_row("<a3@x>", "lkml", 3_000, "bar@example.com"),
        ];
        db.insert_batch(&rows).unwrap();

        let hits = db
            .scan_eq(EqField::FromAddr, "foo@example.com", None, None, 10)
            .unwrap();
        assert_eq!(hits.len(), 2);
        // Newest-first.
        assert_eq!(hits[0].message_id, "<a2@x>");
        assert_eq!(hits[1].message_id, "<a1@x>");

        // Mixed-case query still resolves.
        let hits2 = db
            .scan_eq(EqField::FromAddr, "Foo@Example.com", None, None, 10)
            .unwrap();
        assert_eq!(hits2.len(), 2);
    }

    #[test]
    fn scan_eq_list_with_limit_and_order() {
        let mut db = OverDb::open_in_memory().unwrap();
        let mut rows = Vec::new();
        for i in 0..10 {
            rows.push(sample_row(
                &format!("<n{i}@x>"),
                "netdev",
                10_000 + i as i64,
                "u@x",
            ));
        }
        rows.push(sample_row("<other@x>", "lkml", 50_000, "u@x"));
        db.insert_batch(&rows).unwrap();

        let hits = db
            .scan_eq(EqField::List, "netdev", None, None, 5)
            .unwrap();
        assert_eq!(hits.len(), 5);
        for w in hits.windows(2) {
            assert!(w[0].date_unix_ns.unwrap() >= w[1].date_unix_ns.unwrap());
        }
        assert_eq!(hits[0].message_id, "<n9@x>");
    }

    #[test]
    fn scan_eq_since_filter() {
        let mut db = OverDb::open_in_memory().unwrap();
        let rows: Vec<_> = (0..10)
            .map(|i| sample_row(&format!("<s{i}@x>"), "lkml", 100 + i as i64, "u@x"))
            .collect();
        db.insert_batch(&rows).unwrap();

        let hits = db
            .scan_eq(EqField::List, "lkml", Some(105), None, 100)
            .unwrap();
        assert_eq!(hits.len(), 5);
        for h in &hits {
            assert!(h.date_unix_ns.unwrap() >= 105);
        }
    }

    fn sample_row_with_sob(
        mid: &str,
        list: &str,
        date: i64,
        from: &str,
        sob_emails: &[&str],
    ) -> OverRow {
        let mut row = sample_row(mid, list, date, from);
        row.ddd.signed_off_by = sob_emails
            .iter()
            .map(|e| format!("Someone <{e}>"))
            .collect();
        row
    }

    #[test]
    fn scan_eq_touched_files_joins_side_table() {
        let mut db = OverDb::open_in_memory().unwrap();
        let mut r1 = sample_row("<t1@x>", "netdev", 1_000, "a@x");
        r1.ddd.touched_files = vec![
            "drivers/net/foo.c".into(),
            "include/linux/foo.h".into(),
        ];
        let mut r2 = sample_row("<t2@x>", "linux-fs", 2_000, "b@x");
        r2.ddd.touched_files = vec!["drivers/net/foo.c".into()];
        let mut r3 = sample_row("<t3@x>", "netdev", 3_000, "c@x");
        r3.ddd.touched_files = vec!["drivers/net/bar.c".into()];
        db.insert_batch(&[r1, r2, r3]).unwrap();

        // Cross-list lookup by exact path; newest-first.
        let hits = db
            .scan_eq(EqField::TouchedFile, "drivers/net/foo.c", None, None, 10)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].message_id, "<t2@x>");
        assert_eq!(hits[1].message_id, "<t1@x>");

        // list_filter narrows.
        let hits = db
            .scan_eq(
                EqField::TouchedFile,
                "drivers/net/foo.c",
                None,
                Some("netdev"),
                10,
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_id, "<t1@x>");

        // Non-matching path returns empty without a scan.
        let hits = db
            .scan_eq(EqField::TouchedFile, "kernel/sched/core.c", None, None, 10)
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn backfill_touched_files_populates_side_table() {
        let mut db = OverDb::open_in_memory().unwrap();
        let mut r1 = sample_row("<b1@x>", "lkml", 1, "a@x");
        r1.ddd.touched_files = vec!["fs/x.c".into(), "fs/y.c".into()];
        db.insert_batch(&[r1]).unwrap();
        // Wipe the side table to simulate an older over.db.
        db.conn
            .execute("DELETE FROM over_touched_file", [])
            .unwrap();

        let n = db.backfill_touched_files().unwrap();
        assert_eq!(n, 2);
        let hits = db
            .scan_eq(EqField::TouchedFile, "fs/x.c", None, None, 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn insert_or_replace_prunes_stale_touched_file_rows() {
        let mut db = OverDb::open_in_memory().unwrap();
        let mut v1 = sample_row("<r@x>", "lkml", 1, "a@x");
        v1.ddd.touched_files = vec!["fs/old.c".into()];
        db.insert_batch(&[v1]).unwrap();
        let mut v2 = sample_row("<r@x>", "lkml", 2, "a@x");
        v2.ddd.touched_files = vec!["fs/new.c".into()];
        db.insert_batch(&[v2]).unwrap();

        let old_hits = db
            .scan_eq(EqField::TouchedFile, "fs/old.c", None, None, 10)
            .unwrap();
        assert!(old_hits.is_empty(), "stale touched_file entry survived");
        let new_hits = db
            .scan_eq(EqField::TouchedFile, "fs/new.c", None, None, 10)
            .unwrap();
        assert_eq!(new_hits.len(), 1);
    }

    #[test]
    fn scan_eq_signed_off_by_joins_trailer_side_table() {
        let mut db = OverDb::open_in_memory().unwrap();
        let rows = vec![
            sample_row_with_sob(
                "<s1@x>",
                "lkml",
                1_000,
                "author1@x",
                &["gregkh@linuxfoundation.org"],
            ),
            sample_row_with_sob(
                "<s2@x>",
                "lkml",
                2_000,
                "author2@x",
                &["gregkh@linuxfoundation.org", "akpm@linux-foundation.org"],
            ),
            sample_row_with_sob("<s3@x>", "lkml", 3_000, "a@x", &["other@x.com"]),
        ];
        db.insert_batch(&rows).unwrap();

        let hits = db
            .scan_eq(
                EqField::SignedOffBy,
                "gregkh@linuxfoundation.org",
                None,
                None,
                10,
            )
            .unwrap();
        assert_eq!(hits.len(), 2);
        // Newest-first.
        assert_eq!(hits[0].message_id, "<s2@x>");
        assert_eq!(hits[1].message_id, "<s1@x>");

        // Case-insensitive on input.
        let hits2 = db
            .scan_eq(
                EqField::SignedOffBy,
                "GREGKH@LinuxFoundation.Org",
                None,
                None,
                10,
            )
            .unwrap();
        assert_eq!(hits2.len(), 2);

        // list_filter narrows.
        let hits3 = db
            .scan_eq(
                EqField::SignedOffBy,
                "gregkh@linuxfoundation.org",
                None,
                Some("other-list"),
                10,
            )
            .unwrap();
        assert!(hits3.is_empty());
    }

    #[test]
    fn per_list_stats_reports_row_counts_and_date_windows() {
        let mut db = OverDb::open_in_memory().unwrap();
        let rows = vec![
            sample_row("<a1@x>", "lkml", 1_000, "a@x"),
            sample_row("<a2@x>", "lkml", 3_000, "b@x"),
            sample_row("<a3@x>", "lkml", 2_000, "a@x"),
            sample_row("<n1@x>", "netdev", 5_000, "c@x"),
            sample_row("<e1@x>", "empty", 0, "d@x"),
        ];
        // mutate 'empty' row to have no date so MIN/MAX pick up NULLs
        let mut rows = rows;
        rows[4].date_unix_ns = None;
        db.insert_batch(&rows).unwrap();

        let stats = db.per_list_stats().unwrap();
        assert_eq!(stats.len(), 3);
        // Alphabetical order.
        assert_eq!(stats[0].list, "empty");
        assert_eq!(stats[0].rows, 1);
        assert_eq!(stats[0].earliest_date_unix_ns, None);
        assert_eq!(stats[0].latest_date_unix_ns, None);

        assert_eq!(stats[1].list, "lkml");
        assert_eq!(stats[1].rows, 3);
        assert_eq!(stats[1].earliest_date_unix_ns, Some(1_000));
        assert_eq!(stats[1].latest_date_unix_ns, Some(3_000));

        assert_eq!(stats[2].list, "netdev");
        assert_eq!(stats[2].rows, 1);
    }

    #[test]
    fn trailer_email_side_table_covers_every_registered_kind() {
        let mut db = OverDb::open_in_memory().unwrap();
        let mut row = sample_row("<m@x>", "lkml", 1_000, "a@x");
        row.ddd.signed_off_by = vec!["SOB <sob@example.com>".into()];
        row.ddd.reviewed_by = vec!["Rev <rev@example.com>".into()];
        row.ddd.acked_by = vec!["Ack <ack@example.com>".into()];
        row.ddd.tested_by = vec!["Tst <tst@example.com>".into()];
        row.ddd.co_developed_by = vec!["Co <co@example.com>".into()];
        row.ddd.reported_by = vec!["Rep <rep@example.com>".into()];
        db.insert_batch(&[row]).unwrap();

        let cases = [
            (EqField::SignedOffBy, "sob@example.com"),
            (EqField::ReviewedBy, "rev@example.com"),
            (EqField::AckedBy, "ack@example.com"),
            (EqField::TestedBy, "tst@example.com"),
            (EqField::CoDevelopedBy, "co@example.com"),
            (EqField::ReportedBy, "rep@example.com"),
        ];
        for (field, email) in cases {
            let hits = db.scan_eq(field, email, None, None, 10).unwrap();
            assert_eq!(
                hits.len(),
                1,
                "{:?} lookup failed for {}",
                field,
                email
            );
            assert_eq!(hits[0].message_id, "<m@x>");
        }
    }

    #[test]
    fn insert_or_replace_prunes_stale_sob_side_rows() {
        let mut db = OverDb::open_in_memory().unwrap();
        let row_v1 = sample_row_with_sob("<r@x>", "lkml", 1, "a@x", &["old@x.com"]);
        db.insert_batch(&[row_v1]).unwrap();
        // Overwrite with a new ddd that drops old@, adds new@.
        let row_v2 = sample_row_with_sob("<r@x>", "lkml", 2, "a@x", &["new@x.com"]);
        db.insert_batch(&[row_v2]).unwrap();

        let by_old = db
            .scan_eq(EqField::SignedOffBy, "old@x.com", None, None, 10)
            .unwrap();
        assert!(by_old.is_empty(), "stale SOB entry survived REPLACE");
        let by_new = db
            .scan_eq(EqField::SignedOffBy, "new@x.com", None, None, 10)
            .unwrap();
        assert_eq!(by_new.len(), 1);
    }

    #[test]
    fn backfill_trailer_emails_fills_side_table_from_ddd() {
        let mut db = OverDb::open_in_memory().unwrap();
        let rows = vec![sample_row_with_sob(
            "<b@x>",
            "lkml",
            1,
            "a@x",
            &["gregkh@linuxfoundation.org"],
        )];
        db.insert_batch(&rows).unwrap();
        // Wipe the side table to simulate an over.db built before F2b.
        db.conn
            .execute("DELETE FROM over_trailer_email", [])
            .unwrap();

        let n = db.backfill_trailer_emails().unwrap();
        assert_eq!(n, 1);

        let hits = db
            .scan_eq(
                EqField::SignedOffBy,
                "gregkh@linuxfoundation.org",
                None,
                None,
                10,
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn backfill_subject_normalized_populates_column() {
        let mut db = OverDb::open_in_memory().unwrap();
        // Insert via raw SQL bypassing the write path, simulating legacy rows.
        let rows = vec![
            sample_row("<bf1@x>", "lkml", 1, "a@x"),
            sample_row("<bf2@x>", "lkml", 2, "a@x"),
        ];
        db.insert_batch(&rows).unwrap();
        // Clear the column on every row so backfill has work to do.
        db.conn
            .execute("UPDATE over SET subject_normalized = NULL", [])
            .unwrap();

        let updated = db.backfill_subject_normalized().unwrap();
        assert_eq!(updated, 2);

        // Indexed lookup now succeeds.
        let hits = db
            .scan_eq(EqField::SubjectNormalized, "subj for <bf2@x>", None, None, 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_id, "<bf2@x>");

        // Second run is a no-op (rowid cursor walked past everything).
        let updated_again = db.backfill_subject_normalized().unwrap();
        assert_eq!(updated_again, 0);
    }

    #[test]
    fn scan_eq_subject_normalized_indexed() {
        let mut db = OverDb::open_in_memory().unwrap();
        let rows = vec![
            sample_row("<s1@x>", "lkml", 1_000, "a@x"),
            sample_row("<s2@x>", "lkml", 2_000, "a@x"),
        ];
        db.insert_batch(&rows).unwrap();

        // sample_row sets subject_normalized = "subj for <mid>"
        let hits = db
            .scan_eq(EqField::SubjectNormalized, "subj for <s2@x>", None, None, 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_id, "<s2@x>");
    }

    #[test]
    fn legacy_schema_migrates_in_place_for_subject_normalized() {
        // Build a v0 over.db shape (no subject_normalized column),
        // then reopen through OverDb::open and verify the migration
        // ran without data loss.
        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE over (
                    rowid INTEGER PRIMARY KEY AUTOINCREMENT,
                    message_id TEXT NOT NULL,
                    list TEXT NOT NULL,
                    from_addr TEXT,
                    date_unix_ns INTEGER,
                    in_reply_to TEXT,
                    tid TEXT,
                    body_segment_id INTEGER NOT NULL,
                    body_offset INTEGER NOT NULL,
                    body_length INTEGER NOT NULL,
                    body_sha256 TEXT NOT NULL,
                    has_patch INTEGER NOT NULL DEFAULT 0,
                    is_cover_letter INTEGER NOT NULL DEFAULT 0,
                    series_version INTEGER,
                    series_index INTEGER,
                    series_total INTEGER,
                    files_changed INTEGER,
                    insertions INTEGER,
                    deletions INTEGER,
                    commit_oid TEXT,
                    ddd BLOB NOT NULL
                );
                CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                INSERT INTO meta VALUES ('schema_version', '1');
                INSERT INTO over (message_id, list, body_segment_id, body_offset,
                    body_length, body_sha256, ddd)
                VALUES ('<legacy@x>', 'lkml', 1, 0, 100, 'sha-legacy',
                    x'');",
            )
            .unwrap();
        }
        // Reopen through the full OverDb migration path.
        let db = OverDb::open(&path).unwrap();
        // Column must now be present.
        let exists = column_exists(&db.conn, "over", "subject_normalized").unwrap();
        assert!(exists, "migration should have added subject_normalized");
        // Existing row survived and has NULL subject_normalized.
        let v: Option<String> = db
            .conn
            .query_row(
                "SELECT subject_normalized FROM over WHERE message_id = ?1",
                ["<legacy@x>"],
                |r| r.get(0),
            )
            .unwrap();
        assert!(v.is_none());
    }

    #[test]
    fn scan_eq_body_sha256_and_commit_oid_indexed() {
        let mut db = OverDb::open_in_memory().unwrap();
        let rows = vec![
            sample_row("<b1@x>", "lkml", 1_000, "a@x"),
            sample_row("<b2@x>", "lkml", 2_000, "a@x"),
        ];
        db.insert_batch(&rows).unwrap();

        let by_sha = db
            .scan_eq(EqField::BodySha256, "sha-<b2@x>", None, None, 10)
            .unwrap();
        assert_eq!(by_sha.len(), 1);
        assert_eq!(by_sha[0].message_id, "<b2@x>");

        let by_oid = db
            .scan_eq(EqField::CommitOid, "oid-<b1@x>", None, None, 10)
            .unwrap();
        assert_eq!(by_oid.len(), 1);
        assert_eq!(by_oid[0].message_id, "<b1@x>");

        // Non-existent value returns empty, not a scan.
        let miss = db
            .scan_eq(EqField::CommitOid, "deadbeef", None, None, 10)
            .unwrap();
        assert!(miss.is_empty());
    }

    #[test]
    fn cross_post_get_returns_freshest() {
        let mut db = OverDb::open_in_memory().unwrap();
        let r1 = sample_row("<a@b>", "lkml", 1_000, "u@x");
        let r2 = sample_row("<a@b>", "netdev", 5_000, "u@x");
        db.insert_batch(&[r1, r2]).unwrap();
        let got = db.get("<a@b>").unwrap().expect("row missing");
        assert_eq!(got.list, "netdev");
        assert_eq!(got.date_unix_ns, Some(5_000));

        // get_many on the same id should yield exactly one entry —
        // the freshest.
        let got_m = db.get_many(&["<a@b>".to_string()]).unwrap();
        assert_eq!(got_m.len(), 1);
        assert_eq!(got_m["<a@b>"].list, "netdev");
    }

    #[test]
    fn insert_or_replace_idempotent_on_mid_list() {
        let mut db = OverDb::open_in_memory().unwrap();
        let mut row = sample_row("<dup@x>", "lkml", 1_000, "u@x");
        db.insert_batch(std::slice::from_ref(&row)).unwrap();

        // Re-insert with mutated metadata; should overwrite, not duplicate.
        row.date_unix_ns = Some(9_999);
        row.ddd.subject_raw = Some("updated".to_string());
        db.insert_batch(&[row]).unwrap();

        let got = db.get("<dup@x>").unwrap().unwrap();
        assert_eq!(got.date_unix_ns, Some(9_999));
        assert_eq!(got.subject_raw.as_deref(), Some("updated"));

        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM over WHERE message_id='<dup@x>'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn ddd_round_trip_preserves_lists() {
        let payload = DddPayload {
            subject_raw: Some("hello".into()),
            subject_tags: vec!["RFC".into(), "PATCH".into()],
            touched_files: vec!["a.c".into(), "b.c".into()],
            signed_off_by: vec!["X <x@y>".into()],
            ..Default::default()
        };
        let blob = encode_ddd(&payload).unwrap();
        let back = decode_ddd(&blob).unwrap();
        assert_eq!(payload, back);
    }
}
