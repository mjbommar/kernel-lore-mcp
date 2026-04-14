# Ops — cost model

All-in steady state target: **under $150/mo.** Strict budget cap
before we have funding: **$250/mo.**

## Line items (us-east-1, April 2026 on-demand prices)

| Line | Spec | $/mo |
|---|---|---|
| EC2 | `r7i.xlarge` (24/7) | ~$145 |
| EC2 (alt) | `c7g.xlarge` Graviton (24/7) | ~$85 |
| EBS | 500 GB gp3 base | $40 |
| EBS IOPS upgrade | 6000 IOPS (3000 over base) | $19 |
| EBS throughput | 250 MB/s (125 over base) | $5 |
| Data transfer out | 100 GB/mo baseline | $9 |
| CloudFront | GETs cached, minimal origin hits | $5–15 |
| S3 Glacier IR backup | 50 GB store + indices | $5 |
| Route 53 | 1 hosted zone + a few queries | $1 |
| **Total on Intel** | | **~$229/mo** |
| **Total on Graviton** | | **~$169/mo** |

## Savings Plans

1-yr no-upfront Compute Savings Plan on the EC2 line takes ~28% off.
With Graviton + Savings Plan: **~$120/mo.**

## Ingestion burst

`c7i.4xlarge` on-demand for initial full-corpus index build:
- ~2 hours × $0.856/hr = $1.71 per rebuild.
- Done manually when we change the schema (expected: every few
  months in v1; approaching zero post-v1).

## Traffic-scaling scenarios

If we hit 50 QPS sustained:
- CloudFront hit rate should exceed 80% on repeat tool calls
  (cursor continuations, thread fetches) — origin load doesn't
  scale linearly.
- If origin CPU saturates, first action is to pre-compute the
  top-K query cache, not upsize. We're I/O-light, not CPU-heavy.
- Upsize path: `r7i.2xlarge` (~$290/mo) → handles ~200 QPS.

## What NOT to spend on

- ALB. Overkill for one target; nginx on the same box handles it.
- RDS / managed DB. We don't have a DB.
- Kubernetes. One instance, one systemd unit.
- OpenSearch / Elasticsearch managed. That's literally what we're
  replacing with tantivy.
