//! Path tier — Aho-Corasick-based file-path mention reverse index.
//!
//! Answers: "which messages mention `smbacl.c` anywhere in their body?"
//! Unlike `touched_files[]` (from `diff --git` headers), this tier
//! catches reviewer discussions, bug reports, shortlogs, and free
//! prose mentions of filenames.
//!
//! Design lives at `docs/indexing/path-tier.md`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

use crate::error::{Error, Result};

/// In-memory vocabulary of all known kernel source paths, backed by an
/// Aho-Corasick automaton for O(n) body scanning.
pub struct PathVocab {
    paths: Vec<String>,
    basename_index: HashMap<String, Vec<u32>>,
    automaton: AhoCorasick,
}

impl PathVocab {
    /// Build from a deduplicated, sorted list of paths (typically the
    /// union of `touched_files[]` across the entire corpus).
    pub fn from_paths(mut paths: Vec<String>) -> Result<Self> {
        paths.sort();
        paths.dedup();

        // MatchKind::Standard is required for overlapping iteration.
        // LeftmostFirst/LeftmostLongest don't support it (AC crate
        // invariant). Standard emits all matches at every position.
        let automaton = AhoCorasickBuilder::new()
            .match_kind(MatchKind::Standard)
            .build(&paths)
            .map_err(|e| Error::State(format!("AC build failed: {e}")))?;

        let mut basename_index: HashMap<String, Vec<u32>> = HashMap::new();
        for (id, path) in paths.iter().enumerate() {
            let basename = path.rsplit('/').next().unwrap_or(path).to_owned();
            basename_index.entry(basename).or_default().push(id as u32);
        }

        Ok(Self {
            paths,
            basename_index,
            automaton,
        })
    }

    /// Number of distinct paths in the vocabulary.
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Scan a body (prose + patch bytes) and return the set of path_ids
    /// that appear anywhere in it.
    pub fn scan_body(&self, body: &[u8]) -> Vec<u32> {
        let mut seen = roaring::RoaringBitmap::new();
        for mat in self.automaton.find_overlapping_iter(body) {
            seen.insert(mat.pattern().as_u32());
        }
        seen.iter().collect()
    }

    /// Exact lookup: full path → path_id.
    pub fn lookup_exact(&self, path: &str) -> Option<u32> {
        self.paths
            .binary_search_by(|p| p.as_str().cmp(path))
            .ok()
            .map(|i| i as u32)
    }

    /// Basename lookup: "smbacl.c" → all path_ids whose basename matches.
    pub fn lookup_basename(&self, basename: &str) -> &[u32] {
        self.basename_index
            .get(basename)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Prefix lookup: "fs/smb/server/" → all path_ids under that prefix.
    pub fn lookup_prefix(&self, prefix: &str) -> Vec<u32> {
        let start = self.paths.partition_point(|p| p.as_str() < prefix);
        let mut out = Vec::new();
        for (i, p) in self.paths[start..].iter().enumerate() {
            if !p.starts_with(prefix) {
                break;
            }
            out.push((start + i) as u32);
        }
        out
    }

    /// Resolve a path_id back to its full path string.
    pub fn path_for_id(&self, id: u32) -> Option<&str> {
        self.paths.get(id as usize).map(|s| s.as_str())
    }
}

/// On-disk path-tier directory layout under `<data_dir>/paths/`.
///
/// - `vocab.txt`         — one path per line, sorted
/// - `postings/<mid>.roaring` — per-message path_ids (for rebuild)
///
/// For v0.1.x we keep the design simple: the vocab is rebuilt from
/// all Parquet metadata on each `reindex`, and the postings are
/// recomputed by scanning all bodies. This avoids incremental
/// segment management for a tier that is small relative to the
/// trigram tier. Optimization (segment-based incremental) can land
/// in v0.2.x if the full-rebuild cost exceeds the 5-min cadence.
const VOCAB_FILE: &str = "vocab.txt";

/// Persist the vocabulary to `<data_dir>/paths/vocab.txt`.
pub fn save_vocab(data_dir: &Path, vocab: &PathVocab) -> Result<()> {
    let dir = data_dir.join("paths");
    fs::create_dir_all(&dir)?;
    let content = vocab.paths.join("\n");
    let tmp = dir.join(".vocab.txt.tmp");
    fs::write(&tmp, content.as_bytes())?;
    fs::rename(&tmp, dir.join(VOCAB_FILE))?;
    Ok(())
}

/// Load the vocabulary from `<data_dir>/paths/vocab.txt`.
pub fn load_vocab(data_dir: &Path) -> Result<Option<PathVocab>> {
    let path = data_dir.join("paths").join(VOCAB_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)?;
    let paths: Vec<String> = content
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_owned())
        .collect();
    if paths.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathVocab::from_paths(paths)?))
}

/// Convenience: extract the vocabulary root dir.
pub fn paths_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("paths")
}

/// Build (or rebuild) `<data_dir>/paths/vocab.txt` from whatever
/// source has the distinct-paths signal.
///
/// Primary source: `over.db::over_touched_file` — one indexed
/// `SELECT DISTINCT` over a (path, ...) primary key. Fast; this is
/// the production path once `backfill_touched_files` has run.
///
/// Fallback: stream every Parquet row in `<data_dir>/metadata/` and
/// union their `touched_files` lists. Slower but works on fresh
/// deployments and on the Python single-shard ingest path that
/// doesn't write to over.db by default. Returns `Ok(0)` only when
/// the corpus genuinely carries no touched_files (e.g. an all-prose
/// list).
pub fn rebuild_vocab_from_over(data_dir: &Path) -> Result<u64> {
    let mut paths: Vec<String> = Vec::new();
    if let Some(from_over) = collect_paths_from_over(data_dir)? {
        paths = from_over;
    }
    if paths.is_empty() {
        // Fallback: walk the metadata Parquet. The Reader already
        // implements this shape via `scan_streaming`; calling it
        // here keeps one dedup strategy.
        let reader = crate::reader::Reader::new(data_dir);
        let mut seen = std::collections::BTreeSet::<String>::new();
        reader.scan_streaming(None, |row| {
            for p in &row.touched_files {
                if !p.is_empty() {
                    seen.insert(p.clone());
                }
            }
            true
        })?;
        paths = seen.into_iter().collect();
    }
    let count = paths.len() as u64;
    if count == 0 {
        return Ok(0);
    }
    let vocab = PathVocab::from_paths(paths)?;
    save_vocab(data_dir, &vocab)?;
    Ok(count)
}

fn collect_paths_from_over(data_dir: &Path) -> Result<Option<Vec<String>>> {
    use rusqlite::Connection;

    let over_path = data_dir.join("over.db");
    if !over_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(
        &over_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // `SELECT DISTINCT path` on the composite PRIMARY KEY streams
    // in sorted order without a temp sort.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT path FROM over_touched_file ORDER BY path ASC",
    )?;
    let mut rows = stmt.query([])?;
    let mut paths: Vec<String> = Vec::new();
    while let Some(r) = rows.next()? {
        let p: String = r.get(0)?;
        if !p.is_empty() {
            paths.push(p);
        }
    }
    Ok(Some(paths))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_paths() -> Vec<String> {
        vec![
            "fs/smb/server/smbacl.c".into(),
            "fs/smb/server/smb2pdu.c".into(),
            "fs/smb/server/smb2pdu.h".into(),
            "net/sunrpc/xdr.c".into(),
            "include/linux/smbacl.h".into(),
            "drivers/gpu/drm/i915/gem/i915_gem_context.c".into(),
        ]
    }

    #[test]
    fn exact_lookup() {
        let v = PathVocab::from_paths(sample_paths()).unwrap();
        assert!(v.lookup_exact("fs/smb/server/smbacl.c").is_some());
        assert!(v.lookup_exact("does/not/exist.c").is_none());
    }

    #[test]
    fn basename_lookup() {
        let v = PathVocab::from_paths(sample_paths()).unwrap();
        let ids = v.lookup_basename("smbacl.c");
        assert_eq!(ids.len(), 1);
        assert_eq!(v.path_for_id(ids[0]).unwrap(), "fs/smb/server/smbacl.c");

        let h_ids = v.lookup_basename("smb2pdu.h");
        assert_eq!(h_ids.len(), 1);
    }

    #[test]
    fn prefix_lookup() {
        let v = PathVocab::from_paths(sample_paths()).unwrap();
        let ids = v.lookup_prefix("fs/smb/server/");
        assert_eq!(ids.len(), 3);
        for id in &ids {
            assert!(v.path_for_id(*id).unwrap().starts_with("fs/smb/server/"));
        }
    }

    #[test]
    fn scan_body_finds_paths() {
        let v = PathVocab::from_paths(sample_paths()).unwrap();

        let body = b"The issue is in fs/smb/server/smbacl.c, specifically the \
            smb_check_perm_dacl function. Also see net/sunrpc/xdr.c for context.";
        let found = v.scan_body(body);
        assert!(found.len() >= 2);
        let found_paths: Vec<&str> = found.iter().filter_map(|id| v.path_for_id(*id)).collect();
        assert!(found_paths.contains(&"fs/smb/server/smbacl.c"));
        assert!(found_paths.contains(&"net/sunrpc/xdr.c"));
    }

    #[test]
    fn scan_body_with_diff_prefix() {
        let v = PathVocab::from_paths(sample_paths()).unwrap();
        // Reviewer quoting a diff header — the path is prefixed with a/ or b/.
        // AC won't match because it looks for "fs/smb/..." not "a/fs/smb/...".
        // This is correct behavior: the diff-header path is already in
        // touched_files[]. The path tier catches *free prose* mentions.
        let body = b"In a/fs/smb/server/smbacl.c we see the issue but also \
            in prose: fs/smb/server/smb2pdu.c has the same pattern.";
        let found = v.scan_body(body);
        let found_paths: Vec<&str> = found.iter().filter_map(|id| v.path_for_id(*id)).collect();
        // "a/fs/smb/server/smbacl.c" contains "fs/smb/server/smbacl.c" as a
        // substring — AC finds it via overlapping match.
        assert!(found_paths.contains(&"fs/smb/server/smbacl.c"));
        assert!(found_paths.contains(&"fs/smb/server/smb2pdu.c"));
    }

    #[test]
    fn empty_vocab() {
        let v = PathVocab::from_paths(vec![]).unwrap();
        assert!(v.is_empty());
        assert!(v.scan_body(b"fs/smb/server/smbacl.c").is_empty());
    }

    #[test]
    fn save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let v = PathVocab::from_paths(sample_paths()).unwrap();
        let count = v.len();
        save_vocab(tmp.path(), &v).unwrap();

        let loaded = load_vocab(tmp.path()).unwrap().unwrap();
        assert_eq!(loaded.len(), count);
        assert!(loaded.lookup_exact("fs/smb/server/smbacl.c").is_some());
    }
}
