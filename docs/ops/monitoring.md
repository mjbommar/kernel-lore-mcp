# Ops — monitoring

Single-box, so keep this simple.

## What we watch

| Metric | Source | Alert at |
|---|---|---|
| `/status` HTTP 200 | external blackbox prober | 3 consecutive failures |
| `kernel_lore_mcp_last_ingest_age_seconds` | `/metrics` | > 900 on a 5 min box |
| `kernel_lore_mcp_freshness_ok` | `/metrics` | 0 for 10 min |
| `kernel_lore_mcp_sync_active` | `/metrics` | stuck at 1 for unexpectedly long runs |
| CPU 5-min avg | CloudWatch | > 80% for 15 min |
| EBS IOPS used | CloudWatch | > 5000 for 5 min |
| Disk free | node-exporter | < 20% |
| 5xx rate | reverse-proxy or app logs | > 1% |
| `kernel_lore_mcp_tool_latency_seconds` p95 | `/metrics` | > 500ms sustained |
| `kernel_lore_mcp_tool_calls_total{status="rate_limited"}` | `/metrics` | unexpected sustained rise |

## Stack

- **Logs:** structlog in Python, `tracing` in Rust (both emit
  JSON). Forward to CloudWatch Logs.
- **Metrics:** built-in Prometheus exposition at `/metrics`, plus
  whatever host-level metrics stack you already run.
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
- How to manually re-run `kernel-lore-sync` with verbose logging.
- How to roll back after a bad deploy.
- How to rate-limit an abusive IP manually.
- Who to contact at kernel.org if lore manifest becomes
  unreachable (Konstantin Ryabitsev, per
  `../research/lore-infrastructure.md`).
