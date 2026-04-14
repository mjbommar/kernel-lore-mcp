# gix 0.81

Rust-specific (no Python parallel).

Pinned: `gix = { version = "0.81", default-features = false,
features = ["max-performance-safe", "revision", "parallel"] }`.
**Not `git2-rs`.** See `docs/research/2026-04-14-gix-vs-git2.md`
for the rationale; short version: `git2` is not `Sync` and we
need to fan out across shards with rayon.

`blocking-network-client` is intentionally OFF. `grokmirror`
handles fetches; our Rust side only reads local shards.

---

## What gix is used for

Walking lore shards at ingest time. Each shard is a
`public-inbox` repository — a Git repo whose commits are
messages. The commit message contains the `mbox`-serialized
email; the tree contains an `m` blob (the message) and an `s`
blob (the shadow / metadata). See
`docs/ingestion/shard-walking.md` (TODO).

**gix is not used** for any push, fetch, clone, or network
operation. Those live in `grokmirror`.

---

## `ThreadSafeRepository::open` + `to_thread_local()`

This is the load-bearing pattern. Everything else in this doc
hangs off it.

```rust
use gix::ThreadSafeRepository;
use rayon::prelude::*;

// Outside the parallel section — open is cheap.
let shards: Vec<(String, ThreadSafeRepository)> = discover_shards(root)?
    .into_iter()
    .map(|(name, path)| {
        let ts = ThreadSafeRepository::open(path)?;
        Ok::<_, crate::Error>((name, ts))
    })
    .collect::<Result<_, _>>()?;

// Inside: one rayon task per shard.
shards.par_iter().try_for_each(|(name, ts)| -> crate::Result<()> {
    let repo = ts.to_thread_local();    // !Sync, per-worker
    walk_shard(name, &repo, &mut emitter_for(name))
})?;
```

Why two types:

- `ThreadSafeRepository`: `Send + Sync`, cheap handle. Moves
  across threads, can be cloned / shared.
- `Repository`: `Send` only. Cached parsed state (packfile
  indexes, config). What you actually query with.

**Never** call `to_thread_local()` outside a worker and then
send the `Repository` across threads. Compile error by design.

One rayon task per *shard*, not per commit within a shard. The
packfile cache is per-Repository; sharing it within a shard
gives you better locality than spawning per-commit workers.

See `../design/concurrency.md`.

---

## Walk pattern — incremental via `rev_walk`

We want "everything since `last_indexed_oid`", i.e., the set
of commits reachable from HEAD but not from `last_oid`.

```rust
fn walk_shard(
    name: &str,
    repo: &gix::Repository,
    emit: &mut impl FnMut(gix::ObjectId, &[u8]) -> crate::Result<()>,
) -> crate::Result<()> {
    let head = repo.head_id()?;

    let last = state::last_indexed_oid(name)?;  // Option<ObjectId>

    // Build the walk. If we have a last_oid and it still resolves,
    // hide it (and everything reachable from it) from the walk.
    let walk = match last {
        Some(oid) if repo.find_object(oid).is_ok() => {
            repo.rev_walk([head])
                .with_hidden([oid])
                .all()?
        }
        _ => {
            // First run OR shard was repacked upstream and last_oid
            // is dangling. Full walk; the idempotent ingest layer
            // dedupes by message_id.
            tracing::info!(shard = name, "full re-walk (no last_oid or dangling)");
            repo.rev_walk([head]).all()?
        }
    };

    for info in walk {
        let info = info?;
        let commit = repo.find_object(info.id)?.into_commit();
        let tree   = commit.tree()?;
        // Read the `m` blob (mbox-formatted message).
        let m_entry = tree.lookup_entry_by_path("m")?
            .ok_or_else(|| crate::Error::State(
                format!("{name}: commit {} missing `m` blob", info.id)
            ))?;
        let m_blob = repo.find_object(m_entry.oid())?;
        emit(info.id, &m_blob.data)?;
    }

    state::save_last_indexed_oid(name, head)?;
    Ok(())
}
```

Key API points:

- **`rev_walk([head]).with_hidden([last_oid]).all()`** — exact
  `git rev-list head --not last_oid` semantics. Returns
  commits reachable from `head` that aren't reachable from
  `last_oid`. First-parent discipline not needed for lore
  (linear history).
- **`.all()`** materializes to an iterator of
  `Result<gix::revision::walk::Info, ...>`. Don't collect the
  whole thing; it can be millions on a full walk.
- **`with_hidden`** takes any iterable of `ObjectId`s; we only
  hide one per shard.
- Commits yield via `find_object(info.id)` — the walk gives
  IDs, not full objects, to avoid parsing cost when you're
  filtering.

---

## Handling shard repack / dangling `last_oid`

`public-inbox` repacks shards periodically. After a repack,
`last_oid` may not resolve (object pruned, rewritten with
slightly different commit IDs, etc.). Our fallback:

1. `repo.find_object(last_oid)` fails → full re-walk.
2. Downstream dedupe by `message_id`: the metadata tier's
   `message_id` is unique; duplicate adds become upserts (or
   no-ops if body_sha256 matches).

Never treat a missing `last_oid` as a data loss or bug. It is
an expected upstream event, logged at `INFO` not `WARN`.

---

## Reading a blob at a given path in a commit's tree

Needed for:

- The `m` blob (message body) per commit.
- Occasionally the `s` blob (shadow / pre-parsed metadata) if
  we ever trust it.

Pattern:

```rust
let tree  = commit.tree()?;
let entry = tree.lookup_entry_by_path("m")?
    .ok_or_else(|| crate::Error::State("missing m".into()))?;
let blob  = repo.find_object(entry.oid())?.into_blob();
let bytes: &[u8] = &blob.data;
```

`lookup_entry_by_path` accepts `&str` or `&[u8]`. For lore
shards, keys are always ASCII; `&str` is fine.

For trees with many files (not lore's case — always 2-3 files),
a manual iterator over `tree.entries()` with path-prefix
matching is more efficient.

---

## `commit-graph` and `multi-pack-index` speedups

`public-inbox` repos benefit enormously from:

- `git commit-graph write --reachable --changed-paths` — makes
  parent/child traversal O(1) per commit.
- `git multi-pack-index write` — single index across all
  packfiles.

gix 0.81 honors both automatically if present. We don't
generate them (grokmirror / public-inbox does). If a shard
lacks them, walks are slower but correct.

**Don't** write `commit-graph` from our code — it's a
filesystem mutation on shards we don't own. If shards come
through slow, file an issue with `grokmirror` config.

---

## Features we enable and why

| Feature | Why |
|---|---|
| `max-performance-safe` | All the speed, no `unsafe` opt-ins. Uses SIMD where soundness-proved. |
| `revision` | `rev_walk`, `ObjectId` helpers. |
| `parallel` | Internal parallelism for pack index loading. |

Features we DON'T enable:

| Feature | Why not |
|---|---|
| `blocking-network-client` | No fetch in our Rust side. |
| `async-network-client` | ditto, and we don't use async. |
| `max-performance` (unsafe variant) | The `-safe` variant is fast enough. Our budget isn't in gix. |
| `worktree-mutation` | We never write. |
| `credentials`, `prodash-render-line`, `prodash-render-tui` | We don't run interactive. |

---

## Error handling

gix error types are rich (one enum per subsystem). We wrap at
the call boundary:

```rust
let repo = ThreadSafeRepository::open(path)
    .map_err(|e| crate::Error::Gix(format!("open {}: {}", path.display(), e)))?;
```

Don't `#[from]` gix errors into `crate::Error`. gix changes
error shapes across minor versions more than the other crates
we pin; stringifying at the boundary is the path that ages
best.

---

## Don't-do list

| Anti-pattern | Why |
|---|---|
| Adding `git2`, `libgit2-sys`, `git2-rs` | Not `Sync`. See research doc. |
| Using `blocking-network-client` | Our fetch pipeline is `grokmirror`. |
| Holding a `Repository` across rayon task boundaries | `!Sync`. Won't compile. |
| Shelling out to `git` | Parsing output is fragile; gix is the same or faster. |
| Writing to shards from our code | They're not ours to write. Read-only. |
| Walking per-commit in a par_iter (nested) | Cache locality drops. One task per shard. |

---

## Checklist for a gix change

1. Still on `--no-default-features`, only the three listed.
2. `ThreadSafeRepository::open` happens outside the parallel
   section.
3. `to_thread_local()` happens inside the worker.
4. `rev_walk([head]).with_hidden([last_oid])` with a
   dangling-oid fallback.
5. Error mapping via `Error::Gix(format!(...))`, not
   `#[from]`.
6. `last_indexed_oid` saved atomically (rename) after a
   successful shard completion.

See also:
- `docs/ingestion/shard-walking.md` (TODO) — concrete walk spec.
- `../design/concurrency.md` — rayon + thread-local repos.
- `../design/errors.md` — Gix variant and context mapping.
