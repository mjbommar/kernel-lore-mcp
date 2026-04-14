# Ops — monitoring

Single-box, so keep this simple.

## What we watch

| Metric | Source | Alert at |
|---|---|---|
| `/status` HTTP 200 | external blackbox prober | 3 consecutive failures |
| Last `grok-pull` UTC | filesystem mtime on manifest | > 30 min stale |
| Last ingest UTC | `/status` | > 30 min behind grok-pull |
| CPU 5-min avg | CloudWatch | > 80% for 15 min |
| EBS IOPS used | CloudWatch | > 5000 for 5 min |
| Disk free | node-exporter | < 20% |
| 5xx rate | nginx access log | > 1% |
| p95 tool latency | structlog → log-based metric | > 500ms |
| Rate-limit rejections | nginx | > 100/min (abuse signal) |

## Stack

- **Logs:** structlog in Python, `tracing` in Rust (both emit
  JSON). Forward to CloudWatch Logs.
- **Metrics:** CloudWatch native + a thin Prometheus exporter
  (uvicorn middleware) for custom counters.
- **Uptime:** a single external prober (Better Stack / UptimeRobot
  free tier) hitting `/status`.
- **Dashboards:** CloudWatch dashboard with the table above.
- **Paging:** PagerDuty on 5xx + uptime. Everything else Slack
  notifier.

## Non-alerts (visibility only)

- Query tier utilization (which tier served each query — we want
  to learn the real distribution).
- Top N queries by frequency. If any one query dominates, add a
  preset cache.
- Per-tool QPS and p50/p95/p99.
- Per-list message-count growth.

## On-call runbook

A [`runbook.md`](./runbook.md) lives adjacent to this doc.
Required sections:
- How to reset the index from the compressed store.
- How to manually re-run ingestion with verbose logging.
- How to roll back after a bad deploy.
- How to rate-limit an abusive IP manually.
- Who to contact at kernel.org if lore manifest becomes
  unreachable (Konstantin Ryabitsev, per
  `../research/lore-infrastructure.md`).
