# Ops — EC2 sizing

## Recommended instance

The v0 draft of this doc recommended `c7g.xlarge` (8 GB). That's
wrong — our hot set is 25–45 GB of mmap'd indices. An 8 GB box
thrashes on cold BM25. Corrected:

| | Serving (steady state) | Ingestion (separate unit) |
|---|---|---|
| Instance | **`r7g.xlarge`** Graviton (4 vCPU, 32 GB) **or** `r7i.xlarge` Intel (4 vCPU, 32 GB) | `c7g.4xlarge` / `c7i.4xlarge`, spun up on-demand for full reindex |
| EBS | **500–750 GB gp3, 16000 IOPS, 1000 MB/s** | borrow same volume (stop/start serving unit briefly) |
| Network | Standard | Bursty (lore sync traffic ~1–5 GB/day steady, 50–120 GB initial) |
| Cost (us-east-1 on-demand, Apr 2026) | ~$145/mo (Graviton) or ~$184/mo (Intel) compute + ~$140/mo EBS | ~$0.70/hr × a few hrs/week = pennies |

Why IOPS jumped from 6000 to 16000: the original budget came from
back-of-envelope math on warm-cache BM25. Under real load with
unfortunate tail queries (phrase-equivalent / unanchored regex
over a cold segment) you'll queue at 6000. 16000 gives headroom
without reaching io2 Block Express pricing.

Graviton wins on $/perf for serving (tantivy, gix, roaring, fst,
zstd all compile clean for aarch64). Intel only if you need SIMD
intrinsics we don't write in v1.

## Storage budget (v1)

| Component | Size |
|---|---|
| lore git shards (kept for re-walk) | 50–120 GB |
| Subsystem maintainer trees | 50–100 GB |
| Compressed raw store | 20–35 GB |
| Metadata tier (Parquet) | 2–5 GB |
| Trigram tier | 15–25 GB |
| BM25 tier (tantivy) | 8–15 GB |
| Ingestion scratch | 50 GB |
| OS + logs | 10 GB |
| **Total** | **~200–360 GB** |

500 GB gp3 is comfortable. 750 GB gives a year of growth
headroom.

## Hot set

The *working set* (indices + metadata) is ~25–45 GB. Fits in RAM
on an `r7i.xlarge` (32 GB). mmap'd, so cold start still works on
smaller instances; warm latency improves.

## Networking

- Outbound: `kernel-lore-sync` against `erol.kernel.org`
  (authoritative mirror) or any tier-1 lore mirror. Bursty; pick a
  mirror close to region.
- Inbound: public HTTP/HTTPS. Behind ALB or nginx with rate
  limiting. CloudFront for GET cacheability (most MCP tool calls
  are idempotent).

## Security

- No SSH keys in source; use SSM Session Manager.
- No IAM credentials needed on the box beyond EC2 role for S3
  backups.
- Firewall: 80/443 world, 22 disabled (SSM only), everything else
  closed.

## Backup

- gp3 snapshots daily. 7-day retention.
- Compressed store tiered to S3 Glacier Instant Retrieval monthly.
  (Restoring takes minutes; cheap insurance.)

## Not needed v1

- Auto-scaling group.
- Load balancer beyond a single target.
- Multi-AZ.
- Read replicas (everything's mmap, single-node is fine).

Revisit when we hit 50+ QPS sustained OR we miss the 99.5%
monthly SLO for two consecutive months (see
`docs/ops/monitoring.md`).

## AZ failure risk

EBS gp3 is single-AZ. If the AZ goes, the box is cold until we
restore from snapshot in another AZ — ~1 hr RTO. For a read-only
public archive this is acceptable v1. State it.
