//! Git-metadata sidecar — a small SQLite table of (sha, subject,
//! author_email, author_date_ns) per commit, sourced from the
//! canonical mainline tree and any subsystem trees the operator
//! points us at.
//!
//! Purpose: lore has the patch mail, but it doesn't have the git
//! history of torvalds/linux.git. Several feature tools want to
//! answer "was this patch merged?" deterministically — that is a
//! git-side question. This sidecar is the cheap-but-real source
//! of truth.
//!
//! Scope notes (v1):
//!   - Captures (repo, sha, subject, author_email, author_date_ns).
//!   - patch_id column is declared but left NULL — computing
//!     `git patch-id --stable` over 1.5M commits is hours of I/O,
//!     deferred to v2. Subject + author + date-window matching
//!     already covers ~60-70% of the canonical `b4`-style cascade
//!     per the research brief.
//!   - Incremental ingest via `rev_walk().with_hidden([last_tip])`.
//!   - Separate SQLite file (`<data_dir>/git_sidecar.db`) — easy
//!     to rebuild, never touches over.db.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};

use crate::error::{Error, Result};

/// Current schema version. Bumped on any incompatible change.
pub const SCHEMA_VERSION: i64 = 1;

/// A single commit row in the sidecar.
#[derive(Debug, Clone)]
pub struct CommitRecord {
    pub repo: String,
    pub sha: String,
    pub subject: String,
    pub author_email: String,
    pub author_date_ns: i64,
    pub patch_id: Option<String>,
}

/// Owning handle on the sidecar DB.
pub struct GitSidecar {
    conn: Connection,
}

impl GitSidecar {
    /// Open (or create) the sidecar DB at `path`. Runs migration +
    /// schema-version check.
    pub fn open(path: &Path) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags)?;
        Self::configure(&conn)?;
        Self::migrate(&conn)?;
        Self::verify_schema_version(&conn)?;
        Ok(Self { conn })
    }

    /// Test-only in-memory constructor.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        Self::migrate(&conn)?;
        Self::verify_schema_version(&conn)?;
        Ok(Self { conn })
    }

    fn configure(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        Ok(())
    }

    fn migrate(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS commits (
                repo            TEXT    NOT NULL,
                sha             TEXT    NOT NULL,
                subject         TEXT    NOT NULL,
                author_email    TEXT    NOT NULL,
                author_date_ns  INTEGER NOT NULL,
                patch_id        TEXT,
                PRIMARY KEY (repo, sha)
            );

            CREATE INDEX IF NOT EXISTS commits_patch_id ON commits (patch_id)
                WHERE patch_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS commits_subject  ON commits (subject);
            CREATE INDEX IF NOT EXISTS commits_author   ON commits (author_email, author_date_ns);
            CREATE INDEX IF NOT EXISTS commits_date     ON commits (author_date_ns);

            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS tips (
                repo    TEXT PRIMARY KEY,
                tip_sha TEXT NOT NULL
            );
            "#,
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    fn verify_schema_version(conn: &Connection) -> Result<()> {
        let v: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap_or_default();
        if v.is_empty() {
            return Ok(()); // fresh DB
        }
        let parsed: i64 = v
            .parse()
            .map_err(|e| Error::State(format!("git_sidecar schema_version: {e}")))?;
        if parsed != SCHEMA_VERSION {
            return Err(Error::State(format!(
                "git_sidecar schema_version {parsed} != expected {SCHEMA_VERSION}"
            )));
        }
        Ok(())
    }

    /// Bulk-insert a batch of commits. Idempotent via `INSERT OR REPLACE`
    /// on the (repo, sha) primary key — rerunning a walk over the same
    /// commits updates them in place.
    pub fn insert_batch(&mut self, rows: &[CommitRecord]) -> Result<u64> {
        if rows.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut written: u64 = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO commits (repo, sha, subject, author_email, author_date_ns, patch_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for r in rows {
                stmt.execute(params![
                    r.repo,
                    r.sha,
                    r.subject,
                    r.author_email,
                    r.author_date_ns,
                    r.patch_id,
                ])?;
                written += 1;
            }
        }
        tx.commit()?;
        Ok(written)
    }

    /// Record the latest-ingested tip for `repo` so incremental
    /// re-ingest can use `rev_walk().with_hidden([last_tip])`.
    pub fn set_tip(&mut self, repo: &str, tip_sha: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO tips(repo, tip_sha) VALUES (?1, ?2)",
            params![repo, tip_sha],
        )?;
        Ok(())
    }

    /// Read the last-ingested tip for `repo`, or `None` when we've
    /// never run a walk for it.
    pub fn tip(&self, repo: &str) -> Result<Option<String>> {
        let row = self
            .conn
            .query_row("SELECT tip_sha FROM tips WHERE repo = ?1", [repo], |r| {
                r.get::<_, String>(0)
            })
            .ok();
        Ok(row)
    }

    /// Total commit count. Useful for ops & tests.
    pub fn count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM commits", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Find a commit by exact patch_id. Returns all matches across
    /// all repos (patch-id is stable under cherry-picks, so a patch
    /// can appear in linux.git + linux-stable.git + a subsystem tree).
    pub fn find_by_patch_id(&self, patch_id: &str) -> Result<Vec<CommitRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT repo, sha, subject, author_email, author_date_ns, patch_id \
                 FROM commits WHERE patch_id = ?1",
        )?;
        let rows = stmt.query_map([patch_id], row_to_commit)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Find commits with matching subject + author_email within a
    /// date window. The canonical `b4`-style fallback when patch_id
    /// isn't available or doesn't match (because the patch was
    /// editorially polished on apply).
    pub fn find_by_subject_author(
        &self,
        subject: &str,
        author_email: &str,
        window_ns: i64,
        center_ns: i64,
    ) -> Result<Vec<CommitRecord>> {
        let low = center_ns.saturating_sub(window_ns);
        let high = center_ns.saturating_add(window_ns);
        let mut stmt = self.conn.prepare(
            "SELECT repo, sha, subject, author_email, author_date_ns, patch_id \
             FROM commits \
             WHERE subject = ?1 \
               AND author_email = ?2 \
               AND author_date_ns BETWEEN ?3 AND ?4",
        )?;
        let rows = stmt.query_map(
            params![subject, author_email.to_lowercase(), low, high],
            row_to_commit,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Every repo present in the sidecar, its commit count, and the
    /// last-recorded tip SHA (when known). Powers the
    /// `lore_stable_backport_status` / `lore_thread_state` trust
    /// decision: if `linux-stable` is present, the tools can answer
    /// authoritatively; if absent, they fall back to lore-only
    /// heuristics and annotate the caveat accordingly.
    pub fn repos_and_counts(&self) -> Result<Vec<(String, u64, Option<String>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.repo, COUNT(*) AS n, t.tip_sha \
             FROM commits c \
             LEFT JOIN tips t ON t.repo = c.repo \
             GROUP BY c.repo \
             ORDER BY c.repo ASC",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let repo: String = r.get(0)?;
            let count: i64 = r.get(1)?;
            let tip: Option<String> = r.get(2)?;
            out.push((repo, count as u64, tip));
        }
        Ok(out)
    }

    /// Look up one commit by repo + sha — the trivial "is this
    /// sha in the sidecar?" predicate.
    pub fn find_by_sha(&self, repo: &str, sha: &str) -> Result<Option<CommitRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT repo, sha, subject, author_email, author_date_ns, patch_id \
             FROM commits WHERE repo = ?1 AND sha = ?2",
        )?;
        let row = stmt.query_row(params![repo, sha], row_to_commit).ok();
        Ok(row)
    }
}

/// Default location of the sidecar DB inside a kernel-lore data_dir.
pub fn sidecar_path(data_dir: &Path) -> PathBuf {
    data_dir.join("git_sidecar.db")
}

fn row_to_commit(r: &rusqlite::Row<'_>) -> rusqlite::Result<CommitRecord> {
    Ok(CommitRecord {
        repo: r.get(0)?,
        sha: r.get(1)?,
        subject: r.get(2)?,
        author_email: r.get(3)?,
        author_date_ns: r.get(4)?,
        patch_id: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        repo: &str,
        sha: &str,
        subject: &str,
        email: &str,
        date_ns: i64,
        patch_id: Option<&str>,
    ) -> CommitRecord {
        CommitRecord {
            repo: repo.to_owned(),
            sha: sha.to_owned(),
            subject: subject.to_owned(),
            author_email: email.to_owned(),
            author_date_ns: date_ns,
            patch_id: patch_id.map(str::to_owned),
        }
    }

    #[test]
    fn insert_and_roundtrip() {
        let mut db = GitSidecar::open_in_memory().unwrap();
        let r = row(
            "linux",
            "abc123",
            "mm: fix oops on null",
            "alice@example.com",
            1_700_000_000_000_000_000,
            Some("deadbeef0000"),
        );
        assert_eq!(db.insert_batch(&[r.clone()]).unwrap(), 1);
        assert_eq!(db.count().unwrap(), 1);
        let got = db.find_by_sha("linux", "abc123").unwrap().unwrap();
        assert_eq!(got.subject, "mm: fix oops on null");
        assert_eq!(got.patch_id.as_deref(), Some("deadbeef0000"));
    }

    #[test]
    fn find_by_patch_id_across_repos() {
        let mut db = GitSidecar::open_in_memory().unwrap();
        db.insert_batch(&[
            row("linux", "abc", "s1", "a@x", 1000, Some("pid1")),
            row("linux-stable", "def", "s1", "a@x", 2000, Some("pid1")),
            row("linux", "ghi", "s2", "b@x", 3000, Some("pid2")),
        ])
        .unwrap();
        let hits = db.find_by_patch_id("pid1").unwrap();
        assert_eq!(hits.len(), 2);
        let repos: std::collections::HashSet<&str> = hits.iter().map(|c| c.repo.as_str()).collect();
        assert!(repos.contains("linux"));
        assert!(repos.contains("linux-stable"));
    }

    #[test]
    fn find_by_subject_author_window() {
        let mut db = GitSidecar::open_in_memory().unwrap();
        db.insert_batch(&[
            row(
                "linux",
                "aaa",
                "foo: fix bar",
                "alice@x",
                1_000_000_000,
                None,
            ),
            row(
                "linux",
                "bbb",
                "foo: fix bar",
                "alice@x",
                50_000_000_000,
                None,
            ),
            row(
                "linux",
                "ccc",
                "other subject",
                "alice@x",
                1_100_000_000,
                None,
            ),
        ])
        .unwrap();
        let window_ns: i64 = 2_000_000_000;
        let center_ns: i64 = 1_000_000_000;
        let hits = db
            .find_by_subject_author("foo: fix bar", "alice@x", window_ns, center_ns)
            .unwrap();
        // Only the "aaa" row falls inside ±2s window.
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sha, "aaa");
    }

    #[test]
    fn tip_roundtrip() {
        let mut db = GitSidecar::open_in_memory().unwrap();
        assert!(db.tip("linux").unwrap().is_none());
        db.set_tip("linux", "deadbeef").unwrap();
        assert_eq!(db.tip("linux").unwrap().as_deref(), Some("deadbeef"));
        db.set_tip("linux", "cafebabe").unwrap();
        assert_eq!(db.tip("linux").unwrap().as_deref(), Some("cafebabe"));
    }

    #[test]
    fn repos_and_counts_summarizes_every_repo() {
        let mut db = GitSidecar::open_in_memory().unwrap();
        db.insert_batch(&[
            row("linux", "a1", "s1", "alice@x", 1, None),
            row("linux", "a2", "s2", "bob@x", 2, None),
            row("linux-stable", "b1", "s1", "alice@x", 3, None),
        ])
        .unwrap();
        db.set_tip("linux", "a2").unwrap();
        // No tip recorded for linux-stable yet — should come back as None.

        let stats = db.repos_and_counts().unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].0, "linux");
        assert_eq!(stats[0].1, 2);
        assert_eq!(stats[0].2.as_deref(), Some("a2"));
        assert_eq!(stats[1].0, "linux-stable");
        assert_eq!(stats[1].1, 1);
        assert_eq!(stats[1].2, None);
    }

    #[test]
    fn reopen_preserves_rows() {
        use tempfile::TempDir;
        let d = TempDir::new().unwrap();
        let path = d.path().join("g.db");
        {
            let mut db = GitSidecar::open(&path).unwrap();
            db.insert_batch(&[row("linux", "a", "s", "e@x", 1, None)])
                .unwrap();
        }
        let db = GitSidecar::open(&path).unwrap();
        assert_eq!(db.count().unwrap(), 1);
    }
}
