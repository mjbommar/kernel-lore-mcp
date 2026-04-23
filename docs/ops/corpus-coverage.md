# Corpus coverage

Last verified: 2026-04-17 (full ingest run).

## Summary

kernel-lore-mcp indexes the complete lore.kernel.org archive:
**29 million messages across 346 mailing lists (390 git shards).**

This covers every public-inbox v2 shard published in the
lore.kernel.org grokmirror manifest. No list is excluded. The
corpus spans from the earliest archived messages (~2000 for some
lists) through the present, with incremental sync via
`kernel-lore-sync` typically keeping the index within roughly
5-11 minutes of upstream.

## Disk footprint

| Component | Size | Description |
|-----------|------|-------------|
| Git mirrors (shards/) | 94 GB | Bare repos from grokmirror |
| Compressed store (store/) | 104 GB | zstd-6 compressed raw messages |
| Metadata (metadata/) | 4.8 GB | Parquet columnar index |
| over.db (over.db) | 19 GB | SQLite metadata point-lookup tier (see [`../architecture/over-db.md`](../architecture/over-db.md)) |
| Trigram (trigram/) | 17 GB | fst + roaring posting lists |
| TID side-table (tid/) | 459 MB | thread-id mapping |
| **Total** | **~239 GB** | All components |

The index alone (without git mirrors) is ~145 GB. The git mirrors
are needed for incremental sync but can be discarded after a full
ingest if no further sync is planned.

BM25 (tantivy) is not included in the above — it is built as a
separate pass via `kernel-lore-reindex --tier bm25` and adds
~10-15 GB.

## List inventory

346 lists, 390 git shards. The number of shards per list is a proxy
for volume: public-inbox v2 rolls a new epoch shard roughly every
500k–1M messages.

### By shard count (top 20)

| List | Shards | Est. messages |
|------|--------|---------------|
| lkml | 19 | ~5M |
| netdev | 4 | ~1M |
| linux-arm-kernel | 4 | ~900k |
| linux-devicetree | 4 | ~800k |
| qemu-devel | 4 | ~800k |
| dri-devel | 3 | ~700k |
| linux-mm | 3 | ~600k |
| dpdk-dev | 2 | ~400k |
| intel-gfx | 2 | ~350k |
| git | 2 | ~300k |
| kvm | 2 | ~300k |
| oe-kbuild-all | 2 | ~300k |
| linux-fsdevel | 2 | ~300k |
| xen-devel | 2 | ~250k |
| linux-media | 2 | ~250k |
| stable | 2 | ~200k |
| u-boot | 2 | ~500k |
| All 329 single-shard lists | 1 each | varies |

### Activity tiers (as of April 2026)

| Tier | Lists | Shards |
|------|-------|--------|
| Active (posted in last 30 days) | 282 | 326 |
| Quiet (30 days – 6 months) | 19 | 19 |
| Dormant (6 months – 1 year) | 5 | 5 |
| Dead (no posts in >1 year) | 40 | 40 |

Dead lists include historical archives like `ultralinux` (last post
2008), `linux-x11` (2014), `linux-metag` (2018). They are still
indexed — historical search is a feature, not waste.

## Initial setup time

Measured on an 8-core (4x2 HT) workstation with 64 GB RAM, writing
to a Synology NAS over NFS (1 GbE).

| Step | Wall clock | Notes |
|------|------------|-------|
| `kernel-lore-sync` fetch phase (all 390 shards) | ~2 hours | Bandwidth-limited by kernel.org |
| `kernel-lore-sync` ingest phase (metadata + store + trigram + over.db) | ~3 hours | CPU-bound; 4 cores saturated |
| Retry failed shards | ~2 hours | 17 shards failed on first pass due to malformed Message-IDs; all succeeded on retry |
| **Total cold start** | **~7 hours** | One-time; incremental sync is seconds |

On faster hardware (r7g.xlarge with gp3 NVMe) the ingest phase
would be roughly 2x faster. The grokmirror pull is always
bandwidth-limited.

## Incremental sync

After the initial ingest, `kernel-lore-sync` typically runs on a
5-minute timer (configurable). Each tick:

1. Fetches `manifest.js.gz` and diffs shard fingerprints.
2. Fetches only changed shards from lore.kernel.org.
3. Walks only new commits in updated shards and appends to the
   existing index.

Cost per tick is negligible for storage and CPU.

## Known gaps

These are inherent to lore.kernel.org, not to our indexing:

- **Private lists** (`security@kernel.org`) — embargoed traffic
  never reaches lore.
- **Distro backport lists** — vendor-internal.
- **Off-list discussion** — IRC, private email, video calls.
- **Lore propagation delay** — lore trails vger by 1–5 minutes;
  our sync adds another ~0–5 minutes of scheduler jitter plus
  ingest time.

See the `blind_spots://coverage` MCP resource for the full list.

## Failure modes and recovery

### Malformed messages

Some archived messages (particularly pre-2010) contain:

- **Line-folded Message-IDs** — RFC 2822 header folding leaves
  `\r\n` + whitespace inside the Message-ID. Fixed: the ingest
  pipeline normalizes by collapsing whitespace.
- **`\r\n` line endings in bodies** — causes byte-offset drift
  when splitting prose from patch. Fixed: uses `find("\ndiff --git")`
  instead of `lines()` + byte accumulator.
- **Non-UTF-8 encoded bodies** — handled by `mail-parser` with
  `full_encoding` feature.

### Shard failures

The ingest binary retries each failed shard up to 3 times (default,
configurable via `--max-retries N`) with exponential backoff (2s,
4s, 8s, ...). Per-attempt errors are logged with full error chains
for diagnosis.

### Re-ingest

If the index becomes corrupt or a schema migration requires it, the
compressed store is the source of truth. `kernel-lore-reindex`
rebuilds slower derived tiers from local data without refetching
from lore.
