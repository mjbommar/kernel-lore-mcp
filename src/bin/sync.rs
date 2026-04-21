//! `kernel-lore-sync` — one-shot pull + ingest + generation bump.
//!
//! Internalizes the grokmirror + kernel-lore-ingest two-process
//! pipeline into a single binary that holds the writer lock for the
//! entire update. See `docs/plans/2026-04-15-internalize-grokmirror.md`
//! for motivation.
//!
//! Pipeline:
//!   1. HTTP GET `<manifest_url>` (default lore.kernel.org).
//!   2. Diff against the local manifest cache; subset = shards whose
//!      fingerprint changed (or that are absent locally).
//!   3. For each changed shard, gix clone-or-fetch in parallel.
//!   4. For each fetched shard, ingest via the existing
//!      `ingest_shard_with_bm25` under a single writer lock.
//!   5. Rebuild tid side-table once at the end, bump generation once.
//!   6. Persist the fresh manifest cache (only on full success, so a
//!      partial-failure rerun re-fetches the same shards).
//!
//! Exit codes:
//!   0  success
//!   1  CLI / config error
//!   2  partial failure (some shards fetched or ingested with errors)
//!   3  manifest fetch failure (upstream unreachable)
//!
//! Usage:
//!   KLMCP_DATA_DIR=/var/klmcp/data kernel-lore-sync
//!   kernel-lore-sync --data-dir /var/klmcp/data --include '/lkml/*'
//!   kernel-lore-sync --dry-run  # manifest fetch + diff only
//!
//! Stays single-process: the writer lock taken here covers manifest
//! fetch, gix fetch, ingest, tid rebuild, and the generation bump,
//! so a concurrent sync invocation fails the flock and exits cleanly
//! without touching state.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use rayon::prelude::*;

use _core::sync::{
    DEFAULT_MANIFEST_URL, FetchOutcome, diff_manifest, fetch_manifest, fetch_shard,
    load_local_manifest, save_local_manifest, shard_local_path,
};

/// One changed shard we intend to pull + ingest.
#[derive(Debug, Clone)]
struct ChangedShard {
    /// Manifest key, e.g. `/netdev/git/0.git`.
    manifest_path: String,
    /// `list` segment — first non-empty path component.
    list: String,
    /// Shard number as a string (`"0"` for public-inbox v2). For
    /// single-shard v1 layouts (`/<list>.git`), this is `"0"` too.
    shard: String,
    /// On-disk bare-repo path under `<data_dir>/shards/...`.
    local_path: PathBuf,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .init();

    let args = parse_args()?;
    let data_dir = args
        .data_dir
        .or_else(|| std::env::var_os("KLMCP_DATA_DIR").map(PathBuf::from))
        .context("--data-dir or KLMCP_DATA_DIR required")?;
    std::fs::create_dir_all(&data_dir)?;

    let manifest_url = args
        .manifest_url
        .clone()
        .or_else(|| std::env::var("KLMCP_MANIFEST_URL").ok())
        .unwrap_or_else(|| DEFAULT_MANIFEST_URL.to_string());

    let start = Instant::now();
    tracing::info!(
        data_dir = %data_dir.display(),
        manifest_url = manifest_url,
        include = ?args.include,
        exclude = ?args.exclude,
        dry_run = args.dry_run,
        "sync starting"
    );

    // Step 1: manifest fetch. Bail with exit=3 if unreachable —
    // distinguishes network failure from partial fetch/ingest failure
    // so systemd `OnFailure=` can alert differently.
    let remote = match fetch_manifest(&manifest_url) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "manifest fetch failed");
            std::process::exit(3);
        }
    };
    tracing::info!(shards = remote.len(), "manifest fetched");

    // Step 2: diff against local cache.
    let local = load_local_manifest(&data_dir).context("load local manifest cache")?;
    let changed_paths = diff_manifest(&remote, &local, &args.include, &args.exclude);
    tracing::info!(
        changed = changed_paths.len(),
        tracked_local = local.len(),
        "diff complete"
    );

    if args.dry_run {
        // Print a stable list to stdout for scripts / humans; structured
        // log line already went to stderr.
        for p in &changed_paths {
            println!("{p}");
        }
        return Ok(());
    }

    if changed_paths.is_empty() {
        tracing::info!(
            elapsed_secs = start.elapsed().as_secs_f64(),
            "sync complete (no-op)"
        );
        return Ok(());
    }

    // Step 3+: everything past here mutates state. Take the writer
    // lock once for the whole pipeline.
    let state = _core::State::new(&data_dir)?;
    let _writer_lock = state
        .acquire_writer_lock()
        .context("another writer is running (writer.lock held)")?;

    // Resolve manifest paths to ChangedShard descriptors.
    let changed: Vec<ChangedShard> = changed_paths
        .iter()
        .filter_map(|p| ChangedShard::from_manifest_path(&data_dir, p))
        .collect();
    if changed.len() != changed_paths.len() {
        tracing::warn!(
            dropped = changed_paths.len() - changed.len(),
            "some manifest paths were unparseable; skipping those shards"
        );
    }

    // Step 3: parallel gix fetch. rayon's default pool (one thread per
    // core) is the right fan-out for a network-bound workload too —
    // the per-shard fetches are independent and the bandwidth budget
    // is lore's not ours.
    let fetch_results: Vec<Result<FetchOutcome, String>> = changed
        .par_iter()
        .map(|sh| {
            fetch_shard(&data_dir, &sh.manifest_path, &manifest_url)
                .map_err(|e| format!("{}: {e}", sh.manifest_path))
        })
        .collect();
    let mut fetch_failed: Vec<&ChangedShard> = Vec::new();
    let mut fetched_ok: Vec<&ChangedShard> = Vec::new();
    for (sh, res) in changed.iter().zip(fetch_results.iter()) {
        match res {
            Ok(outcome) => {
                tracing::info!(
                    manifest_path = sh.manifest_path,
                    local = %sh.local_path.display(),
                    outcome = ?outcome,
                    "shard fetched"
                );
                fetched_ok.push(sh);
            }
            Err(e) => {
                tracing::error!(
                    manifest_path = sh.manifest_path,
                    error = %e,
                    "shard fetch failed"
                );
                fetch_failed.push(sh);
            }
        }
    }

    tracing::info!(
        fetched = fetched_ok.len(),
        failed = fetch_failed.len(),
        "fetch phase done"
    );

    if fetched_ok.is_empty() {
        tracing::warn!("no shards fetched successfully; skipping ingest");
        std::process::exit(if fetch_failed.is_empty() { 0 } else { 2 });
    }

    // Step 4: orchestrate ingest over successfully-fetched shards.
    // Mirrors bin/ingest.rs's orchestration but scoped to the changed
    // subset — no whole-corpus rescan per sync tick.
    let run_id = args.run_id.unwrap_or_else(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("sync-{now}")
    });

    let skip_bm25 = !args.with_bm25;
    if skip_bm25 {
        tracing::info!(
            "BM25 deferred (default). Run `kernel-lore-ingest --rebuild-bm25` after sync if needed."
        );
    }

    let over_path = data_dir.join("over.db");
    let with_over = match (args.with_over, args.no_over) {
        (true, true) => anyhow::bail!("--with-over and --no-over are mutually exclusive"),
        (true, false) => true,
        (false, true) => false,
        (false, false) => over_path.exists(),
    };

    let bm25 = if !skip_bm25 {
        Some(Mutex::new(
            _core::BmWriter::open(&data_dir).context("open BM25 writer")?,
        ))
    } else {
        None
    };
    let over = if with_over {
        Some(Mutex::new(
            _core::OverDb::open(&over_path).context("open over.db")?,
        ))
    } else {
        None
    };

    // Shared per-list Store. Same invariant as bin/ingest.rs: every
    // shard in a list serializes its segment-append through one
    // SegmentWriter so offsets stay monotonic.
    let mut stores: std::collections::HashMap<String, Mutex<_core::Store>> =
        std::collections::HashMap::new();
    for sh in &fetched_ok {
        stores.entry(sh.list.clone()).or_insert_with(|| {
            Mutex::new(
                _core::Store::open(&data_dir, &sh.list).expect("failed to open store for list"),
            )
        });
    }

    let max_retries = args.max_retries;
    tracing::info!(
        shards = fetched_ok.len(),
        lists = stores.len(),
        with_over,
        with_bm25 = !skip_bm25,
        "ingest phase starting"
    );
    let ingest_phase_start = Instant::now();
    let ingest_results: Vec<(&ChangedShard, Result<_core::IngestStats>, f64)> = fetched_ok
        .par_iter()
        .map(|sh| {
            let shard_start = Instant::now();
            let per_run_id = format!("{run_id}-{}-{}", sh.list, sh.shard);
            let shared_store = stores.get(&sh.list).expect("store for list must exist");
            let shared_bm25 = bm25.as_ref();
            let mut last_err: Option<anyhow::Error> = None;
            for attempt in 0..=max_retries {
                if attempt > 0 {
                    let backoff = std::time::Duration::from_secs(1 << attempt.min(5));
                    tracing::warn!(
                        list = sh.list,
                        shard = sh.shard,
                        attempt,
                        backoff_secs = backoff.as_secs(),
                        "retrying failed shard"
                    );
                    std::thread::sleep(backoff);
                }
                match _core::ingest_shard_with_bm25(
                    &data_dir,
                    &sh.local_path,
                    &sh.list,
                    &sh.shard,
                    &per_run_id,
                    shared_bm25,
                    Some(shared_store),
                    over.as_ref(),
                    skip_bm25,
                ) {
                    Ok(stats) => {
                        let elapsed = shard_start.elapsed().as_secs_f64();
                        return (*sh, Ok(stats), elapsed);
                    }
                    Err(e) => {
                        tracing::warn!(
                            list = sh.list,
                            shard = sh.shard,
                            attempt,
                            error = %e,
                            "shard ingest attempt failed"
                        );
                        last_err = Some(anyhow::Error::from(e));
                    }
                }
            }
            (
                *sh,
                Err(last_err.unwrap_or_else(|| anyhow!("unknown ingest failure"))),
                shard_start.elapsed().as_secs_f64(),
            )
        })
        .collect();
    tracing::info!(
        elapsed_secs = ingest_phase_start.elapsed().as_secs_f64(),
        "ingest phase done"
    );

    // Commit BM25 once, after all shards finish.
    if let Some(ref bm25_mutex) = bm25 {
        let mut w = bm25_mutex
            .lock()
            .map_err(|_| anyhow!("bm25 writer mutex poisoned"))?;
        w.commit().context("bm25 commit")?;
    }

    // tid rebuild: reasonable to do every sync tick; touches only the
    // subset of rows whose tid changed. Cheap at steady state.
    let tid_result = _core::rebuild_tid(&data_dir).context("rebuild tid side-table")?;
    tracing::info!(
        rows = tid_result.1,
        path = %tid_result.0.display(),
        "tid rebuild done"
    );

    // Tally.
    let mut total_ingested: u64 = 0;
    let mut total_failed: u64 = 0;
    let mut total_over_rows: u64 = 0;
    let mut total_over_failed: u64 = 0;
    let mut successful_paths: Vec<&str> = Vec::new();
    for (sh, res, elapsed) in &ingest_results {
        match res {
            Ok(stats) => {
                total_ingested += stats.ingested;
                total_over_rows += stats.over_rows_written;
                if stats.over_failed {
                    total_over_failed += 1;
                }
                successful_paths.push(&sh.manifest_path);
                tracing::info!(
                    list = sh.list,
                    shard = sh.shard,
                    ingested = stats.ingested,
                    skipped_no_m = stats.skipped_no_m,
                    skipped_empty = stats.skipped_empty,
                    skipped_no_mid = stats.skipped_no_mid,
                    over_rows = stats.over_rows_written,
                    over_failed = stats.over_failed,
                    elapsed_secs = elapsed,
                    "shard done"
                );
            }
            Err(e) => {
                total_failed += 1;
                tracing::error!(
                    list = sh.list,
                    shard = sh.shard,
                    error = %e,
                    elapsed_secs = elapsed,
                    "shard failed"
                );
            }
        }
    }

    let new_gen = state.bump_generation().context("bump generation")?;
    tracing::info!(generation = new_gen, "generation bumped");

    // Per-tier markers. Same discipline as bin/ingest.rs: only
    // advance the `over` marker on full success so readers bypass
    // over.db on drift.
    if with_over && total_over_failed == 0 {
        state
            .set_tier_generation("over", new_gen)
            .context("set over.generation marker")?;
    } else if with_over {
        tracing::warn!(
            over_failed_shards = total_over_failed,
            corpus_gen = new_gen,
            "over.generation marker NOT advanced"
        );
    }
    state
        .set_tier_generation("bm25", new_gen)
        .context("set bm25.generation marker")?;
    state
        .set_tier_generation("trigram", new_gen)
        .context("set trigram.generation marker")?;
    state
        .set_tier_generation("tid", new_gen)
        .context("set tid.generation marker")?;

    // Persist manifest cache — start from the existing local entries
    // (so filtered-out shards keep their history) and overwrite only
    // the paths that both fetched AND ingested successfully this run.
    // Fetch-failed or ingest-failed shards keep their old (or absent)
    // cache entry so the next sync re-tries them.
    let mut updated_local = local;
    for path in &successful_paths {
        if let Some(entry) = remote.get(*path) {
            updated_local.insert((*path).to_string(), entry.clone());
        }
    }
    save_local_manifest(&data_dir, &updated_local).context("save manifest cache")?;

    tracing::info!(
        elapsed_secs = start.elapsed().as_secs_f64(),
        changed = changed.len(),
        fetched = fetched_ok.len(),
        ingested_shards = ingest_results.len() - total_failed as usize,
        failed_shards = total_failed,
        ingested_msgs = total_ingested,
        over_rows = total_over_rows,
        over_failed_shards = total_over_failed,
        "sync complete"
    );

    if total_failed > 0 || !fetch_failed.is_empty() || total_over_failed > 0 {
        std::process::exit(2);
    }
    Ok(())
}

impl ChangedShard {
    /// Parse a manifest key like `/<list>/git/<N>.git` (public-inbox
    /// v2) or `/<list>.git` (v1) into a ChangedShard. Returns `None`
    /// if the shape doesn't match — the caller logs and skips.
    fn from_manifest_path(data_dir: &Path, path: &str) -> Option<Self> {
        let trimmed = path.trim_start_matches('/');
        let parts: Vec<&str> = trimmed.split('/').collect();
        let (list, shard): (String, String) = match parts.as_slice() {
            // /<list>/git/<N>.git
            [list, "git", shard] if shard.ends_with(".git") => (
                (*list).to_string(),
                shard.trim_end_matches(".git").to_string(),
            ),
            // /<list>.git (single-shard v1)
            [only] if only.ends_with(".git") => {
                (only.trim_end_matches(".git").to_string(), "0".to_string())
            }
            _ => return None,
        };
        if list.is_empty() {
            return None;
        }
        Some(Self {
            manifest_path: path.to_string(),
            list,
            shard,
            local_path: shard_local_path(data_dir, path),
        })
    }
}

#[derive(Default)]
struct Args {
    data_dir: Option<PathBuf>,
    manifest_url: Option<String>,
    include: Vec<String>,
    exclude: Vec<String>,
    run_id: Option<String>,
    with_bm25: bool,
    with_over: bool,
    no_over: bool,
    max_retries: u32,
    dry_run: bool,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        max_retries: 3,
        ..Args::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data-dir" => args.data_dir = it.next().map(PathBuf::from),
            "--manifest-url" => args.manifest_url = it.next(),
            "--include" => args
                .include
                .push(it.next().context("--include expects a pattern")?),
            "--exclude" => args
                .exclude
                .push(it.next().context("--exclude expects a pattern")?),
            "--run-id" => args.run_id = it.next(),
            "--with-bm25" => args.with_bm25 = true,
            "--with-over" => args.with_over = true,
            "--no-over" => args.no_over = true,
            "--max-retries" => {
                args.max_retries = it.next().and_then(|s| s.parse().ok()).unwrap_or(3);
            }
            "--dry-run" => args.dry_run = true,
            "--help" | "-h" => {
                println!(
                    "kernel-lore-sync\n\
                     \n\
                     --data-dir PATH       (or $KLMCP_DATA_DIR)\n\
                     --manifest-url URL    (or $KLMCP_MANIFEST_URL;\n\
                                            default: {DEFAULT_MANIFEST_URL})\n\
                     --include PATTERN     fnmatch; repeatable (default: all)\n\
                     --exclude PATTERN     fnmatch; repeatable\n\
                     --run-id STRING       stable id for this run\n\
                     --with-bm25           build BM25 inline\n\
                     --with-over           force on over.db writes\n\
                     --no-over             force off over.db writes\n\
                                           (default: on iff <data_dir>/over.db exists)\n\
                     --max-retries N       per-shard ingest retry count (default: 3)\n\
                     --dry-run             fetch manifest + diff, don't touch shards\n"
                );
                std::process::exit(0);
            }
            other => {
                return Err(anyhow!("unknown arg: {other}"));
            }
        }
    }
    Ok(args)
}
