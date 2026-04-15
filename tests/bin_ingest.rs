//! Integration test for the `kernel-lore-ingest` binary.
//!
//! Builds two synthetic public-inbox-like shards under a fake
//! grokmirror-managed root and runs the binary against them. Verifies
//! the parquet artifacts end up where the reader expects and that
//! rayon handled both shards.

use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

fn make_shard(shard_dir: &Path, messages: &[&[u8]]) {
    let run = |args: &[&str], cwd: &Path| {
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
    };
    let work = tempdir().unwrap();
    run(&["init", "-q", "-b", "master", "."], work.path());
    for (i, msg) in messages.iter().enumerate() {
        std::fs::write(work.path().join("m"), msg).unwrap();
        run(&["add", "m"], work.path());
        run(&["commit", "-q", "-m", &format!("m{i}")], work.path());
    }
    if shard_dir.exists() {
        std::fs::remove_dir_all(shard_dir).unwrap();
    }
    std::fs::create_dir_all(shard_dir.parent().unwrap()).unwrap();
    run(
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

#[test]
fn ingest_binary_walks_mirror_and_writes_parquet() {
    let mirror = tempdir().unwrap();
    let data = tempdir().unwrap();

    make_shard(
        &mirror.path().join("linux-cifs/git/0.git"),
        &[b"From: Alice <alice@example.com>\r\n\
Subject: [PATCH 1/1] ksmbd: tighten ACL bounds\r\n\
Date: Mon, 14 Apr 2026 12:00:00 +0000\r\n\
Message-ID: <a1@x>\r\n\
\r\n\
Prose.\r\n\
Signed-off-by: Alice <alice@example.com>\r\n\
---\r\n\
diff --git a/x.c b/x.c\r\n\
--- a/x.c\r\n\
+++ b/x.c\r\n\
@@ -1,1 +1,2 @@ int foo(int x)\r\n\
 a\r\n\
+b\r\n"],
    );
    make_shard(
        &mirror.path().join("linux-nfs/git/0.git"),
        &[b"From: Bob <bob@example.com>\r\n\
Subject: [PATCH] nfs: do a thing\r\n\
Date: Mon, 14 Apr 2026 13:00:00 +0000\r\n\
Message-ID: <n1@x>\r\n\
\r\n\
Body.\r\n"],
    );

    let bin = env!("CARGO_BIN_EXE_kernel-lore-ingest");
    let out = Command::new(bin)
        .arg("--data-dir")
        .arg(data.path())
        .arg("--lore-mirror")
        .arg(mirror.path())
        .arg("--run-id")
        .arg("itest-0001")
        .output()
        .expect("spawn kernel-lore-ingest");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "binary failed. status={:?} stderr={}",
        out.status,
        stderr
    );

    // Both lists should have a parquet file for this run.
    let cifs_parquet = data
        .path()
        .join("metadata/linux-cifs/itest-0001-linux-cifs-0.parquet");
    let nfs_parquet = data
        .path()
        .join("metadata/linux-nfs/itest-0001-linux-nfs-0.parquet");
    assert!(cifs_parquet.exists(), "missing: {}", cifs_parquet.display());
    assert!(nfs_parquet.exists(), "missing: {}", nfs_parquet.display());

    // Structured log lines should carry both shards.
    assert!(stderr.contains("linux-cifs"));
    assert!(stderr.contains("linux-nfs"));
    assert!(stderr.contains("\"shard done\""));
    assert!(stderr.contains("\"ingest complete\""));
}

#[test]
fn ingest_binary_respects_list_filter() {
    let mirror = tempdir().unwrap();
    let data = tempdir().unwrap();

    make_shard(
        &mirror.path().join("linux-cifs/git/0.git"),
        &[b"Subject: [PATCH] a\r\nMessage-ID: <a@x>\r\n\r\nok\r\n"],
    );
    make_shard(
        &mirror.path().join("linux-nfs/git/0.git"),
        &[b"Subject: [PATCH] b\r\nMessage-ID: <b@x>\r\n\r\nok\r\n"],
    );

    let bin = env!("CARGO_BIN_EXE_kernel-lore-ingest");
    let out = Command::new(bin)
        .arg("--data-dir")
        .arg(data.path())
        .arg("--lore-mirror")
        .arg(mirror.path())
        .arg("--list")
        .arg("linux-cifs")
        .arg("--run-id")
        .arg("filtered")
        .output()
        .expect("spawn binary");

    assert!(out.status.success());
    assert!(
        data.path()
            .join("metadata/linux-cifs/filtered-linux-cifs-0.parquet")
            .exists()
    );
    assert!(!data.path().join("metadata/linux-nfs").exists());
}
