//! `kernel-lore-ingest` — walk a grokmirror-managed lore mirror and
//! ingest every shard into `<data_dir>`.
//!
//! Usage:
//!     KLMCP_DATA_DIR=/var/klmcp/data \
//!     KLMCP_LORE_MIRROR_DIR=/var/lore-mirror \
//!     kernel-lore-ingest
//!
//! Or with explicit args:
//!     kernel-lore-ingest --data-dir /var/klmcp/data \
//!                        --lore-mirror /var/lore-mirror \
//!                        --list linux-cifs \
//!                        --run-id run-2026-04-14T19-00
//!
//! The binary walks `<lore_mirror>/<list>/git/<N>.git` for every
//! `<list>` directory under the mirror root (or the one specified via
//! `--list`), and calls `kernel_lore_mcp::ingest_shard` for each.
//! Rayon parallelizes across shards; each shard's writer holds the
//! per-data_dir flock for its segment of the run.
//!
//! Structured log lines stream to stderr via `tracing`. One-shot; no
//! daemonization. Cron invokes it periodically.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result};
use rayon::prelude::*;

/// Discovered public-inbox v2 shard: one `<list>/git/<N>.git` directory.
#[derive(Debug, Clone)]
struct ShardRef {
    list: String,
    shard: String,
    path: PathBuf,
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

    let args = parse_args();
    let data_dir = args
        .data_dir
        .or_else(|| std::env::var_os("KLMCP_DATA_DIR").map(PathBuf::from))
        .context("--data-dir or KLMCP_DATA_DIR required")?;

    std::fs::create_dir_all(&data_dir)?;

    // --rebuild-bm25: standalone BM25 rebuild from existing store.
    // Doesn't need --lore-mirror or the writer lock (reads only).
    if args.rebuild_bm25_only {
        tracing::info!(data_dir = %data_dir.display(), "rebuilding BM25 from store");
        let start = Instant::now();
        let count = _core::rebuild_bm25(&data_dir).context("rebuild_bm25 failed")?;
        tracing::info!(
            docs = count,
            elapsed_secs = start.elapsed().as_secs_f64(),
            "BM25 rebuild complete"
        );
        return Ok(());
    }

    let lore_mirror = args
        .lore_mirror
        .or_else(|| std::env::var_os("KLMCP_LORE_MIRROR_DIR").map(PathBuf::from))
        .context("--lore-mirror or KLMCP_LORE_MIRROR_DIR required")?;

    std::fs::create_dir_all(&data_dir)?;

    let run_id = args.run_id.unwrap_or_else(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("run-{now}")
    });

    let shards = discover_shards(&lore_mirror, args.list.as_deref())?;
    tracing::info!(
        shards = shards.len(),
        data_dir = %data_dir.display(),
        lore_mirror = %lore_mirror.display(),
        run_id,
        "ingest starting"
    );

    // Acquire the writer lock ONCE for the whole run; rayon fan-out
    // then shares it across shards.
    let state = _core::State::new(&data_dir)?;
    let _writer_lock = state
        .acquire_writer_lock()
        .context("another ingest process is running (writer.lock held)")?;

    let skip_bm25 = !args.with_bm25;
    if skip_bm25 {
        tracing::info!(
            "BM25 deferred (default). Run --rebuild-bm25 after ingest to build the prose index."
        );
    }

    // over.db tier: default-enabled if `<data_dir>/over.db` already
    // exists (so re-ingests over an existing deployment keep it
    // fresh) and default-disabled otherwise (don't surprise the
    // first run with a 12-18 GB sidecar). Explicit --with-over /
    // --no-over override the auto-detect.
    let over_path = data_dir.join("over.db");
    let with_over = match (args.with_over, args.no_over) {
        (true, true) => {
            anyhow::bail!("--with-over and --no-over are mutually exclusive");
        }
        (true, false) => true,
        (false, true) => false,
        (false, false) => over_path.exists(),
    };
    if with_over {
        tracing::info!(
            over_db = %over_path.display(),
            "over.db incremental writes enabled"
        );
    } else {
        tracing::info!("over.db incremental writes disabled (use --with-over to enable)");
    }

    // Single shared BM25 writer for the whole run. Only opened when
    // --with-bm25 is set; otherwise skipped entirely for speed.
    let bm25 = if !skip_bm25 {
        Some(Mutex::new(
            _core::BmWriter::open(&data_dir).context("open BM25 writer")?,
        ))
    } else {
        None
    };

    // Single shared OverDb for the whole run; rayon shards serialize
    // their per-shard insert_batch through this mutex. SQLite WAL
    // tolerates this fine — there's exactly one writer (us) and
    // readers (the MCP server) are non-blocking.
    let over = if with_over {
        Some(Mutex::new(
            _core::OverDb::open(&over_path).context("open over.db")?,
        ))
    } else {
        None
    };

    // One shared Store per list. When a list has multiple shards
    // (e.g. lkml has 19), every shard in that list MUST serialize
    // its store appends through the same SegmentWriter so the
    // offset counter stays correct. Without this, parallel shards
    // from the same list produce metadata with stale offsets.
    let mut stores: std::collections::HashMap<String, Mutex<_core::Store>> =
        std::collections::HashMap::new();
    for shard in &shards {
        stores.entry(shard.list.clone()).or_insert_with(|| {
            Mutex::new(
                _core::Store::open(&data_dir, &shard.list).expect("failed to open store for list"),
            )
        });
    }

    let start = Instant::now();
    let max_retries = args.max_retries;
    let totals = shards
        .par_iter()
        .map(|shard| {
            ingest_one(
                &data_dir,
                shard,
                &run_id,
                &bm25,
                &stores,
                over.as_ref(),
                skip_bm25,
                max_retries,
            )
        })
        .collect::<Vec<_>>();

    // Commit BM25 once, after all shards finish (only if BM25 was built).
    if let Some(ref bm25_mutex) = bm25 {
        let mut w = bm25_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("bm25 writer mutex poisoned"))?;
        w.commit().context("bm25 commit")?;
    }

    // Rebuild the tid side-table over the entire metadata corpus.
    // Cheap relative to ingest; runs after every multi-shard run so
    // cover-letters always carry their patches' touched_files.
    let tid_result = _core::rebuild_tid(&data_dir).context("rebuild tid side-table")?;
    tracing::info!(
        rows = tid_result.1,
        path = %tid_result.0.display(),
        "tid rebuild done"
    );

    // Bump generation ONCE, after BM25 commit + tid rebuild, so
    // readers never see an inconsistent snapshot. Individual
    // ingest_shard_with_bm25 calls skip the bump when a shared
    // BM25 writer is in use.
    let new_gen = state.bump_generation().context("bump generation")?;
    tracing::info!(generation = new_gen, "generation bumped");

    let mut total_ingested: u64 = 0;
    let mut total_failed: u64 = 0;
    let mut total_over_rows: u64 = 0;
    let mut total_over_failed: u64 = 0;
    for (shard, result) in shards.iter().zip(totals.iter()) {
        match result {
            Ok(stats) => {
                total_ingested += stats.ingested;
                total_over_rows += stats.over_rows_written;
                if stats.over_failed {
                    total_over_failed += 1;
                }
                tracing::info!(
                    list = shard.list,
                    shard = shard.shard,
                    ingested = stats.ingested,
                    skipped_no_m = stats.skipped_no_m,
                    skipped_empty = stats.skipped_empty,
                    skipped_no_mid = stats.skipped_no_mid,
                    parquet = ?stats.parquet_path,
                    over_rows = stats.over_rows_written,
                    over_failed = stats.over_failed,
                    "shard done"
                );
            }
            Err(e) => {
                total_failed += 1;
                tracing::error!(
                    list = shard.list,
                    shard = shard.shard,
                    error = %e,
                    "shard failed"
                );
            }
        }
    }

    tracing::info!(
        elapsed_secs = start.elapsed().as_secs_f64(),
        shards = shards.len(),
        failed = total_failed,
        ingested = total_ingested,
        over_rows = total_over_rows,
        over_failed_shards = total_over_failed,
        "ingest complete"
    );

    if total_failed > 0 || total_over_failed > 0 {
        std::process::exit(2);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn ingest_one(
    data_dir: &Path,
    shard: &ShardRef,
    run_id: &str,
    bm25: &Option<Mutex<_core::BmWriter>>,
    stores: &std::collections::HashMap<String, Mutex<_core::Store>>,
    over: Option<&Mutex<_core::OverDb>>,
    skip_bm25: bool,
    max_retries: u32,
) -> Result<_core::IngestStats> {
    let per_shard_run_id = format!("{run_id}-{}-{}", shard.list, shard.shard);
    let shared_store = stores
        .get(&shard.list)
        .ok_or_else(|| anyhow::anyhow!("no shared store for list {:?}", shard.list))?;
    let shared_bm25 = bm25.as_ref();

    let mut last_err = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = std::time::Duration::from_secs(1 << attempt.min(5));
            tracing::warn!(
                list = shard.list,
                shard = shard.shard,
                attempt,
                backoff_secs = backoff.as_secs(),
                "retrying failed shard"
            );
            std::thread::sleep(backoff);
        }

        match _core::ingest_shard_with_bm25(
            data_dir,
            &shard.path,
            &shard.list,
            &shard.shard,
            &per_shard_run_id,
            shared_bm25,
            Some(shared_store),
            over,
            skip_bm25,
        ) {
            Ok(stats) => {
                if attempt > 0 {
                    tracing::info!(
                        list = shard.list,
                        shard = shard.shard,
                        attempt,
                        "shard succeeded on retry"
                    );
                }
                return Ok(stats);
            }
            Err(e) => {
                tracing::warn!(
                    list = shard.list,
                    shard = shard.shard,
                    attempt,
                    error = %e,
                    error_chain = ?e,
                    "shard attempt failed"
                );
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap()).with_context(|| {
        format!(
            "ingest_shard failed for {}/{} at {} after {} retries",
            shard.list,
            shard.shard,
            shard.path.display(),
            max_retries,
        )
    })
}

fn discover_shards(mirror_root: &Path, only_list: Option<&str>) -> Result<Vec<ShardRef>> {
    let mut out = Vec::new();
    let read = std::fs::read_dir(mirror_root)
        .with_context(|| format!("read_dir {}", mirror_root.display()))?;
    for entry in read {
        let entry = entry?;
        let ftype = entry.file_type()?;
        if !ftype.is_dir() {
            continue;
        }
        let list = entry.file_name().to_string_lossy().into_owned();
        if let Some(want) = only_list {
            if list != want {
                continue;
            }
        }
        let git_dir = entry.path().join("git");
        if !git_dir.is_dir() {
            continue;
        }
        for shard_entry in std::fs::read_dir(&git_dir)? {
            let shard_entry = shard_entry?;
            let name = shard_entry.file_name().to_string_lossy().into_owned();
            let Some(num) = name.strip_suffix(".git") else {
                continue;
            };
            let path = shard_entry.path();
            if !path.is_dir() {
                continue;
            }
            out.push(ShardRef {
                list: list.clone(),
                shard: num.to_owned(),
                path,
            });
        }
    }
    // Deterministic order; rayon will still parallelize.
    out.sort_by(|a, b| (&a.list, &a.shard).cmp(&(&b.list, &b.shard)));
    Ok(out)
}

#[derive(Default)]
struct Args {
    data_dir: Option<PathBuf>,
    lore_mirror: Option<PathBuf>,
    list: Option<String>,
    run_id: Option<String>,
    with_bm25: bool,
    rebuild_bm25_only: bool,
    max_retries: u32,
    with_over: bool,
    no_over: bool,
}

fn parse_args() -> Args {
    // Minimal arg parser to avoid pulling in clap. The CLI surface is
    // tiny and stable.
    let mut args = Args {
        // default: retry each failed shard up to 3 times
        max_retries: 3,
        ..Args::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data-dir" => args.data_dir = it.next().map(PathBuf::from),
            "--lore-mirror" => args.lore_mirror = it.next().map(PathBuf::from),
            "--list" => args.list = it.next(),
            "--run-id" => args.run_id = it.next(),
            "--max-retries" => {
                args.max_retries = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(3);
            }
            "--with-bm25" => args.with_bm25 = true,
            "--rebuild-bm25" => args.rebuild_bm25_only = true,
            "--with-over" => args.with_over = true,
            "--no-over" => args.no_over = true,
            "--help" | "-h" => {
                println!(
                    "kernel-lore-ingest\n\
                     \n\
                     --data-dir PATH       (or $KLMCP_DATA_DIR)\n\
                     --lore-mirror PATH    (or $KLMCP_LORE_MIRROR_DIR)\n\
                     --list NAME           optional: restrict to one list\n\
                     --run-id STRING       optional: stable id for this run\n\
                     --max-retries N       retry failed shards N times (default: 3)\n\
                     --with-bm25           build BM25 inline (slower; default: skip)\n\
                     --rebuild-bm25        ONLY rebuild BM25 from existing store, then exit\n\
                     --with-over           force on incremental over.db writes\n\
                     --no-over             force off incremental over.db writes\n\
                                           (default: on iff <data_dir>/over.db exists)\n"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }
    args
}
