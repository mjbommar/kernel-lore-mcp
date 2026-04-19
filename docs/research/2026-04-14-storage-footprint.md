# Research — storage footprint for full-lore mirror (April 14 2026)

## Manifest ground truth

`lore.kernel.org/manifest.js.gz` as of 2026-04-14:
- **390 git shards across 346 unique lists.**
- Highest-volume lists: lkml (19 shards), qemu-devel / netdev /
  linux-arm-kernel / linux-devicetree (4 shards each).
- Manifest itself is ~20 KB gzipped; no `size` field on entries,
  only fingerprint + modified timestamp.

## Empirical size scaling

From the existing nas4 mirror (4 lists, fetched April 2026):
- linux-fsdevel: 1.1 GB (1 shard, packed)
- linux-nfs: 415 MB (1 shard)
- linux-cifs: 129 MB (1 shard)
- linux-cve-announce: 28 MB
- lkml: 3.4 GB across 19 shards (~180 MB/shard avg — shards grow
  linearly until they hit ~1 GB cap, then a new shard starts)

Extrapolation to all of lore: **50–120 GB of git objects** after
`git gc`. Plus Xapian-equivalent indices on top (but we're not
keeping Xapian — we derive our own).

## Our index estimates (full corpus)

Per [`../architecture/four-tier-index.md`](../architecture/four-tier-index.md)
and [`../indexing/compressed-store.md`](../indexing/compressed-store.md):

| Component | Size |
|---|---|
| lore git shards (source, kept for re-walk) | 50–120 GB |
| Subsystem maintainer trees (cel-linux, cifs-2.6, ksmbd, linux-next, ...) | 50–100 GB |
| Compressed raw store (zstd-dict per list) | 20–35 GB |
| Metadata tier (Parquet + Arrow) | 2–5 GB |
| Trigram tier (fst + roaring) | 15–25 GB |
| BM25 tier (tantivy, no positions) | 8–15 GB |
| Ingestion scratch | 50 GB |
| **Total v1** | **~200–350 GB** |

## Growth rate

- lkml has added ~1 shard/year historically; other high-volume
  lists similar.
- Monthly growth: order of 1–3 GB across git sources, 200–500 MB
  across indices. Negligible relative to the budget.

## Budget pick

**500 GB gp3.** Fits with headroom through 2028.

## Sources

- [lore.kernel.org manifest](https://lore.kernel.org/manifest.js.gz)
- [Mirroring lore.kernel.org — Konstantin Ryabitsev](https://people.kernel.org/monsieuricon/mirroring-lore-kernel-org)
- nas4 mirror (internal; `/nas4/data/workspace-infosec/lore-mirror/`)
