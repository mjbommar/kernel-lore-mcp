use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use git2::Repository as Git2Repository;
use rusqlite::Connection;
use serde::Serialize;

const DOCTOR_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Default)]
struct Args {
    data_dir: Option<PathBuf>,
    json: bool,
    heal: bool,
    clean_manifest_cache: bool,
}

#[derive(Debug, Default, Serialize)]
struct TierHealth {
    manifest_cache_present: bool,
    generation: Option<u64>,
    last_ingest_unix_secs: Option<u64>,
    last_ingest_age_seconds: Option<u64>,
    metadata_ready: bool,
    over_db_ready: bool,
    over_db_open_ok: bool,
    bm25_ready: bool,
    trigram_ready: bool,
    tid_ready: bool,
    tid_parquet_present: bool,
    path_vocab_ready: bool,
    embedding_ready: bool,
    maintainers_ready: bool,
    git_sidecar_ready: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RepairableShard {
    path: String,
    source_ref: String,
    repair_ref: String,
    head_oid: String,
}

#[derive(Debug, Clone, Serialize)]
struct BrokenShard {
    path: String,
    reason: String,
}

#[derive(Debug, Default, Serialize)]
struct DoctorSummary {
    version: String,
    data_dir: String,
    tiers: TierHealth,
    shard_repos_total: usize,
    shard_repos_healthy: usize,
    shard_repos_repairable: usize,
    shard_repos_broken: usize,
    repaired_heads: Vec<RepairableShard>,
    removed_broken_shards: Vec<String>,
    repairable_shards: Vec<RepairableShard>,
    broken_shards: Vec<BrokenShard>,
    notes: Vec<String>,
}

#[derive(Debug, Clone)]
struct HeadCandidate {
    source_ref: String,
    repair_ref: String,
    oid: git2::Oid,
    rank: usize,
}

#[derive(Debug)]
enum RepoHealth {
    Healthy,
    Repairable(HeadCandidate),
    Broken(String),
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let data_dir = args
        .data_dir
        .clone()
        .or_else(|| std::env::var_os("KLMCP_DATA_DIR").map(PathBuf::from))
        .context("--data-dir or KLMCP_DATA_DIR required")?;

    let summary = inspect_data_dir(&data_dir, &args)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).context("serialize doctor summary")?
        );
    } else {
        print_human(&summary);
    }

    if has_severe_issues(&summary) {
        std::process::exit(2);
    }
    Ok(())
}

fn parse_args() -> Result<Args> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data-dir" => args.data_dir = it.next().map(PathBuf::from),
            "--json" => args.json = true,
            "--heal" => args.heal = true,
            "--clean-manifest-cache" => args.clean_manifest_cache = true,
            "--help" | "-h" => {
                println!(
                    "kernel-lore-doctor\n\
                     version: {DOCTOR_VERSION}\n\
                     \n\
                     --data-dir PATH         (or $KLMCP_DATA_DIR)\n\
                     --json                  emit machine-readable JSON\n\
                     --heal                  repair shard HEADs when possible and\n\
                                             remove broken shard repos so next sync\n\
                                             reclones them\n\
                     --clean-manifest-cache  remove <data_dir>/state/manifest.json\n\
                                             so the next sync refetches all shards\n"
                );
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("{DOCTOR_VERSION}");
                std::process::exit(0);
            }
            other => return Err(anyhow!("unknown arg: {other}")),
        }
    }
    Ok(args)
}

fn inspect_data_dir(data_dir: &Path, args: &Args) -> Result<DoctorSummary> {
    let mut summary = DoctorSummary {
        version: DOCTOR_VERSION.to_owned(),
        data_dir: data_dir.display().to_string(),
        tiers: inspect_tiers(data_dir),
        ..DoctorSummary::default()
    };

    let manifest_path = data_dir.join("state").join("manifest.json");
    if args.clean_manifest_cache {
        if manifest_path.exists() {
            fs::remove_file(&manifest_path)
                .map_err(|e| anyhow!("remove {}: {e}", manifest_path.display()))?;
            summary.notes.push(format!(
                "removed manifest cache {}",
                manifest_path.display()
            ));
        } else {
            summary
                .notes
                .push("manifest cache already absent".to_owned());
        }
        summary.tiers.manifest_cache_present = manifest_path.exists();
    }

    if !summary.tiers.manifest_cache_present {
        summary
            .notes
            .push("manifest cache missing; next sync will refetch all shards".to_owned());
    }

    let shard_root = data_dir.join("shards");
    let shard_paths = shard_repo_paths(&shard_root)?;
    summary.shard_repos_total = shard_paths.len();

    for path in shard_paths {
        match inspect_repo_health(&path) {
            RepoHealth::Healthy => {
                summary.shard_repos_healthy += 1;
            }
            RepoHealth::Repairable(candidate) => {
                if args.heal {
                    match repair_repo_head(&path, &candidate) {
                        Ok(()) => {
                            summary.shard_repos_healthy += 1;
                            summary.repaired_heads.push(RepairableShard {
                                path: path.display().to_string(),
                                source_ref: candidate.source_ref,
                                repair_ref: candidate.repair_ref,
                                head_oid: candidate.oid.to_string(),
                            });
                        }
                        Err(e) => {
                            summary.shard_repos_broken += 1;
                            summary.broken_shards.push(BrokenShard {
                                path: path.display().to_string(),
                                reason: format!("repair failed: {e}"),
                            });
                        }
                    }
                } else {
                    summary.shard_repos_repairable += 1;
                    summary.repairable_shards.push(RepairableShard {
                        path: path.display().to_string(),
                        source_ref: candidate.source_ref,
                        repair_ref: candidate.repair_ref,
                        head_oid: candidate.oid.to_string(),
                    });
                }
            }
            RepoHealth::Broken(reason) => {
                if args.heal {
                    fs::remove_dir_all(&path)
                        .map_err(|e| anyhow!("remove broken shard {}: {e}", path.display()))?;
                    summary
                        .removed_broken_shards
                        .push(path.display().to_string());
                } else {
                    summary.shard_repos_broken += 1;
                    summary.broken_shards.push(BrokenShard {
                        path: path.display().to_string(),
                        reason,
                    });
                }
            }
        }
    }

    if args.heal {
        if !summary.repaired_heads.is_empty() {
            summary.notes.push(format!(
                "repaired {} shard HEAD refs in place",
                summary.repaired_heads.len()
            ));
        }
        if !summary.removed_broken_shards.is_empty() {
            summary.notes.push(format!(
                "removed {} broken shard repos; next sync will reclone them",
                summary.removed_broken_shards.len()
            ));
        }
    }

    Ok(summary)
}

fn inspect_tiers(data_dir: &Path) -> TierHealth {
    let state = data_dir.join("state");
    let generation_path = state.join("generation");
    let manifest_path = state.join("manifest.json");
    let (generation, last_ingest_unix_secs, last_ingest_age_seconds) =
        generation_mtime(&generation_path);
    let over_db_path = data_dir.join("over.db");
    TierHealth {
        manifest_cache_present: manifest_path.exists(),
        generation,
        last_ingest_unix_secs,
        last_ingest_age_seconds,
        metadata_ready: has_any(data_dir.join("metadata")),
        over_db_ready: over_db_path.exists(),
        over_db_open_ok: over_db_path.exists() && over_db_open_ok(&over_db_path),
        bm25_ready: state.join("bm25.generation").exists(),
        trigram_ready: state.join("trigram.generation").exists(),
        tid_ready: state.join("tid.generation").exists(),
        tid_parquet_present: data_dir.join("tid").join("tid.parquet").exists(),
        path_vocab_ready: data_dir.join("paths").join("vocab.txt").exists(),
        embedding_ready: data_dir.join("embeddings").join("meta.json").exists(),
        maintainers_ready: maintainers_ready(data_dir),
        git_sidecar_ready: data_dir.join("git_sidecar.db").exists(),
    }
}

fn generation_mtime(path: &Path) -> (Option<u64>, Option<u64>, Option<u64>) {
    let generation = fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    let mtime = path
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let age = mtime.and_then(|ts| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|now| now.as_secs().saturating_sub(ts))
    });
    (generation, mtime, age)
}

fn over_db_open_ok(path: &Path) -> bool {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|conn| {
            conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
                .map(|_| ())
        })
        .is_ok()
}

fn maintainers_ready(data_dir: &Path) -> bool {
    if let Ok(override_path) = std::env::var("KLMCP_MAINTAINERS_FILE") {
        return Path::new(&override_path).exists();
    }
    data_dir.join("MAINTAINERS").exists()
}

fn has_any(root: PathBuf) -> bool {
    if !root.is_dir() {
        return false;
    }
    let Ok(mut entries) = fs::read_dir(root) else {
        return false;
    };
    entries.next().is_some()
}

fn shard_repo_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_git_dirs(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_git_dirs(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root).map_err(|e| anyhow!("read_dir {}: {e}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".git"))
        {
            out.push(path);
            continue;
        }
        collect_git_dirs(&path, out)?;
    }
    Ok(())
}

fn inspect_repo_health(path: &Path) -> RepoHealth {
    let repo = match Git2Repository::open_bare(path).or_else(|_| Git2Repository::open(path)) {
        Ok(repo) => repo,
        Err(e) => return RepoHealth::Broken(format!("open: {e}")),
    };

    if let Ok(head) = repo.head()
        && let Some(oid) = head.target()
    {
        let _ = oid;
        return RepoHealth::Healthy;
    }

    match fallback_head_candidate(&repo) {
        Ok(Some(candidate)) => RepoHealth::Repairable(candidate),
        Ok(None) => RepoHealth::Broken("no usable refs found".to_owned()),
        Err(e) => RepoHealth::Broken(e.to_string()),
    }
}

fn fallback_head_candidate(repo: &Git2Repository) -> Result<Option<HeadCandidate>> {
    let mut candidates: Vec<HeadCandidate> = Vec::new();
    push_ref_candidate(
        repo,
        "refs/heads/master",
        "refs/heads/master",
        0,
        &mut candidates,
    )?;
    push_ref_candidate(
        repo,
        "refs/heads/main",
        "refs/heads/main",
        1,
        &mut candidates,
    )?;
    push_symbolic_candidate(repo, "refs/remotes/origin/HEAD", 2, &mut candidates)?;
    push_ref_candidate(
        repo,
        "refs/remotes/origin/master",
        "refs/heads/master",
        3,
        &mut candidates,
    )?;
    push_ref_candidate(
        repo,
        "refs/remotes/origin/main",
        "refs/heads/main",
        4,
        &mut candidates,
    )?;

    let refs = repo.references().context("enumerate refs")?;
    for reference in refs {
        let reference = reference.context("read ref")?;
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
                source_ref: name.to_owned(),
                repair_ref,
                oid: target,
                rank,
            });
        }
    }

    candidates.sort_by(|a, b| {
        a.rank
            .cmp(&b.rank)
            .then_with(|| a.source_ref.cmp(&b.source_ref))
    });
    Ok(candidates.into_iter().next())
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
        source_ref: source_ref.to_owned(),
        repair_ref: repair_ref.to_owned(),
        oid: target,
        rank,
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

fn repair_repo_head(path: &Path, candidate: &HeadCandidate) -> Result<()> {
    let repo = Git2Repository::open_bare(path).or_else(|_| Git2Repository::open(path))?;
    repo.reference(
        &candidate.repair_ref,
        candidate.oid,
        true,
        "kernel-lore-doctor: repair unborn HEAD from fetched refs",
    )
    .with_context(|| format!("reference {}", candidate.repair_ref))?;
    repo.set_head(&candidate.repair_ref)
        .with_context(|| format!("set_head {}", candidate.repair_ref))?;
    Ok(())
}

fn has_severe_issues(summary: &DoctorSummary) -> bool {
    !summary.broken_shards.is_empty()
        || (summary.tiers.over_db_ready && !summary.tiers.over_db_open_ok)
}

fn print_human(summary: &DoctorSummary) {
    println!("kernel-lore-doctor {}", summary.version);
    println!("data_dir: {}", summary.data_dir);
    println!(
        "generation: {}",
        summary
            .tiers
            .generation
            .map(|g| g.to_string())
            .unwrap_or_else(|| "missing".to_owned())
    );
    println!(
        "last_ingest_age_seconds: {}",
        summary
            .tiers
            .last_ingest_age_seconds
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_owned())
    );
    println!(
        "manifest_cache_present: {}",
        summary.tiers.manifest_cache_present
    );
    println!("metadata_ready: {}", summary.tiers.metadata_ready);
    println!(
        "over_db_ready/open_ok: {}/{}",
        summary.tiers.over_db_ready, summary.tiers.over_db_open_ok
    );
    println!(
        "bm25/trigram/tid markers: {}/{}/{}",
        summary.tiers.bm25_ready, summary.tiers.trigram_ready, summary.tiers.tid_ready
    );
    println!(
        "tid_parquet/path_vocab/embedding: {}/{}/{}",
        summary.tiers.tid_parquet_present,
        summary.tiers.path_vocab_ready,
        summary.tiers.embedding_ready
    );
    println!(
        "maintainers/git_sidecar: {}/{}",
        summary.tiers.maintainers_ready, summary.tiers.git_sidecar_ready
    );
    println!(
        "shards: total={} healthy={} repairable={} broken={}",
        summary.shard_repos_total,
        summary.shard_repos_healthy,
        summary.shard_repos_repairable,
        summary.shard_repos_broken
    );

    if !summary.repairable_shards.is_empty() {
        println!("repairable shard repos:");
        for shard in &summary.repairable_shards {
            println!(
                "  {}  {} -> {} ({})",
                shard.path, shard.source_ref, shard.repair_ref, shard.head_oid
            );
        }
    }
    if !summary.broken_shards.is_empty() {
        println!("broken shard repos:");
        for shard in &summary.broken_shards {
            println!("  {}  {}", shard.path, shard.reason);
        }
    }
    if !summary.repaired_heads.is_empty() {
        println!("repaired shard heads:");
        for shard in &summary.repaired_heads {
            println!(
                "  {}  {} -> {} ({})",
                shard.path, shard.source_ref, shard.repair_ref, shard.head_oid
            );
        }
    }
    if !summary.removed_broken_shards.is_empty() {
        println!("removed broken shard repos:");
        for path in &summary.removed_broken_shards {
            println!("  {path}");
        }
    }
    if !summary.notes.is_empty() {
        println!("notes:");
        for note in &summary.notes {
            println!("  {note}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

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

    fn seed_bare_repo(repo_dir: &Path) -> git2::Oid {
        git(&["init", "-q", "--bare", "."], repo_dir);

        let work = tempdir().unwrap();
        git(&["init", "-q", "-b", "master", "."], work.path());
        fs::write(work.path().join("m"), "hello\n").unwrap();
        git(&["add", "m"], work.path());
        git(&["commit", "-q", "-m", "c1"], work.path());
        git(
            &["remote", "add", "origin", repo_dir.to_str().unwrap()],
            work.path(),
        );
        git(&["push", "-q", "origin", "master"], work.path());
        let sha = git_stdout(&["rev-parse", "HEAD"], work.path());
        git2::Oid::from_str(sha.trim()).unwrap()
    }

    #[test]
    fn unborn_repo_without_refs_is_broken() {
        let dir = tempdir().unwrap();
        git(&["init", "-q", "--bare", "."], dir.path());
        match inspect_repo_health(dir.path()) {
            RepoHealth::Broken(_) => {}
            other => panic!("expected broken repo, got {other:?}"),
        }
    }

    #[test]
    fn remote_tracking_ref_is_repairable() {
        let dir = tempdir().unwrap();
        let oid = seed_bare_repo(dir.path());
        git(
            &["update-ref", "refs/remotes/origin/master", &oid.to_string()],
            dir.path(),
        );
        git(
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/master",
            ],
            dir.path(),
        );
        git(&["update-ref", "-d", "refs/heads/master"], dir.path());
        git(&["symbolic-ref", "HEAD", "refs/heads/main"], dir.path());

        match inspect_repo_health(dir.path()) {
            RepoHealth::Repairable(candidate) => {
                assert_eq!(candidate.source_ref, "refs/remotes/origin/master");
                assert_eq!(candidate.repair_ref, "refs/heads/master");
                assert_eq!(candidate.oid, oid);
            }
            other => panic!("expected repairable repo, got {other:?}"),
        }
    }

    #[test]
    fn heal_removes_unrecoverable_shard_repo() {
        let dir = tempdir().unwrap();
        let shard = dir.path().join("shards").join("broken").join("0.git");
        fs::create_dir_all(&shard).unwrap();
        git(&["init", "-q", "--bare", "."], &shard);

        let summary = inspect_data_dir(
            dir.path(),
            &Args {
                heal: true,
                ..Args::default()
            },
        )
        .unwrap();

        assert_eq!(summary.shard_repos_total, 1);
        assert_eq!(summary.removed_broken_shards.len(), 1);
        assert_eq!(summary.broken_shards.len(), 0);
        assert!(!shard.exists());
    }
}
