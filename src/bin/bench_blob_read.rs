//! Benchmark: gix vs git2 blob-read on a real packed shard.
//!
//! Usage:
//!     bench_blob_read <shard.git>
//!
//! Walks the shard's commits, reads every `m` blob via both gix and
//! git2, and reports wall-clock + per-blob timing.

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let shard_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: bench_blob_read <shard.git>");

    eprintln!("shard: {}", shard_path.display());

    // Phase 1: use gix to walk commits and collect (commit_oid, blob_oid) pairs.
    // We need the blob OIDs so git2 can read them directly.
    eprintln!("--- collecting OIDs via gix rev-walk ---");
    let t0 = Instant::now();
    let mut repo = gix::open(&shard_path).expect("gix open");
    repo.object_cache_size(256 * 1024 * 1024);

    let head_id = repo.head_id().expect("head_id").detach();
    let walk = repo.rev_walk([head_id]).all().expect("rev_walk");

    let mut oids: Vec<(String, gix::ObjectId)> = Vec::new(); // (mid_or_idx, blob_oid)
    let mut skipped = 0u64;
    for (i, info) in walk.enumerate() {
        let info = info.expect("walk step");
        let commit = info.object().expect("commit object");
        let tree = commit.tree().expect("tree");
        let Some(entry) = tree.find_entry("m") else {
            skipped += 1;
            continue;
        };
        oids.push((format!("{i}"), entry.object_id()));
    }
    let walk_ms = t0.elapsed().as_millis();
    eprintln!(
        "collected {} blob OIDs in {}ms ({} skipped, {:.2} ms/commit)",
        oids.len(),
        walk_ms,
        skipped,
        walk_ms as f64 / (oids.len() + skipped as usize) as f64
    );

    // Phase 2: read all blobs via gix
    eprintln!("--- gix blob read ({} blobs) ---", oids.len());
    let t1 = Instant::now();
    let mut gix_bytes: u64 = 0;
    for (_, oid) in &oids {
        let obj = repo.find_object(*oid).expect("gix find_object");
        gix_bytes += obj.data.len() as u64;
    }
    let gix_ms = t1.elapsed().as_millis();
    eprintln!(
        "gix: {}ms total, {:.3} ms/blob, {} MB read",
        gix_ms,
        gix_ms as f64 / oids.len() as f64,
        gix_bytes / 1024 / 1024,
    );

    // Phase 3: read all blobs via git2
    eprintln!("--- git2 blob read ({} blobs) ---", oids.len());
    let git2_repo =
        git2::Repository::open_bare(&shard_path).expect("git2 open");
    let t2 = Instant::now();
    let mut git2_bytes: u64 = 0;
    for (_, gix_oid) in &oids {
        let oid_bytes = gix_oid.as_bytes();
        let g2_oid = git2::Oid::from_bytes(oid_bytes).expect("oid convert");
        let blob = git2_repo.find_blob(g2_oid).expect("git2 find_blob");
        git2_bytes += blob.content().len() as u64;
    }
    let git2_ms = t2.elapsed().as_millis();
    eprintln!(
        "git2: {}ms total, {:.3} ms/blob, {} MB read",
        git2_ms,
        git2_ms as f64 / oids.len() as f64,
        git2_bytes / 1024 / 1024,
    );

    eprintln!("---");
    eprintln!(
        "speedup: {:.1}x (git2 vs gix)",
        gix_ms as f64 / git2_ms.max(1) as f64
    );
    assert_eq!(gix_bytes, git2_bytes, "byte count mismatch!");
    eprintln!("byte counts match: {} MB", gix_bytes / 1024 / 1024);
}
