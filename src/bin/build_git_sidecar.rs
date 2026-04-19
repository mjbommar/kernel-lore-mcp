//! `kernel-lore-build-git-sidecar` — ingest commits from a git repo
//! into the sidecar SQLite (`<data_dir>/git_sidecar.db`).
//!
//! Purpose: give feature tools (stable_backport, thread_state,
//! subsystem_churn, file_timeline) a deterministic answer to "is
//! this patch / SHA in mainline (or linux-stable, or …) git tree?"
//! — lore data alone can't answer that.
//!
//! Scope (v1):
//!   - Extracts (sha, subject, author_email, author_date_ns).
//!   - `patch_id` is left NULL. Computing `git patch-id --stable`
//!     over 1.5M commits is hours of I/O and deferred to v2. For
//!     v1 we rely on subject+author+date-window matching, which
//!     covers ~60-70% of the `b4` cascade.
//!   - Incremental: if a tip is recorded for this repo, walk with
//!     `with_hidden([tip])` to see only new commits.
//!
//! Usage:
//!   kernel-lore-build-git-sidecar \
//!       --data-dir /var/klmcp/data \
//!       --repo linux \
//!       --path /var/lib/git/torvalds-linux.git
//!
//! The `--repo` label is a freeform key (`linux`, `linux-stable`,
//! `net-next`, …) used as the `commits.repo` column.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use gix::ObjectId;
use _core::{CommitRecord, GitSidecar};

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
    let repo_label = args.repo.context("--repo label required")?;
    let repo_path = args.path.context("--path to git repo required")?;

    std::fs::create_dir_all(&data_dir)?;
    let sidecar_path = _core::git_sidecar_path(&data_dir);
    let mut sidecar = GitSidecar::open(&sidecar_path).context("open git_sidecar.db")?;

    let last_tip = sidecar.tip(&repo_label)?;
    if let Some(ref t) = last_tip {
        tracing::info!(repo = repo_label, last_tip = %t, "incremental walk");
    } else {
        tracing::info!(repo = repo_label, "first ingest (full walk)");
    }

    let mut repo = gix::open(&repo_path)
        .with_context(|| format!("open {}", repo_path.display()))?;
    repo.object_cache_size(256 * 1024 * 1024);

    let head_id: ObjectId = repo.head_id().context("head_id")?.detach();
    let head_hex = head_id.to_string();

    let mut platform = repo.rev_walk([head_id]);
    if let Some(ref tip_hex) = last_tip
        && let Ok(parsed) = tip_hex.parse::<ObjectId>()
        && repo.find_object(parsed).is_ok()
    {
        platform = platform.with_hidden([parsed]);
    }
    let walk = platform.all().context("rev_walk")?;

    let start = Instant::now();
    let mut batch: Vec<CommitRecord> = Vec::with_capacity(4096);
    let mut total: u64 = 0;

    for info in walk {
        let info = info.context("rev_walk entry")?;
        let commit = repo
            .find_object(info.id)
            .context("find commit")?
            .try_into_commit()
            .context("not a commit")?;
        let message_ref = commit.message().context("commit message")?;
        let subject = message_ref
            .summary()
            .to_string()
            .trim()
            .to_owned();
        let author = commit.author().context("author")?;
        let email = author.email.to_string().to_ascii_lowercase();
        // SignatureRef::seconds() parses the `time` field and returns
        // unix epoch seconds (or 0 on parse error). Convert to ns.
        let author_date_ns = (author.seconds() as i64).saturating_mul(1_000_000_000);
        batch.push(CommitRecord {
            repo: repo_label.clone(),
            sha: info.id.to_string(),
            subject,
            author_email: email,
            author_date_ns,
            patch_id: None,
        });
        if batch.len() >= 4096 {
            total += sidecar.insert_batch(&batch)?;
            batch.clear();
            if total % 40_000 == 0 {
                tracing::info!(
                    repo = repo_label,
                    ingested = total,
                    elapsed_secs = start.elapsed().as_secs_f64(),
                    "progress"
                );
            }
        }
    }
    if !batch.is_empty() {
        total += sidecar.insert_batch(&batch)?;
    }

    sidecar.set_tip(&repo_label, &head_hex)?;

    tracing::info!(
        repo = repo_label,
        ingested = total,
        new_tip = %head_hex,
        elapsed_secs = start.elapsed().as_secs_f64(),
        "ingest complete"
    );
    Ok(())
}

#[derive(Default)]
struct Args {
    data_dir: Option<PathBuf>,
    repo: Option<String>,
    path: Option<PathBuf>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data-dir" => args.data_dir = it.next().map(PathBuf::from),
            "--repo" => args.repo = it.next(),
            "--path" => args.path = it.next().map(PathBuf::from),
            "--help" | "-h" => {
                println!(
                    "kernel-lore-build-git-sidecar\n\n\
                     --data-dir PATH   (or $KLMCP_DATA_DIR)\n\
                     --repo LABEL      freeform key (linux, linux-stable, ...)\n\
                     --path PATH       path to the git repo / bare repo\n\
                     --help            this message\n"
                );
                std::process::exit(0);
            }
            other => {
                anyhow::bail!("unknown arg: {other}");
            }
        }
    }
    Ok(args)
}

fn _unused(_: &Path) {}
