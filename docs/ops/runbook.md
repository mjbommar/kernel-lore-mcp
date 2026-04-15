# Operator runbook

One page. Self-contained. Assumes Ubuntu 24.04 / Debian 12 /
Rocky 9. For cadence background, read
[`update-frequency.md`](./update-frequency.md) first.

## 0. What you are deploying

Three systemd units that together keep a local `lore.kernel.org`
mirror + three-tier index + MCP server running:

- `klmcp-grokmirror.timer` → `klmcp-grokmirror.service` — pulls
  via grokmirror every 5 minutes.
- `klmcp-ingest.path` → `klmcp-ingest.service` — re-ingests on
  every successful pull (debounced 30 s).
- `klmcp-mcp.service` — serves MCP over stdio or Streamable HTTP.

Everything is anonymous read-only. No API keys, no OAuth, no login.
See CLAUDE.md § "Non-negotiable product constraints" for why.

## 1. Install prerequisites

```sh
sudo apt-get install -y \
    git python3-venv python3-pip build-essential curl \
    grokmirror nginx
sudo useradd --system --home /var/lib/kernel-lore-mcp \
    --shell /usr/sbin/nologin kernel-lore-mcp
sudo install -d -o kernel-lore-mcp -g kernel-lore-mcp -m 0755 \
    /var/lib/kernel-lore-mcp /etc/kernel-lore-mcp \
    /usr/local/lib/kernel-lore-mcp
```

## 2. Install the server binary

```sh
git clone https://github.com/mjbommar/kernel-lore-mcp.git
cd kernel-lore-mcp
uv sync
uv run maturin develop --release
sudo install -o root -g root -m 0755 \
    .venv/bin/kernel-lore-mcp /usr/local/bin/
sudo install -o root -g root -m 0755 \
    .venv/bin/kernel-lore-ingest /usr/local/bin/
```

## 3. Drop the scripts + systemd units

```sh
sudo install -o root -g root -m 0755 \
    scripts/klmcp-grok-pull.sh \
    scripts/klmcp-ingest.sh \
    scripts/post-pull-hook.sh \
    /usr/local/lib/kernel-lore-mcp/
sudo install -o root -g root -m 0644 \
    scripts/grokmirror.conf \
    /etc/kernel-lore-mcp/grokmirror.conf
sudo install -o root -g root -m 0644 \
    scripts/systemd/klmcp-grokmirror.service \
    scripts/systemd/klmcp-grokmirror.timer \
    scripts/systemd/klmcp-ingest.service \
    scripts/systemd/klmcp-ingest.path \
    scripts/systemd/klmcp-mcp.service \
    /etc/systemd/system/
sudo install -o root -g kernel-lore-mcp -m 0640 \
    scripts/systemd/etc-kernel-lore-mcp-env.sample \
    /etc/kernel-lore-mcp/env
sudoedit /etc/kernel-lore-mcp/env  # set KLMCP_CURSOR_KEY at minimum
```

Generate the cursor HMAC key (server-side secret; callers never see
it — see [`../mcp/transport-auth.md`](../mcp/transport-auth.md)):

```sh
openssl rand -hex 32
# paste into /etc/kernel-lore-mcp/env:KLMCP_CURSOR_KEY
```

## 4. Enable + start

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now klmcp-grokmirror.timer
sudo systemctl enable --now klmcp-ingest.path
sudo systemctl enable --now klmcp-mcp.service
```

First grokmirror pull fires 60 s after service start; the first
ingest fires right after. Cold-start takes ~30–60 min depending on
network + disk. The timer keeps firing during cold-start but the
ingest debounce prevents overlap, and the `flock` in
`state.rs::acquire_writer_lock` guarantees single-writer safety.

## 5. Verify

```sh
# Timer active + next trigger listed:
systemctl list-timers klmcp-grokmirror.timer

# MCP server up + generation advanced:
curl -s http://127.0.0.1:8080/status | jq .
# Expect: generation >= 1, freshness_ok: true,
#         configured_interval_seconds: 300,
#         last_ingest_age_seconds < 900.

# Prometheus metrics:
curl -s http://127.0.0.1:8080/metrics | grep kernel_lore_mcp_
```

## 6. Alerting

Suggested Prometheus rules:

```yaml
- alert: KernelLoreFreshnessDegraded
  expr: kernel_lore_mcp_freshness_ok == 0
  for: 10m
  annotations:
    summary: Ingest lag > 3x the configured cadence
    runbook: https://.../docs/ops/runbook.md#7-freshness-degraded
- alert: KernelLoreIngestStuck
  expr: kernel_lore_mcp_last_ingest_age_seconds > 3600
  for: 5m
- alert: KernelLoreServerDown
  expr: up{job="kernel-lore-mcp"} == 0
  for: 2m
```

## 7. Freshness degraded

Likely causes, in order of frequency:

1. **Disk full.** `df -h /var/lib/kernel-lore-mcp`. Snapshot-bundle
   restore if under 10 GB free.
2. **grokmirror network-blocked.** Check
   `journalctl -u klmcp-grokmirror.service --since "30 min ago"`.
3. **Ingest stuck on a poison shard.** Check
   `journalctl -u klmcp-ingest.service --since "1 hour ago"` for
   stack traces. Manually retry via
   `sudo touch /var/lib/kernel-lore-mcp/state/grokpull.trigger`.
4. **Clock skew on the box.** `timedatectl status`. Freshness math
   uses `datetime.now(UTC)`; a clock drifted >15 min will
   falsely alert.

## 8. Rotate the cursor HMAC key

```sh
openssl rand -hex 32 | sudo -u kernel-lore-mcp tee \
    /etc/kernel-lore-mcp/env.new >/dev/null
sudo install -o root -g kernel-lore-mcp -m 0640 \
    /etc/kernel-lore-mcp/env.new /etc/kernel-lore-mcp/env
sudo systemctl restart klmcp-mcp.service
```

Rotation invalidates all in-flight pagination cursors; clients
retry from page 1. Cheap.

## 9. Cold-start from a snapshot

Skip the 30–60 min cold-start by fetching a snapshot bundle from
the hosted instance (schedule in
[`../architecture/reciprocity.md`](../architecture/reciprocity.md)):

```sh
curl -o /tmp/klmcp-snapshot.tar.zst \
    https://kernel-lore-mcp.example.org/snapshots/latest.tar.zst
sudo -u kernel-lore-mcp tar -xf /tmp/klmcp-snapshot.tar.zst \
    -C /var/lib/kernel-lore-mcp/
sudo systemctl restart klmcp-mcp.service
# The next grokmirror pull tops up the delta.
```

## 10. Graceful shutdown + restart

```sh
# Drain MCP, stop the timer, let ingest finish.
sudo systemctl stop klmcp-mcp.service
sudo systemctl stop klmcp-ingest.path
sudo systemctl stop klmcp-grokmirror.timer
while pgrep -u kernel-lore-mcp kernel-lore-ingest >/dev/null; do
    sleep 1
done
sudo systemctl stop klmcp-grokmirror.service
```

Reverse for restart.

## 11. Recover from a mid-ingest crash

The `flock` on `state/writer.lock` is released by the kernel when
the process exits, so a dead ingest does not leave a stuck lock.
Atomic writes (tempfile + rename) mean `state/generation` and every
`state/shards/*.oid` file is either the pre-crash value or the
post-crash value — never half-written.

Verify after a crash:

```sh
cat /var/lib/kernel-lore-mcp/state/generation       # parses as u64
ls  /var/lib/kernel-lore-mcp/state/shards/*/*.oid   # every file is
                                                    # 40 hex chars
sudo systemctl start klmcp-grokmirror.service       # kick a fresh tick
```

If a shard was repacked upstream and our recorded OID has gone
dangling, the walker auto-falls-back to a full re-walk (pinned in
`src/ingest.rs::dangling_oid_falls_back_to_full_rewalk`). Storage
gets a duplicate row set for that shard; the reader picks the
freshest row for each Message-ID. The cost is bounded and benign.

## 12. What we will never make you do

- Generate an API key.
- Sign up for an account.
- Authenticate callers.
- Rate-limit callers by identity.
- Rotate a caller-side secret.

Every one of those is ruled out by CLAUDE.md §
"Non-negotiable product constraints" and
[`../mcp/transport-auth.md`](../mcp/transport-auth.md). If you see
a doc that contradicts this, flag it.

## 13. Contact points

- Konstantin Ryabitsev (lore/public-inbox maintainer): via
  people.kernel.org, or `Cc: konstantin@linuxfoundation.org`.
- This project: michael@bommaritollc.com.
- Issues / PRs: <https://github.com/mjbommar/kernel-lore-mcp>.
