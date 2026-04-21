//! End-to-end ingest: walk one public-inbox shard, append bodies to the
//! compressed store, accumulate metadata rows, flush to Parquet.

#![allow(dead_code)]
//!
//! v0.5 scope:
//!   * single-shard walker (multi-shard / multi-list fanout is the CLI
//!     binary's job; we keep this function small and testable)
//!   * incremental via `state::last_indexed_oid` when present;
//!     full-walk fallback if the stored OID is dangling upstream
//!   * one Parquet file per shard per run
//!   * bump `state::generation` once after every commit of the writer
//!     to signal readers
//!
//! Deliberately NOT in scope yet:
//!   * tantivy + trigram writes (phases 2/3)
//!   * tid (thread-id) computation (needs cross-message join)
//!   * cover-letter → patch touched-file propagation (ditto)

use std::path::Path;
use std::sync::Mutex;

use git2::Repository as Git2Repository;
use gix::ObjectId;

use crate::bm25::BmWriter;
use crate::error::{Error, Result};
use crate::metadata::{self, MetadataBatch, MetadataRow};
use crate::over::{DddPayload, OverDb, OverRow};
use crate::parse::{self, ParsedMessage};
use crate::state::State;
use crate::store::{Store, StoreOffset};
use crate::trigram::{segment_dir as trigram_segment_dir, SegmentBuilder as TrigramBuilder};

/// Ingest one public-inbox shard end-to-end.
///
/// `shard_path` points at a bare git repo (the `N.git` directory for a
/// single public-inbox shard). `list` is the list name (e.g.
/// `linux-cifs`); `shard` is the shard number as a string.
pub fn ingest_shard(
    data_dir: &Path,
    shard_path: &Path,
    list: &str,
    shard: &str,
    run_id: &str,
) -> Result<IngestStats> {
    // Acquire the writer lock for the duration of this call. Callers
    // that already hold the lock (the `kernel-lore-ingest` binary
    // acquires it once per run so rayon can fan out across shards
    // without collision) use `ingest_shard_unlocked` directly.
    let state = State::new(data_dir)?;
    let _lock = state.acquire_writer_lock()?;
    ingest_shard_unlocked(data_dir, shard_path, list, shard, run_id)
}

/// Same as `ingest_shard` but assumes the caller already holds
/// `state::acquire_writer_lock`. Use this when fanning out across
/// multiple shards under one outer lock.
pub fn ingest_shard_unlocked(
    data_dir: &Path,
    shard_path: &Path,
    list: &str,
    shard: &str,
    run_id: &str,
) -> Result<IngestStats> {
    // Default: build BM25 inline (backward compat for the Python
    // single-shard path). The Rust binary uses skip_bm25=true.
    ingest_shard_with_bm25(
        data_dir, shard_path, list, shard, run_id, None, None, None, false,
    )
}

/// Full-control variant: accepts optional shared writers so
/// multi-shard binaries can fan out via rayon while still writing to
/// a single compressed store + tantivy index per list.
///
/// - When `shared_bm25` is `Some(mutex)`, the caller is responsible
///   for committing it once per run, after all shards finish.
/// - When `None`, this function opens its own writer, commits, and
///   drops it when the shard finishes.
/// - When `shared_store` is `Some(mutex)`, all same-list shards
///   serialize their appends through one `Store` instance, keeping
///   the cached offset counter correct. **Required** when
///   multiple shards from the same list are ingested in parallel;
///   without it, each shard's independent `SegmentWriter::position`
///   goes stale and the returned `StoreOffset` can point at the
///   wrong frame.
/// - When `None`, this function opens its own `Store`.
///
/// `skip_bm25`: when true (the default for the hot-path ingest),
/// the BM25 tier is not written. This makes ingest ~12x faster
/// because tantivy tokenization + segment management dominates
/// per-message cost. The BM25 index is built separately via
/// `rebuild_bm25()` after the hot path finishes.
///
/// `shared_bm25` / `shared_store`: see earlier docs.
///
/// `shared_over`: when `Some`, every parsed message's row is appended
/// to the shared `OverDb` in a single transaction that commits AFTER
/// the per-shard Parquet write succeeds. `INSERT OR REPLACE` keyed on
/// `(message_id, list)` keeps re-ingests idempotent. When `None`, the
/// over.db tier is skipped entirely (preserving the legacy behavior
/// for callers — typically the Python single-shard path — that don't
/// hold a writer for it).
#[allow(clippy::too_many_arguments)]
pub fn ingest_shard_with_bm25(
    data_dir: &Path,
    shard_path: &Path,
    list: &str,
    shard: &str,
    run_id: &str,
    shared_bm25: Option<&Mutex<BmWriter>>,
    shared_store: Option<&Mutex<Store>>,
    shared_over: Option<&Mutex<OverDb>>,
    skip_bm25: bool,
) -> Result<IngestStats> {
    let state = State::new(data_dir)?;
    let owned_store;
    let store_ref: &Mutex<Store> = match shared_store {
        Some(s) => s,
        None => {
            owned_store = Mutex::new(Store::open(data_dir, list)?);
            &owned_store
        }
    };

    let mut repo = gix::open(shard_path)
        .map_err(|e| Error::Gix(format!("open {}: {e}", shard_path.display())))?;

    // Set a large object cache so the packfile decompression cache
    // stays warm across the walk. The default is 64 entries which
    // thrashes on repos with 60k+ objects. 256 MB cache is generous
    // but transforms pack-heavy reads from ~50 ms/blob to ~1 ms/blob.
    // This is the equivalent of git's cat-file --batch keeping its
    // mmaps warm for the lifetime of the process.
    repo.object_cache_size(256 * 1024 * 1024);

    let head_id = resolve_walk_head_id(&repo, shard_path)?;
    let head_hex = head_id.to_string();

    // Build the walk; use incremental when we have a last-indexed oid
    // and that oid is still reachable.
    let mut platform = repo.rev_walk([head_id]);
    let last = state.last_indexed_oid(list, shard)?;
    if let Some(ref oid_hex) = last {
        if let Ok(parsed) = oid_hex.parse::<ObjectId>() {
            if repo.find_object(parsed).is_ok() {
                platform = platform.with_hidden([parsed]);
            }
            // else: dangling (shard was repacked upstream); full walk.
        }
    }

    let walk = platform
        .all()
        .map_err(|e| Error::Gix(format!("rev_walk: {e}")))?;

    let mut batch = MetadataBatch::new();
    let mut trigram = TrigramBuilder::new();
    // Buffered until the per-shard Parquet write succeeds; only then
    // do we open the over.db transaction. Keeps Parquet as the
    // ordering authority for "this shard committed".
    let mut over_rows: Vec<OverRow> = if shared_over.is_some() {
        Vec::with_capacity(1024)
    } else {
        Vec::new()
    };
    // local_bm25 is Some when (a) the caller didn't supply a shared
    // writer AND (b) skip_bm25 is false. When skip_bm25 is true, no
    // BM25 work happens at all — the index is built separately via
    // rebuild_bm25().
    let mut local_bm25: Option<BmWriter> = if !skip_bm25 && shared_bm25.is_none() {
        Some(BmWriter::open(data_dir)?)
    } else {
        None
    };
    let mut stats = IngestStats::default();

    for info in walk {
        let info = info.map_err(|e| Error::Gix(format!("walk step: {e}")))?;
        let commit = info
            .object()
            .map_err(|e| Error::Gix(format!("commit object: {e}")))?;
        let tree = commit
            .tree()
            .map_err(|e| Error::Gix(format!("commit tree: {e}")))?;
        let Some(m) = tree.find_entry("m") else {
            stats.skipped_no_m += 1;
            continue;
        };
        let blob = m
            .object()
            .map_err(|e| Error::Gix(format!("blob object: {e}")))?;
        let data = &blob.data;
        if data.is_empty() {
            stats.skipped_empty += 1;
            continue;
        }

        // Extract commit author date for the parse_message fallback.
        // commit.time() returns gix_date::Time with .seconds field.
        let commit_date_ns = commit.time().ok().map(|t| t.seconds * 1_000_000_000);
        let parsed = parse::parse_message(data, commit_date_ns);
        let Some(raw_mid) = parsed.message_id.clone() else {
            stats.skipped_no_mid += 1;
            continue;
        };
        // RFC 2822 header folding can leave \r\n + whitespace inside
        // Message-IDs. Normalize by collapsing all whitespace runs to
        // nothing — Message-IDs are opaque tokens, never contain
        // intentional spaces.
        let mid: String = raw_mid.split_whitespace().collect();

        // Patch goes to trigram tier BEFORE we consume `parsed` into the
        // metadata row.
        if let Some(patch_text) = parsed.patch.as_deref() {
            trigram.add(&mid, patch_text.as_bytes())?;
        }

        // Prose (body minus patch) + normalized subject go to BM25.
        // Skipped when skip_bm25 is true (the hot-path default).
        if !skip_bm25 && (!parsed.prose.is_empty() || parsed.subject_normalized.is_some()) {
            match (&mut local_bm25, shared_bm25) {
                (Some(w), _) => {
                    w.add(
                        &mid,
                        list,
                        parsed.subject_normalized.as_deref(),
                        &parsed.prose,
                    )?;
                }
                (None, Some(mutex)) => {
                    let mut w = mutex
                        .lock()
                        .map_err(|_| Error::State("shared bm25 writer poisoned".to_owned()))?;
                    w.add(
                        &mid,
                        list,
                        parsed.subject_normalized.as_deref(),
                        &parsed.prose,
                    )?;
                }
                (None, None) => {}
            }
        }

        let appended = store_ref
            .lock()
            .map_err(|_| Error::State("store mutex poisoned".to_owned()))?
            .append(data)?;
        let body_sha256_hex = hex(&appended.body_sha256);
        if shared_over.is_some() {
            over_rows.push(build_over_row(
                &mid,
                list,
                &parsed,
                appended.ptr,
                appended.body_length,
                &body_sha256_hex,
                shard,
                &info.id.to_string(),
            ));
        }
        let row = MetadataRow {
            list,
            shard,
            commit_oid: &info.id.to_string(),
            offset: appended.ptr,
            body_sha256_hex,
            body_length: appended.body_length,
            parsed,
        };
        batch.push(row);
        stats.ingested += 1;
    }

    store_ref
        .lock()
        .map_err(|_| Error::State("store mutex poisoned".to_owned()))?
        .flush()?;

    if batch.is_empty() {
        // Nothing new; still advance the oid so we don't re-walk.
        state.save_last_indexed_oid(list, shard, &head_hex)?;
        return Ok(stats);
    }

    let rb = batch.finish()?;
    let parquet_path = metadata::write_parquet(data_dir, list, run_id, &rb)?;
    stats.parquet_path = Some(parquet_path.display().to_string());

    // over.db write — strictly AFTER Parquet succeeds. A failure here
    // is logged and surfaced via `stats.over_failed` but does NOT
    // abort the shard: Parquet (the source-of-truth metadata tier)
    // already succeeded, and a future ingest will repopulate the
    // missing rows via INSERT OR REPLACE on (message_id, list).
    if let Some(over_mutex) = shared_over {
        if !over_rows.is_empty() {
            match over_mutex.lock() {
                Ok(mut over) => match over.insert_batch(&over_rows) {
                    Ok(()) => {
                        stats.over_rows_written = over_rows.len() as u64;
                    }
                    Err(e) => {
                        stats.over_failed = true;
                        tracing::error!(
                            list = list,
                            shard = shard,
                            rows = over_rows.len(),
                            error = %e,
                            "over.db insert_batch failed (shard's parquet write succeeded; \
                             will be reconciled on next ingest via INSERT OR REPLACE)"
                        );
                    }
                },
                Err(_) => {
                    stats.over_failed = true;
                    tracing::error!(
                        list = list,
                        shard = shard,
                        "over.db mutex poisoned; skipping over.db write for this shard"
                    );
                }
            }
        }
    }

    // Trigram segment — only finalize if we indexed at least one patch.
    if !trigram.is_empty() {
        let seg = trigram_segment_dir(data_dir, list, run_id);
        trigram.finalize(&seg)?;
        stats.trigram_segment_path = Some(seg.display().to_string());
    }

    // BM25 commit: only if we own the writer. Shared-writer callers
    // commit once at end-of-run.
    if let Some(mut w) = local_bm25 {
        let opstamp = w.commit()?;
        stats.bm25_opstamp = Some(opstamp);
    }

    state.save_last_indexed_oid(list, shard, &head_hex)?;

    // Bump generation ONLY when this call is the end-to-end pipeline
    // for a single shard: caller gave us no shared BM25 writer AND
    // asked us to build BM25 inline. That's the
    // `ingest_shard` / `ingest_shard_unlocked` path (Python single-
    // shard, tests).
    //
    // The multi-shard binaries (kernel-lore-ingest, kernel-lore-sync)
    // either pass a shared BM25 writer OR pass `skip_bm25 = true`; in
    // both shapes they take responsibility for committing BM25,
    // rebuilding tid, and bumping generation once after all shards
    // finish. If we bumped here too we'd emit N+1 generations per
    // multi-shard run — readers would see intermediate snapshots
    // where some tiers had advanced and others hadn't.
    //
    // Per-tier generation markers follow the same rule. On an over.db
    // write failure the main gen still advances (Parquet succeeded)
    // but `over.generation` stays behind — Reader::new sees the
    // drift and bypasses over.db until the next clean ingest.
    let caller_orchestrates = shared_bm25.is_some() || skip_bm25;
    if !caller_orchestrates {
        let new_gen = state.bump_generation()?;
        if !stats.over_failed {
            state.set_tier_generation("over", new_gen)?;
        }
        state.set_tier_generation("bm25", new_gen)?;
        state.set_tier_generation("trigram", new_gen)?;
    }

    Ok(stats)
}

fn resolve_walk_head_id(repo: &gix::Repository, shard_path: &Path) -> Result<ObjectId> {
    match repo.head_id() {
        Ok(head) => Ok(head.detach()),
        Err(head_err) => {
            if let Some((head_id, repaired_ref)) = fallback_head_id_from_refs(shard_path)? {
                tracing::warn!(
                    shard = %shard_path.display(),
                    repaired_head_ref = repaired_ref,
                    head_oid = %head_id,
                    "repo HEAD was unborn; recovered ingest tip from branch refs"
                );
                Ok(head_id)
            } else {
                Err(Error::Gix(format!("head_id: {head_err}")))
            }
        }
    }
}

struct HeadCandidate {
    rank: usize,
    source_ref: String,
    repair_ref: String,
    git2_oid: git2::Oid,
    gix_oid: ObjectId,
}

fn fallback_head_id_from_refs(shard_path: &Path) -> Result<Option<(ObjectId, String)>> {
    let repo = Git2Repository::open_bare(shard_path)
        .or_else(|_| Git2Repository::open(shard_path))
        .map_err(|e| Error::Gix(format!("open {}: {e}", shard_path.display())))?;

    let mut candidates: Vec<HeadCandidate> = Vec::new();
    push_ref_candidate(
        &repo,
        "refs/heads/master",
        "refs/heads/master",
        0,
        &mut candidates,
    )?;
    push_ref_candidate(
        &repo,
        "refs/heads/main",
        "refs/heads/main",
        1,
        &mut candidates,
    )?;
    push_symbolic_candidate(&repo, "refs/remotes/origin/HEAD", 2, &mut candidates)?;
    push_ref_candidate(
        &repo,
        "refs/remotes/origin/master",
        "refs/heads/master",
        3,
        &mut candidates,
    )?;
    push_ref_candidate(
        &repo,
        "refs/remotes/origin/main",
        "refs/heads/main",
        4,
        &mut candidates,
    )?;

    let refs = repo
        .references()
        .map_err(|e| Error::Gix(format!("references {}: {e}", shard_path.display())))?;
    for reference in refs {
        let reference = reference
            .map_err(|e| Error::Gix(format!("reference {}: {e}", shard_path.display())))?;
        let Some(name) = reference.name() else {
            continue;
        };
        let Some(rank) = rank_for_head_fallback(name) else {
            continue;
        };
        let Some(repair_ref) = local_head_ref_for(name) else {
            continue;
        };
        if let Some(target) = reference.target() {
            candidates.push(HeadCandidate {
                rank,
                source_ref: name.to_owned(),
                repair_ref,
                git2_oid: target,
                gix_oid: git2_oid_to_gix(target)?,
            });
        }
    }

    candidates.sort_by(|a, b| {
        a.rank
            .cmp(&b.rank)
            .then_with(|| a.source_ref.cmp(&b.source_ref))
    });
    let Some(candidate) = candidates.into_iter().next() else {
        return Ok(None);
    };

    if let Err(e) = repair_head(&repo, &candidate.repair_ref, candidate.git2_oid) {
        tracing::warn!(
            shard = %shard_path.display(),
            source_ref = candidate.source_ref,
            repaired_head_ref = candidate.repair_ref,
            error = %e,
            "failed to repoint unborn repo HEAD; continuing with recovered ref tip"
        );
    }

    Ok(Some((candidate.gix_oid, candidate.repair_ref)))
}

fn push_ref_candidate(
    repo: &Git2Repository,
    source_ref: &str,
    repair_ref: &str,
    rank: usize,
    out: &mut Vec<HeadCandidate>,
) -> Result<()> {
    let Ok(reference) = repo.find_reference(source_ref) else {
        return Ok(());
    };
    let Some(target) = reference.target() else {
        return Ok(());
    };
    out.push(HeadCandidate {
        rank,
        source_ref: source_ref.to_owned(),
        repair_ref: repair_ref.to_owned(),
        git2_oid: target,
        gix_oid: git2_oid_to_gix(target)?,
    });
    Ok(())
}

fn push_symbolic_candidate(
    repo: &Git2Repository,
    symbolic_ref: &str,
    rank: usize,
    out: &mut Vec<HeadCandidate>,
) -> Result<()> {
    let Ok(reference) = repo.find_reference(symbolic_ref) else {
        return Ok(());
    };
    let Some(target_name) = reference.symbolic_target() else {
        return Ok(());
    };
    let Some(repair_ref) = local_head_ref_for(target_name) else {
        return Ok(());
    };
    push_ref_candidate(repo, target_name, &repair_ref, rank, out)
}

fn local_head_ref_for(refname: &str) -> Option<String> {
    if refname.starts_with("refs/heads/") {
        return Some(refname.to_owned());
    }
    let remainder = refname.strip_prefix("refs/remotes/")?;
    let (_, branch_name) = remainder.split_once('/')?;
    if branch_name == "HEAD" {
        return None;
    }
    Some(format!("refs/heads/{branch_name}"))
}

fn rank_for_head_fallback(refname: &str) -> Option<usize> {
    match refname {
        "refs/heads/master" => Some(0),
        "refs/heads/main" => Some(1),
        "refs/remotes/origin/HEAD" => Some(2),
        "refs/remotes/origin/master" => Some(3),
        "refs/remotes/origin/main" => Some(4),
        other if other.starts_with("refs/heads/") => Some(5),
        other if other.starts_with("refs/remotes/") && !other.ends_with("/HEAD") => Some(6),
        _ => None,
    }
}

fn repair_head(repo: &Git2Repository, repair_ref: &str, oid: git2::Oid) -> Result<()> {
    repo.reference(
        repair_ref,
        oid,
        true,
        "kernel-lore-mcp: repair unborn HEAD from fetched refs",
    )
    .map_err(|e| Error::Gix(format!("reference {repair_ref}: {e}")))?;
    repo.set_head(repair_ref)
        .map_err(|e| Error::Gix(format!("set_head {repair_ref}: {e}")))?;
    Ok(())
}

fn git2_oid_to_gix(oid: git2::Oid) -> Result<ObjectId> {
    oid.to_string()
        .parse::<ObjectId>()
        .map_err(|e| Error::Gix(format!("parse oid {oid}: {e}")))
}

/// Aggregate counters from a single `ingest_shard` invocation.
#[derive(Debug, Default, Clone)]
pub struct IngestStats {
    pub ingested: u64,
    pub skipped_no_m: u64,
    pub skipped_empty: u64,
    pub skipped_no_mid: u64,
    pub parquet_path: Option<String>,
    pub trigram_segment_path: Option<String>,
    pub bm25_opstamp: Option<u64>,
    /// Number of rows written to over.db for this shard. Zero when
    /// the over.db tier is disabled.
    pub over_rows_written: u64,
    /// True iff Parquet succeeded but the over.db transaction failed.
    /// The binary uses this to decide its exit code (2 = partial).
    pub over_failed: bool,
}

/// Project a `ParsedMessage` plus its body locator into the
/// over.db row layout. Indexed columns get the lowercased
/// `from_addr`; the original-case form is preserved inside `ddd`
/// so display paths don't lose information.
#[allow(clippy::too_many_arguments)]
fn build_over_row(
    message_id: &str,
    list: &str,
    parsed: &ParsedMessage,
    ptr: StoreOffset,
    body_length: u64,
    body_sha256_hex: &str,
    shard: &str,
    commit_oid: &str,
) -> OverRow {
    let trailers_json = if parsed.trailers.is_empty() {
        None
    } else {
        serde_json::to_string(&parsed.trailers).ok()
    };
    OverRow {
        message_id: message_id.to_owned(),
        list: list.to_owned(),
        from_addr: parsed.from_addr.as_deref().map(str::to_ascii_lowercase),
        date_unix_ns: parsed.date_unix_ns,
        in_reply_to: parsed.in_reply_to.clone(),
        // tid is rebuilt cross-corpus after every ingest run via
        // `rebuild_tid`; we leave it NULL here and let the rebuild
        // populate it. `INSERT OR REPLACE` on (message_id, list) means
        // we don't risk overwriting a fresher tid because the
        // rebuild writes through its own dedicated update path
        // (Phase 5 wiring; for Phase 4 the column simply stays NULL
        // until that wire-up lands).
        tid: None,
        body_segment_id: ptr.segment_id as i64,
        body_offset: ptr.offset as i64,
        body_length: body_length as i64,
        body_sha256: body_sha256_hex.to_owned(),
        has_patch: parsed.has_patch,
        is_cover_letter: parsed.is_cover_letter,
        series_version: Some(parsed.series_version as i64),
        series_index: parsed.series_index.map(|v| v as i64),
        series_total: parsed.series_total.map(|v| v as i64),
        files_changed: parsed.files_changed.map(|v| v as i64),
        insertions: parsed.insertions.map(|v| v as i64),
        deletions: parsed.deletions.map(|v| v as i64),
        commit_oid: Some(commit_oid.to_owned()),
        ddd: DddPayload {
            subject_raw: parsed.subject_raw.clone(),
            subject_normalized: parsed.subject_normalized.clone(),
            subject_tags: parsed.subject_tags.clone(),
            references: parsed.references.clone(),
            touched_files: parsed.touched_files.clone(),
            touched_functions: parsed.touched_functions.clone(),
            signed_off_by: parsed.signed_off_by.clone(),
            reviewed_by: parsed.reviewed_by.clone(),
            acked_by: parsed.acked_by.clone(),
            tested_by: parsed.tested_by.clone(),
            co_developed_by: parsed.co_developed_by.clone(),
            reported_by: parsed.reported_by.clone(),
            suggested_by: parsed.suggested_by.clone(),
            helped_by: parsed.helped_by.clone(),
            assisted_by: parsed.assisted_by.clone(),
            fixes: parsed.fixes.clone(),
            link: parsed.link.clone(),
            closes: parsed.closes.clone(),
            cc_stable: parsed.cc_stable.clone(),
            trailers_json,
            from_name: parsed.from_name.clone(),
            from_addr_original_case: parsed.from_addr.clone(),
            shard: Some(shard.to_owned()),
        },
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Rebuild the BM25 index from the compressed store + metadata.
///
/// Reads every message body from the store, splits prose/patch,
/// and batch-adds to tantivy. This is the deferred pass that runs
/// after the hot-path ingest (which skips BM25 for speed).
///
/// Idempotent: each call rebuilds the entire BM25 index from
/// scratch. Safe to run while the MCP server is serving queries —
/// tantivy's reader reload picks up the new segments on the next
/// query boundary.
pub fn rebuild_bm25(data_dir: &Path) -> Result<u64> {
    use crate::reader::Reader;
    use std::collections::HashMap;

    let reader = Reader::new(data_dir);
    let mut writer = BmWriter::open(data_dir)?;

    // Stream rows through the indexer; never materialize the full
    // corpus. Previously this called scan_all into a Vec which OOMed
    // at 17.6M-row scale (~49 GB RSS).
    let mut stores: HashMap<String, crate::store::Store> = HashMap::new();
    let mut count: u64 = 0;
    let mut err: Option<Error> = None;

    reader.scan_streaming(None, |row| {
        let store = match stores.get(&row.list) {
            Some(s) => s,
            None => match crate::store::Store::open(data_dir, &row.list) {
                Ok(s) => {
                    stores.insert(row.list.clone(), s);
                    stores.get(&row.list).unwrap()
                }
                Err(e) => {
                    err = Some(e);
                    return false;
                }
            },
        };
        let body = match store.read_at(row.body_segment_id, row.body_offset) {
            Ok(b) => b,
            Err(_) => return true,
        };

        let text = std::str::from_utf8(&body).unwrap_or("");

        let prose = if text.starts_with("diff --git ") {
            ""
        } else if let Some(idx) = text.find("\ndiff --git ") {
            &text[..idx + 1]
        } else {
            text
        };

        let subject = row
            .subject_normalized
            .as_deref()
            .or(row.subject_raw.as_deref());

        if !prose.is_empty() || subject.is_some() {
            if let Err(e) = writer.add(&row.message_id, &row.list, subject, prose) {
                err = Some(e);
                return false;
            }
            count += 1;
        }
        true
    })?;

    if let Some(e) = err {
        return Err(e);
    }

    writer.commit()?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn run_git(args: &[&str], cwd: &Path) {
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
    }

    /// Build a minimal "public-inbox-like" shard: a bare repo with one
    /// commit per message, each commit's tree containing a single blob
    /// named `m` holding the raw RFC822 message.
    fn make_synthetic_shard(shard_dir: &Path, messages: &[&[u8]]) {
        // Build in a working tree, then clone --bare into shard_dir.
        let work = tempdir().unwrap();
        run_git(&["init", "-q", "-b", "master", "."], work.path());
        for (i, msg) in messages.iter().enumerate() {
            std::fs::write(work.path().join("m"), msg).unwrap();
            run_git(&["add", "m"], work.path());
            run_git(&["commit", "-q", "-m", &format!("msg {i}")], work.path());
        }
        // Clone --bare into final location.
        if shard_dir.exists() {
            std::fs::remove_dir_all(shard_dir).unwrap();
        }
        run_git(
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

    fn sample_messages() -> Vec<Vec<u8>> {
        vec![
            b"From: Alice <alice@example.com>\r\n\
Subject: [PATCH v2 1/2] ksmbd: tighten ACL bounds\r\n\
Date: Mon, 14 Apr 2026 12:00:00 +0000\r\n\
Message-ID: <m1@x>\r\n\
\r\n\
Prose.\r\n\
Signed-off-by: Alice <alice@example.com>\r\n\
---\r\n\
diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c\r\n\
--- a/fs/smb/server/smbacl.c\r\n\
+++ b/fs/smb/server/smbacl.c\r\n\
@@ -1,1 +1,2 @@ int smb_check_perm_dacl(struct ksmbd_conn *c)\r\n\
 a\r\n\
+b\r\n"
                .to_vec(),
            b"From: Bob <bob@example.com>\r\n\
Subject: [PATCH v2 2/2] ksmbd: fix follow-up\r\n\
Date: Mon, 14 Apr 2026 12:05:00 +0000\r\n\
Message-ID: <m2@x>\r\n\
In-Reply-To: <m1@x>\r\n\
References: <m1@x>\r\n\
\r\n\
More prose.\r\n\
Signed-off-by: Bob <bob@example.com>\r\n\
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

    #[test]
    fn end_to_end_ingest_synthetic_shard() {
        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let msgs = sample_messages();
        let msg_refs: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
        make_synthetic_shard(&shard_dir, &msg_refs);

        let data = tempdir().unwrap();
        let stats = ingest_shard(data.path(), &shard_dir, "linux-cifs", "0", "run-0001").unwrap();

        assert_eq!(stats.ingested, 2);
        assert!(stats.parquet_path.is_some());

        // State recorded.
        let state = State::new(data.path()).unwrap();
        assert!(state.last_indexed_oid("linux-cifs", "0").unwrap().is_some());
        assert_eq!(state.generation().unwrap(), 1);

        // Parquet file exists and is non-trivial.
        let p = data.path().join("metadata/linux-cifs/run-0001.parquet");
        assert!(p.exists());
        assert!(p.metadata().unwrap().len() > 500);

        // Store has segment-000000.zst with both messages.
        let seg = data.path().join("store/linux-cifs/segment-000000.zst");
        assert!(seg.exists());
        assert!(seg.metadata().unwrap().len() > 0);
    }

    #[test]
    fn unborn_head_recovers_from_master_ref() {
        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let msgs = sample_messages();
        let msg_refs: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
        make_synthetic_shard(&shard_dir, &msg_refs);

        // Reproduce the bad local-clone state from sync: HEAD points
        // at an unborn `main`, but the real commits live on `master`.
        run_git(&["symbolic-ref", "HEAD", "refs/heads/main"], &shard_dir);

        let data = tempdir().unwrap();
        let stats = ingest_shard(data.path(), &shard_dir, "linux-wireless", "0", "run-0001")
            .expect("ingest should recover from unborn HEAD");
        assert_eq!(stats.ingested, 2);

        let out = Command::new("git")
            .args(["symbolic-ref", "HEAD"])
            .current_dir(&shard_dir)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "refs/heads/master"
        );
    }

    #[test]
    fn unborn_head_recovers_from_remote_tracking_ref() {
        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let msgs = sample_messages();
        let msg_refs: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
        make_synthetic_shard(&shard_dir, &msg_refs);

        let out = Command::new("git")
            .args(["rev-parse", "refs/heads/master"])
            .current_dir(&shard_dir)
            .output()
            .unwrap();
        assert!(out.status.success());
        let head_oid = String::from_utf8_lossy(&out.stdout).trim().to_owned();

        // Simulate a stale gix clone from an older run:
        // only remote-tracking refs have commits, while local HEAD
        // still points at unborn refs/heads/main.
        run_git(
            &[
                "update-ref",
                "refs/remotes/origin/master",
                head_oid.as_str(),
            ],
            &shard_dir,
        );
        run_git(
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/master",
            ],
            &shard_dir,
        );
        run_git(&["update-ref", "-d", "refs/heads/master"], &shard_dir);
        run_git(&["symbolic-ref", "HEAD", "refs/heads/main"], &shard_dir);

        let data = tempdir().unwrap();
        let stats = ingest_shard(data.path(), &shard_dir, "linux-wireless", "0", "run-0002")
            .expect("ingest should recover from remote-tracking refs");
        assert_eq!(stats.ingested, 2);

        let out = Command::new("git")
            .args(["symbolic-ref", "HEAD"])
            .current_dir(&shard_dir)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "refs/heads/master"
        );
    }

    #[test]
    fn incremental_skip_on_second_run() {
        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let msgs = sample_messages();
        let msg_refs: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
        make_synthetic_shard(&shard_dir, &msg_refs);

        let data = tempdir().unwrap();
        let first = ingest_shard(data.path(), &shard_dir, "l", "0", "a").unwrap();
        assert_eq!(first.ingested, 2);
        let second = ingest_shard(data.path(), &shard_dir, "l", "0", "b").unwrap();
        assert_eq!(second.ingested, 0);
        assert!(second.parquet_path.is_none());
        let state = State::new(data.path()).unwrap();
        // Generation only bumps on new data.
        assert_eq!(state.generation().unwrap(), 1);
    }

    /// Append two fresh messages to an already-ingested shard and
    /// re-ingest. Generation must bump exactly once; row count must
    /// grow by two; metadata mtime must advance. This models the
    /// common 5-min grokmirror cadence case.
    #[test]
    fn incremental_append_bumps_generation_and_rows() {
        use crate::reader::Reader as CoreReader;

        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let initial = sample_messages();
        let initial_refs: Vec<&[u8]> = initial.iter().map(|m| m.as_slice()).collect();
        make_synthetic_shard(&shard_dir, &initial_refs);

        let data = tempdir().unwrap();
        let first = ingest_shard(data.path(), &shard_dir, "linux-cifs", "0", "r1").unwrap();
        assert_eq!(first.ingested, 2);
        let gen_after_first = State::new(data.path()).unwrap().generation().unwrap();
        assert_eq!(gen_after_first, 1);

        // Append two new messages via a fresh working-tree clone.
        let extra = vec![
            b"From: Carol <carol@example.com>\r\n\
Subject: [PATCH] ksmbd: third patch\r\n\
Date: Mon, 14 Apr 2026 13:00:00 +0000\r\n\
Message-ID: <m3@x>\r\n\
\r\n\
Prose.\r\n"
                .to_vec(),
            b"From: Dave <dave@example.com>\r\n\
Subject: [PATCH] ksmbd: fourth patch\r\n\
Date: Mon, 14 Apr 2026 14:00:00 +0000\r\n\
Message-ID: <m4@x>\r\n\
\r\n\
Prose.\r\n"
                .to_vec(),
        ];
        append_messages_to_bare(&shard_dir, &extra);

        let second = ingest_shard(data.path(), &shard_dir, "linux-cifs", "0", "r2").unwrap();
        assert_eq!(second.ingested, 2, "only the two new commits should ingest");
        let gen_after_second = State::new(data.path()).unwrap().generation().unwrap();
        assert_eq!(
            gen_after_second,
            gen_after_first + 1,
            "generation bumps exactly once on incremental append"
        );

        // Reader sees all four messages without any explicit reload.
        let reader = CoreReader::new(data.path());
        for mid in ["m1@x", "m2@x", "m3@x", "m4@x"] {
            assert!(
                reader.fetch_message(mid).unwrap().is_some(),
                "reader missing {mid:?} after incremental ingest"
            );
        }
    }

    /// If the recorded last-indexed OID is missing from the shard
    /// (typical after public-inbox repack), the walker must fall
    /// through to a full re-walk instead of aborting.
    #[test]
    fn dangling_oid_falls_back_to_full_rewalk() {
        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let msgs = sample_messages();
        let msg_refs: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
        make_synthetic_shard(&shard_dir, &msg_refs);

        let data = tempdir().unwrap();
        let _first = ingest_shard(data.path(), &shard_dir, "l", "0", "a").unwrap();

        // Poison the state: claim we last indexed an OID that doesn't
        // exist in this repo. The ingest path must detect that and
        // fall back to rewalking from HEAD.
        let state = State::new(data.path()).unwrap();
        state
            .save_last_indexed_oid("l", "0", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
            .unwrap();

        let second = ingest_shard(data.path(), &shard_dir, "l", "0", "b").unwrap();
        // Fallback re-walks from HEAD and re-ingests every message.
        // Dedup lives at query time (readers pick the freshest row
        // for a given message_id), not at write time, so public-inbox
        // v2's "message edit" semantics survive. The storage cost is
        // bounded by shard-repack frequency and is negligible at our
        // scale.
        assert_eq!(
            second.ingested, 2,
            "fallback re-walk should re-ingest every commit"
        );
        let oid = state.last_indexed_oid("l", "0").unwrap().unwrap();
        assert_ne!(
            oid, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "OID must advance to the real HEAD after fallback re-walk"
        );
        // Generation bumps on the re-walk (new write happened).
        assert_eq!(
            state.generation().unwrap(),
            2,
            "fallback re-walk counts as one ingest"
        );
    }

    /// Three consecutive ingests with no shard changes: generation
    /// must stay flat after the first. Pins the contract that
    /// grokmirror ticks with no changed shards cost zero writes.
    #[test]
    fn skip_bm25_call_does_not_bump_generation() {
        // Regression: kernel-lore-sync / kernel-lore-ingest pass
        // `skip_bm25 = true` and no shared BM25 writer. Before the
        // caller_orchestrates fix, the internal bump inside
        // ingest_shard_with_bm25 AND the outer bump in the binary
        // both fired — the generation counter advanced by 2 per run,
        // which broke per-tier marker discipline.
        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let owned = sample_messages();
        let refs: Vec<&[u8]> = owned.iter().map(|m| m.as_slice()).collect();
        make_synthetic_shard(&shard_dir, &refs);

        let data = tempdir().unwrap();
        let state = State::new(data.path()).unwrap();
        let gen0 = state.generation().unwrap();

        // Simulate the multi-shard binary's call shape: no shared
        // BM25, skip_bm25=true, no shared store, no over.
        let stats = ingest_shard_with_bm25(
            data.path(),
            &shard_dir,
            "linux-cifs",
            "0",
            "r1",
            None, // shared_bm25
            None, // shared_store
            None, // shared_over
            true, // skip_bm25
        )
        .unwrap();
        assert!(stats.ingested > 0);
        let gen1 = state.generation().unwrap();
        assert_eq!(
            gen1, gen0,
            "internal bump must defer to caller when skip_bm25=true"
        );
    }

    #[test]
    fn idle_ticks_do_not_bump_generation() {
        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let msgs = sample_messages();
        let msg_refs: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
        make_synthetic_shard(&shard_dir, &msg_refs);

        let data = tempdir().unwrap();
        ingest_shard(data.path(), &shard_dir, "l", "0", "a").unwrap();
        ingest_shard(data.path(), &shard_dir, "l", "0", "b").unwrap();
        ingest_shard(data.path(), &shard_dir, "l", "0", "c").unwrap();

        let state = State::new(data.path()).unwrap();
        assert_eq!(
            state.generation().unwrap(),
            1,
            "idle ticks must not bump generation"
        );
    }

    /// Append `messages` as one-per-commit atop an existing bare shard
    /// via an intermediate working clone, then push back. Mimics the
    /// grokmirror delta-packfile path.
    fn append_messages_to_bare(shard_dir: &Path, messages: &[Vec<u8>]) {
        let run = |args: &[&str], cwd: &Path| {
            let out = std::process::Command::new("git")
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
        run(
            &[
                "clone",
                "-q",
                shard_dir.to_str().unwrap(),
                work.path().to_str().unwrap(),
            ],
            Path::new("/"),
        );
        // Let a bare source receive pushes.
        run(
            &["config", "receive.denyCurrentBranch", "updateInstead"],
            shard_dir,
        );
        for (i, msg) in messages.iter().enumerate() {
            std::fs::write(work.path().join("m"), msg).unwrap();
            run(&["add", "m"], work.path());
            run(
                &["commit", "-q", "-m", &format!("appended {i}")],
                work.path(),
            );
        }
        run(&["push", "-q", "origin", "HEAD"], work.path());
    }
}
