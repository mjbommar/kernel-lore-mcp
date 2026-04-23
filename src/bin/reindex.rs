//! `kernel-lore-reindex` — rebuild derived tiers from the local data
//! store without refetching lore.
//!
//! This is the maintenance complement to the live-safe sync path:
//! `kernel-lore-sync` keeps raw metadata, over.db, and optional BM25
//! current; `kernel-lore-reindex` rebuilds slower derived artifacts
//! from the already-downloaded corpus.
//!
//! Defaults:
//!   * `tid`
//!   * `path_vocab`
//!
//! Explicit `--tier bm25` is supported, but a long-lived serving
//! process may need a restart (or a later generation bump) before it
//! reloads the rebuilt Tantivy reader.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};

const REINDEX_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Tid,
    PathVocab,
    Bm25,
}

impl Tier {
    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "tid" => Ok(Self::Tid),
            "path_vocab" | "path-vocab" | "paths" => Ok(Self::PathVocab),
            "bm25" => Ok(Self::Bm25),
            other => Err(anyhow!(
                "unknown tier {other:?}; use tid | path_vocab | bm25"
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Tid => "tid",
            Self::PathVocab => "path_vocab",
            Self::Bm25 => "bm25",
        }
    }
}

#[derive(Default)]
struct Args {
    data_dir: Option<PathBuf>,
    tiers: Vec<String>,
    all: bool,
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
    let tiers = resolve_tiers(&args)?;
    let data_dir = args
        .data_dir
        .or_else(|| std::env::var_os("KLMCP_DATA_DIR").map(PathBuf::from))
        .context("--data-dir or KLMCP_DATA_DIR required")?;
    std::fs::create_dir_all(&data_dir)?;
    let tier_names: Vec<&str> = tiers.iter().map(|t| t.name()).collect();

    let state = _core::State::new(&data_dir)?;
    let _writer_lock = state
        .acquire_writer_lock()
        .context("another writer is running (writer.lock held)")?;
    let corpus_generation = state.generation().context("read corpus generation")?;

    tracing::info!(
        version = REINDEX_VERSION,
        data_dir = %data_dir.display(),
        corpus_generation,
        tiers = ?tier_names,
        "reindex starting"
    );

    let start = Instant::now();
    for tier in tiers {
        let tier_start = Instant::now();
        match tier {
            Tier::Tid => {
                tracing::info!("tid rebuild starting");
                let (path, rows) =
                    _core::rebuild_tid(&data_dir).context("rebuild tid side-table")?;
                state
                    .set_tier_generation("tid", corpus_generation)
                    .context("set tid.generation marker")?;
                tracing::info!(
                    rows,
                    marker_generation = corpus_generation,
                    path = %path.display(),
                    elapsed_secs = tier_start.elapsed().as_secs_f64(),
                    "tid rebuild done"
                );
            }
            Tier::PathVocab => {
                tracing::info!("path vocab rebuild starting");
                let count = _core::path_tier::rebuild_vocab_from_over(&data_dir)
                    .context("rebuild path vocab from over/parquet")?;
                if count > 0 {
                    state
                        .set_tier_generation("path_vocab", corpus_generation)
                        .context("set path_vocab.generation marker")?;
                    tracing::info!(
                        paths = count,
                        marker_generation = corpus_generation,
                        path = %data_dir.join("paths").join("vocab.txt").display(),
                        elapsed_secs = tier_start.elapsed().as_secs_f64(),
                        "path vocab rebuild done"
                    );
                } else {
                    tracing::info!(
                        paths = count,
                        elapsed_secs = tier_start.elapsed().as_secs_f64(),
                        "path vocab rebuild produced an empty vocab; marker left unchanged"
                    );
                }
            }
            Tier::Bm25 => {
                tracing::warn!(
                    "bm25 rebuild starting; restart serving processes or run a later generation bump if you need live readers to reload immediately"
                );
                let docs = _core::rebuild_bm25(&data_dir).context("rebuild bm25 from store")?;
                state
                    .set_tier_generation("bm25", corpus_generation)
                    .context("set bm25.generation marker")?;
                tracing::warn!(
                    docs,
                    marker_generation = corpus_generation,
                    elapsed_secs = tier_start.elapsed().as_secs_f64(),
                    "bm25 rebuild done; marker updated, but long-lived readers may still need a restart to pick up the new index"
                );
            }
        }
    }

    tracing::info!(
        elapsed_secs = start.elapsed().as_secs_f64(),
        corpus_generation,
        tiers = ?tier_names,
        "reindex complete"
    );
    Ok(())
}

fn resolve_tiers(args: &Args) -> Result<Vec<Tier>> {
    let mut out: Vec<Tier> = Vec::new();
    if args.all {
        out.extend([Tier::Tid, Tier::PathVocab, Tier::Bm25]);
    }
    if !args.all && args.tiers.is_empty() {
        out.extend([Tier::Tid, Tier::PathVocab]);
    }
    for raw in &args.tiers {
        let tier = Tier::parse(raw)?;
        if !out.contains(&tier) {
            out.push(tier);
        }
    }
    Ok(out)
}

fn parse_args() -> Result<Args> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data-dir" => args.data_dir = it.next().map(PathBuf::from),
            "--tier" => args.tiers.push(it.next().context("--tier expects a name")?),
            "--all" => args.all = true,
            "--help" | "-h" => {
                println!(
                    "kernel-lore-reindex\n\
                     version: {REINDEX_VERSION}\n\
                     \n\
                     --data-dir PATH       (or $KLMCP_DATA_DIR)\n\
                     --tier NAME           repeatable: tid | path_vocab | bm25\n\
                     --all                 rebuild tid + path_vocab + bm25\n\
                     \n\
                     Defaults:\n\
                     with no --tier flags, rebuilds tid + path_vocab only.\n\
                     \n\
                     Notes:\n\
                     - This command rebuilds from the local corpus; it does NOT refetch lore.\n\
                     - `--tier bm25` updates the on-disk index and marker, but a long-lived\n\
                       serving process may need a restart before it reloads the rebuilt reader.\n"
                );
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("{REINDEX_VERSION}");
                std::process::exit(0);
            }
            other => return Err(anyhow!("unknown arg: {other}")),
        }
    }
    Ok(args)
}
