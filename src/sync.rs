//! `kernel-lore-sync` core: manifest fetch + diff + gix fetch.
//!
//! Replaces the external `grokmirror` Python dependency with a single
//! in-process pipeline:
//!
//! 1. HTTP GET `<manifest_url>` (gzip-decoded JSON).
//! 2. Compare each shard's `fingerprint` against the local cache.
//! 3. For each changed shard: `gix` smart-HTTP fetch; clone if absent.
//! 4. Save the fresh fingerprint cache atomically on full success.
//!
//! This module owns only the *mirror* side: fetching / updating the
//! on-disk shard tree under `<data_dir>/shards/<list>/git/<N>.git`.
//! Ingest orchestration (writer lock, rayon fan-out over shards,
//! BM25 + over.db + tid, generation bump) lives in
//! `src/bin/sync.rs` which composes this module with the existing
//! `crate::ingest::ingest_shard_with_bm25`.
//!
//! Why not just shell out to `git fetch`?
//!
//! * One less language (no Python grokmirror, no `subprocess` call).
//! * One less crash surface (no external binary whose error codes we'd
//!   have to parse).
//! * The writer lock that serializes ingest also covers the fetch,
//!   closing the trigger-file race the old pipeline had between
//!   "shards landed" and "ingest started".
//!
//! Design choices:
//!
//! * `ureq` for the manifest HTTP GET (tiny, sync, rustls). gix already
//!   pulls rustls via its blocking-http transport feature, so we don't
//!   add a second TLS implementation.
//! * We recognize the two lore manifest shapes in the wild: public-
//!   inbox v2 (`/<list>/<N>.git` under the root) and v1 (`/<list>.git`
//!   at the root). `shard_url` + `shard_local_path` normalize both.
//! * fnmatch-style include / exclude filters, matching grokmirror's
//!   UX. Implemented with `glob::Pattern`-esque logic inline — a real
//!   glob dep is overkill for the shell-glob subset we need.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use git2::Repository as Git2Repository;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Default upstream. Override with `--manifest-url` or
/// `KLMCP_MANIFEST_URL` for a different public-inbox instance.
pub const DEFAULT_MANIFEST_URL: &str = "https://lore.kernel.org/manifest.js.gz";

/// Per-shard entry in the lore manifest. We only care about
/// `fingerprint` for diffing; `modified` is recorded for ops metrics.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ShardMeta {
    pub fingerprint: String,
    #[serde(default)]
    pub modified: Option<i64>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
}

/// Full manifest. Keyed by shard path ("/<list>/git/<N>.git" or
/// "/<list>.git"), value is the shard's upstream fingerprint.
///
/// BTreeMap keeps output deterministic — the save/restore round trip
/// then produces byte-identical files when nothing changed, which
/// makes ops diffs clean.
pub type Manifest = BTreeMap<String, ShardMeta>;

/// One pass of the sync pipeline's summary stats.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncSummary {
    pub shards_total: usize,
    pub shards_changed: usize,
    pub shards_fetched: usize,
    pub shards_failed: usize,
}

/// Outcome of fetching one shard.
#[derive(Debug)]
pub enum FetchOutcome {
    /// New clone — the shard was absent locally; we created it.
    Cloned,
    /// Existing local repo was unusable (missing/open-broken/no refs),
    /// so we discarded it and recloned from upstream.
    Recloned,
    /// Existing bare repo fetched incrementally; the new head may
    /// match the old one if the upstream fingerprint changed in a
    /// ref we don't track (manifest fingerprints cover all refs).
    Fetched,
    /// Nothing to do — the caller is expected to skip fetching when
    /// the fingerprint already matches; we keep the variant so
    /// integration tests that re-run sync observe steady state.
    UpToDate,
}

/// HTTP timeout for the manifest GET. Tiny blob (~2 MB compressed),
/// but give networks some slack without blocking the systemd timer
/// indefinitely.
const MANIFEST_TIMEOUT_SECS: u64 = 60;

/// Fetch `<manifest_url>` and gunzip-decode into a [`Manifest`].
///
/// Returns `Error::Sync` with a user-readable reason on HTTP failure,
/// bad gzip, or bad JSON. The caller decides whether to retry.
pub fn fetch_manifest(manifest_url: &str) -> Result<Manifest> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(MANIFEST_TIMEOUT_SECS)))
        .user_agent(user_agent())
        .build()
        .new_agent();

    let resp = agent
        .get(manifest_url)
        .call()
        .map_err(|e| Error::Sync(format!("GET {manifest_url}: {e}")))?;

    let (_parts, body) = resp.into_parts();
    let reader = body.into_reader();
    let mut decoder = flate2::read::GzDecoder::new(reader);
    let mut buf = Vec::with_capacity(2 * 1024 * 1024);
    decoder
        .read_to_end(&mut buf)
        .map_err(|e| Error::Sync(format!("gunzip manifest: {e}")))?;

    serde_json::from_slice::<Manifest>(&buf)
        .map_err(|e| Error::Sync(format!("parse manifest JSON: {e}")))
}

fn user_agent() -> String {
    format!(
        "kernel-lore-mcp/{} (+https://github.com/mjbommar/kernel-lore-mcp)",
        env!("CARGO_PKG_VERSION")
    )
}

/// On-disk location of the saved manifest cache. Keeping the whole
/// manifest (not just fingerprints) lets ops debug against a
/// historical snapshot without a re-fetch, and the extra fields are
/// cheap.
fn fingerprint_cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("state").join("manifest.json")
}

/// Load the last-fetched manifest from disk. Missing file = empty
/// map (fresh deployment; every shard will look "changed").
pub fn load_local_manifest(data_dir: &Path) -> Result<Manifest> {
    let path = fingerprint_cache_path(data_dir);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::Sync(format!("read {}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::new()),
        Err(e) => Err(Error::Sync(format!("read {}: {e}", path.display()))),
    }
}

/// Persist `manifest` atomically under `<data_dir>/state/manifest.json`.
/// Tempfile + rename so an interrupted write never leaves a truncated
/// cache that future diffs would mistake for "nothing upstream".
pub fn save_local_manifest(data_dir: &Path, manifest: &Manifest) -> Result<()> {
    let path = fingerprint_cache_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Sync(format!("mkdir {}: {e}", parent.display())))?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|e| Error::Sync(format!("encode manifest: {e}")))?;
    std::fs::write(&tmp, &bytes)
        .map_err(|e| Error::Sync(format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        Error::Sync(format!(
            "rename {} -> {}: {e}",
            tmp.display(),
            path.display()
        ))
    })?;
    Ok(())
}

/// Apply shell-glob include/exclude filters to a manifest path.
///
/// Matches grokmirror's behaviour: an empty `include` means "include
/// everything"; any exclude hit wins over any include hit. Patterns
/// use the `*` / `?` / `[...]` subset supported by `fnmatch(3)`.
pub fn path_matches(path: &str, include: &[String], exclude: &[String]) -> bool {
    let included = if include.is_empty() {
        true
    } else {
        include.iter().any(|p| fnmatch(p, path))
    };
    if !included {
        return false;
    }
    !exclude.iter().any(|p| fnmatch(p, path))
}

/// Minimal fnmatch: `*` (zero-or-more chars), `?` (one char). Does
/// NOT implement `[...]` character classes — the lore manifest keys
/// are ASCII path-like strings and the subset that actually appears
/// in production configs is `*` and literal segments. Keeps the dep
/// surface at zero.
fn fnmatch(pat: &str, s: &str) -> bool {
    fn go(pat: &[u8], s: &[u8]) -> bool {
        match (pat.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => {
                // Match zero chars here, or one char in `s` + re-try.
                if go(&pat[1..], s) {
                    return true;
                }
                if s.is_empty() {
                    return false;
                }
                go(pat, &s[1..])
            }
            (Some(b'?'), Some(_)) => go(&pat[1..], &s[1..]),
            (Some(pc), Some(sc)) if pc == sc => go(&pat[1..], &s[1..]),
            _ => false,
        }
    }
    go(pat.as_bytes(), s.as_bytes())
}

/// Return the subset of remote manifest entries whose fingerprint
/// differs from (or is absent in) the local cache, filtered by
/// include / exclude. Deterministic order: sorted manifest keys.
pub fn diff_manifest(
    remote: &Manifest,
    local: &Manifest,
    include: &[String],
    exclude: &[String],
) -> Vec<String> {
    let mut changed: Vec<String> = Vec::new();
    for (path, meta) in remote {
        if !path_matches(path, include, exclude) {
            continue;
        }
        let same = local
            .get(path)
            .map(|prev| prev.fingerprint == meta.fingerprint)
            .unwrap_or(false);
        if !same {
            changed.push(path.clone());
        }
    }
    changed
}

/// Translate a manifest path (e.g. `/netdev/git/0.git`) to a full
/// upstream URL given the manifest URL. We strip `manifest.js.gz`
/// off the manifest URL to get the base, then append the shard path.
pub fn shard_url(manifest_url: &str, shard_path: &str) -> String {
    let base = manifest_url
        .rsplit_once('/')
        .map(|(prefix, _)| prefix)
        .unwrap_or(manifest_url);
    format!("{base}{shard_path}")
}

/// Local on-disk location for a manifest path, under
/// `<data_dir>/shards/<shard_path without leading slash>`.
pub fn shard_local_path(data_dir: &Path, shard_path: &str) -> PathBuf {
    let stripped = shard_path.trim_start_matches('/');
    data_dir.join("shards").join(stripped)
}

/// Clone (if missing) or fetch (if present) one shard from upstream.
///
/// Uses gix's smart-HTTP transport (enabled via the
/// `blocking-http-transport-reqwest-rust-tls` feature on our `gix`
/// pin). Bare repo semantics — no working tree, no index.
pub fn fetch_shard(data_dir: &Path, shard_path: &str, manifest_url: &str) -> Result<FetchOutcome> {
    let url = shard_url(manifest_url, shard_path);
    let local = shard_local_path(data_dir, shard_path);
    if !local.exists() {
        clone_shard(&url, &local)?;
        return Ok(FetchOutcome::Cloned);
    }
    if !repo_has_usable_refs(&local) {
        reclone_shard(&url, &local)?;
        return Ok(FetchOutcome::Recloned);
    }
    fetch_existing_shard(&url, &local)?;
    if !repo_has_usable_refs(&local) {
        reclone_shard(&url, &local)?;
        return Ok(FetchOutcome::Recloned);
    }
    Ok(FetchOutcome::Fetched)
}

fn clone_shard(url: &str, local: &Path) -> Result<()> {
    if let Some(parent) = local.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Sync(format!("mkdir {}: {e}", parent.display())))?;
    }
    let should_interrupt = &std::sync::atomic::AtomicBool::new(false);
    gix::prepare_clone_bare(url, local)
        .map_err(|e| {
            Error::Sync(format!(
                "prepare_clone_bare {url} -> {}: {e}",
                local.display()
            ))
        })?
        .fetch_only(gix::progress::Discard, should_interrupt)
        .map_err(|e| Error::Sync(format!("clone {url} -> {}: {e}", local.display())))?;
    Ok(())
}

fn reclone_shard(url: &str, local: &Path) -> Result<()> {
    remove_path(local)?;
    clone_shard(url, local)
}

fn remove_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        std::fs::remove_dir_all(path)
            .map_err(|e| Error::Sync(format!("remove {}: {e}", path.display())))?;
    } else {
        std::fs::remove_file(path)
            .map_err(|e| Error::Sync(format!("remove {}: {e}", path.display())))?;
    }
    Ok(())
}

fn fetch_existing_shard(url: &str, local: &Path) -> Result<()> {
    let repo =
        gix::open(local).map_err(|e| Error::Sync(format!("open repo {}: {e}", local.display())))?;
    let should_interrupt = &std::sync::atomic::AtomicBool::new(false);
    let remote = repo
        .remote_at(url)
        .map_err(|e| Error::Sync(format!("remote_at {url}: {e}")))?
        .with_refspecs(["+refs/*:refs/*"], gix::remote::Direction::Fetch)
        .map_err(|e| Error::Sync(format!("refspecs {url}: {e}")))?;
    let connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|e| Error::Sync(format!("connect {url}: {e}")))?;
    let prepare = connection
        .prepare_fetch(gix::progress::Discard, Default::default())
        .map_err(|e| Error::Sync(format!("prepare_fetch {url}: {e}")))?;
    prepare
        .receive(gix::progress::Discard, should_interrupt)
        .map_err(|e| Error::Sync(format!("receive {url}: {e}")))?;
    Ok(())
}

fn repo_has_usable_refs(local: &Path) -> bool {
    let repo = match Git2Repository::open_bare(local).or_else(|_| Git2Repository::open(local)) {
        Ok(repo) => repo,
        Err(_) => return false,
    };
    let refs = match repo.references() {
        Ok(refs) => refs,
        Err(_) => return false,
    };
    for reference in refs.flatten() {
        if reference.target().is_some() {
            return true;
        }
        if let Some(symbolic_target) = reference.symbolic_target()
            && let Ok(target_ref) = repo.find_reference(symbolic_target)
            && target_ref.target().is_some()
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn git(args: &[&str], cwd: &Path) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_stdout(args: &[&str], cwd: &Path) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn meta(fp: &str) -> ShardMeta {
        ShardMeta {
            fingerprint: fp.into(),
            modified: None,
            owner: None,
            reference: None,
        }
    }

    #[test]
    fn fnmatch_star_and_literal() {
        assert!(fnmatch("*", "anything"));
        assert!(fnmatch("/lkml/*", "/lkml/git/0.git"));
        assert!(!fnmatch("/lkml/*", "/netdev/git/0.git"));
        assert!(fnmatch("/linux-??/*", "/linux-fs/x"));
        assert!(!fnmatch("/linux-???/*", "/linux-fs/x"));
    }

    #[test]
    fn path_matches_defaults_and_exclude_wins() {
        let none: Vec<String> = vec![];
        assert!(path_matches("/anything", &none, &none));

        let inc = vec!["/lkml/*".into(), "/netdev/*".into()];
        assert!(path_matches("/lkml/git/0.git", &inc, &none));
        assert!(!path_matches("/other/x", &inc, &none));

        let exc = vec!["/lkml/git/99.git".into()];
        assert!(!path_matches("/lkml/git/99.git", &inc, &exc));
        assert!(path_matches("/lkml/git/0.git", &inc, &exc));
    }

    #[test]
    fn diff_manifest_detects_new_and_changed() {
        let local: Manifest = [("/a".into(), meta("old"))].into_iter().collect();
        let remote: Manifest = [
            ("/a".into(), meta("new")), // changed
            ("/b".into(), meta("bfp")), // new
            ("/c".into(), meta("cfp")), // new
        ]
        .into_iter()
        .collect();
        let none: Vec<String> = vec![];
        let mut changed = diff_manifest(&remote, &local, &none, &none);
        changed.sort();
        assert_eq!(changed, vec!["/a", "/b", "/c"]);
    }

    #[test]
    fn diff_manifest_honors_include_exclude() {
        let local: Manifest = Manifest::new();
        let remote: Manifest = [
            ("/lkml/git/0.git".into(), meta("x")),
            ("/netdev/git/0.git".into(), meta("x")),
            ("/private/git/0.git".into(), meta("x")),
        ]
        .into_iter()
        .collect();
        let include = vec!["/lkml/*".into(), "/netdev/*".into()];
        let exclude: Vec<String> = vec![];
        let mut changed = diff_manifest(&remote, &local, &include, &exclude);
        changed.sort();
        assert_eq!(changed, vec!["/lkml/git/0.git", "/netdev/git/0.git"]);
    }

    #[test]
    fn diff_manifest_reports_nothing_when_fingerprints_match() {
        let local: Manifest = [("/a".into(), meta("one")), ("/b".into(), meta("two"))]
            .into_iter()
            .collect();
        let remote = local.clone();
        let none: Vec<String> = vec![];
        assert!(diff_manifest(&remote, &local, &none, &none).is_empty());
    }

    #[test]
    fn shard_url_strips_manifest_filename() {
        assert_eq!(
            shard_url(
                "https://lore.kernel.org/manifest.js.gz",
                "/netdev/git/0.git"
            ),
            "https://lore.kernel.org/netdev/git/0.git"
        );
    }

    #[test]
    fn shard_local_path_joins_under_shards_dir() {
        let p = shard_local_path(Path::new("/data"), "/netdev/git/0.git");
        assert_eq!(p, Path::new("/data/shards/netdev/git/0.git"));
    }

    #[test]
    fn manifest_roundtrip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let m: Manifest = [("/a".into(), meta("fp1")), ("/b".into(), meta("fp2"))]
            .into_iter()
            .collect();
        save_local_manifest(dir.path(), &m).unwrap();
        let loaded = load_local_manifest(dir.path()).unwrap();
        assert_eq!(loaded, m);
    }

    #[test]
    fn load_local_manifest_missing_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_local_manifest(dir.path()).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn fetch_shard_reclones_repo_with_no_usable_refs() {
        let upstream_root = tempfile::tempdir().unwrap();
        let remote = upstream_root.path().join("list.git");
        git(&["init", "-q", "--bare", "list.git"], upstream_root.path());

        let work = tempfile::tempdir().unwrap();
        git(&["init", "-q", "-b", "master", "."], work.path());
        fs::write(work.path().join("m"), "hello\n").unwrap();
        git(&["add", "m"], work.path());
        git(&["commit", "-q", "-m", "c1"], work.path());
        git(
            &["remote", "add", "origin", remote.to_str().unwrap()],
            work.path(),
        );
        git(&["push", "-q", "origin", "master"], work.path());

        let data = tempfile::tempdir().unwrap();
        let local = shard_local_path(data.path(), "/list.git");
        fs::create_dir_all(&local).unwrap();
        git(&["init", "-q", "--bare", "."], &local);
        assert!(!repo_has_usable_refs(&local));

        let manifest_url = format!("{}/manifest.js.gz", upstream_root.path().display());
        let outcome = fetch_shard(data.path(), "/list.git", &manifest_url).unwrap();
        assert!(matches!(outcome, FetchOutcome::Recloned));
        assert!(repo_has_usable_refs(&local));

        let remote_head = git_stdout(&["rev-parse", "refs/heads/master"], &remote);
        let local_head = git_stdout(&["rev-parse", "refs/heads/master"], &local);
        assert_eq!(local_head.trim(), remote_head.trim());
    }
}
