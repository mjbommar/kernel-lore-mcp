# Ingestion — shard walking

## public-inbox v2 layout recap

Each public-inbox archive is a **sharded git repo**: one bare repo
per shard at `<list>/git/<N>.git`. Shards max ~1 GB; high-volume
lists get many (lkml has 19). The commit log is the message log —
**one commit per message**. Commit tree contains:

- `m` — the RFC822 text blob (what we want)
- `d` — deletion marker for replaced messages (skip)

Commit author/committer date ≈ message received-time.

## Why gix

- `ThreadSafeRepository::to_thread_local()` — share a repo across
  rayon workers without re-opening per thread. git2-rs's
  `Repository` is not `Sync`.
- Faster linear-history walks on big repos per the gitoxide perf
  reports (2–4× on multi-GB mailing-list repos).
- mmap pack cache with a tunable size — matters when you're
  walking 390 shards concurrently.

## Walking pattern

```rust
use gix::ThreadSafeRepository;
use rayon::prelude::*;

let shards: Vec<_> = discover_shards(&lore_mirror_dir)?; // paths to N.git
shards.par_iter().try_for_each(|path| -> anyhow::Result<()> {
    let ts_repo = ThreadSafeRepository::open(path)?;
    let repo = ts_repo.to_thread_local();
    let last_oid = load_last_indexed_oid(path)?;
    let head = repo.head_commit()?;

    let walk = repo.rev_walk([head.id])
        .with_hidden(last_oid.into_iter())
        .all()?;

    for info in walk {
        let commit = info?.object()?;
        let tree = commit.tree()?;
        let Some(m) = tree.find_entry("m") else { continue };
        let blob = repo.find_object(m.oid())?.into_blob();
        process_message(&blob.data, &commit)?;
    }
    save_last_indexed_oid(path, head.id)?;
    Ok(())
})?;
```

## Parallelism

- **Across shards** via rayon. 390 shards >> CPU count; per-shard
  work is roughly uniform; cache locality on one packfile per
  worker.
- **Not within a shard.** Tried and regretted: pack random-access
  thrash.

## Running `git multi-pack-index write` + `git commit-graph write`

Both libs (gix, git2) benefit 5–10× from a commit-graph. Run once
per shard before first ingest:

```bash
find /var/lore-mirror -name '*.git' -type d -exec sh -c '
    cd "$1" && git multi-pack-index write && git commit-graph write
' _ {} \;
```

Add this to the grokmirror post-pull hook so new shards pick it up.

## Incremental: the `last_indexed_oid` file

Per shard, we store the OID of the newest commit we finished
processing in `<state_dir>/<shard>.oid`. Next run passes it via
`with_hidden` — gix walks only descendants. Atomic write +
rename so a crash mid-write doesn't produce a garbage state.

## What we skip

- `d` blobs — deletion markers; public-inbox semantics say the
  underlying message is gone. We delete from our indices via the
  `message_id` carried in the commit message subject.
- Empty `m` blobs — rare; log and skip.
- Messages without a `Message-ID` header — also rare; log and
  skip. They can't participate in threading.
