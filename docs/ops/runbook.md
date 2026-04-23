# Operator runbook

Two mode­s documented here:

- **§0A — Local dev on one laptop.** No systemd, no nginx, no
  service user. ~10-30 minutes from clone to "my agent just cited a
  lore Message-ID." This is where you start.
- **§1 onwards — Hosted / multi-user deployment.** Full systemd,
  sandboxing, rate-limiting, Prometheus alerting. Use this when
  you're deploying to a shared box.

For cadence background read
[`update-frequency.md`](./update-frequency.md).

## 0A. Local dev — run it on your laptop

You want to: point Claude Code / Codex / Cursor at `kernel-lore-mcp`
running locally against a slice of lore, and start asking it
questions. Skip §1+ entirely.

```sh
# 0A.1 — prereqs
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
# No grokmirror install step — v0.2.0's kernel-lore-sync internalizes
# manifest fetch + gix fetch + ingest in one binary. Legacy grokmirror
# path is still documented in §0Z for operators who prefer it.

# 0A.2 — clone + build
git clone https://github.com/mjbommar/kernel-lore-mcp.git
cd kernel-lore-mcp
uv sync
uv run maturin develop --release
cargo build --release \
    --bin kernel-lore-sync \
    --bin kernel-lore-reindex \
    --bin kernel-lore-doctor

# 0A.3 — pick a data dir (any path)
export KLMCP_DATA_DIR=~/klmcp-data
mkdir -p "$KLMCP_DATA_DIR"

# 0A.4 — first sync: fetches lore manifest (~390 shards), filters to
# a smaller slice via --include, clones changed shards, ingests them,
# bumps generation. One atomic process under one writer lock.
# ~10-30 min depending on which lists you include.
./target/release/kernel-lore-sync \
    --data-dir "$KLMCP_DATA_DIR" \
    --with-over \
    --include '/lkml/*' \
    --include '/linux-cifs/*' \
    --include '/netdev/*'
# Drop --include entirely to mirror all 390 shards (will take hours
# and ~100+ GB of disk on the first run).
# Keep BM25 deferred on a serving box unless you have measured that the
# overlap is acceptable. Inline `--with-bm25` is the heavier path;
# prefer `kernel-lore-reindex --tier bm25` off-peak if prose freshness
# matters and you can restart long-lived readers afterward.

# 0A.5 — optional: rebuild slower derived tiers from the local corpus
# without refetching lore.
./target/release/kernel-lore-reindex --data-dir "$KLMCP_DATA_DIR"

# 0A.5b — over.db has already been written incrementally by the
# sync in 0A.4 (because we passed --with-over). If you skipped that
# flag or need to rebuild over.db from metadata Parquet, the
# source-only `kernel-lore-build-over` helper still exists:
#
#   cargo build --release --bin kernel-lore-build-over
#   ./target/release/kernel-lore-build-over --data-dir "$KLMCP_DATA_DIR"
#
# ~2 min per million rows; ~30 min for a full 17.6M-row corpus.
# Atomic via tempfile+rename — safe to ctrl-C.

# 0A.6 — confirm the index is live (no HTTP needed)
./.venv/bin/kernel-lore-mcp status --data-dir "$KLMCP_DATA_DIR"
# Expect: {"generation": >= 1, "freshness_ok": true, ...}
# While a sync is active, status also reports `writer_lock_present`,
# `sync_active`, and the current sync stage from `state/sync.json`.

# 0A.6b — inspect shard/index health. If a prior run left poisoned shard
# repos behind, --heal repairs unborn HEADs in place and removes
# unrecoverable shard repos so the next sync reclones them cleanly.
cargo build --release --bin kernel-lore-doctor
./target/release/kernel-lore-doctor --data-dir "$KLMCP_DATA_DIR"
# Or repair + clean automatically:
./target/release/kernel-lore-doctor --data-dir "$KLMCP_DATA_DIR" --heal

# 0A.7 — sanity-check the MCP surface without burning API tokens
./scripts/agentic_smoke.sh local
# Expect: PASS: local probe — 6/6 tools, 5/5 resource templates,
#                              5/5 prompts.
```

Now wire an agent into this. Copy the appropriate snippet from
[`../mcp/client-config.md`](../mcp/client-config.md) — stdio,
`command = <repo>/.venv/bin/kernel-lore-mcp`, `env = {
KLMCP_DATA_DIR = "..." }`. No auth, no port, no systemd.

### 0B. Keep it fresh

Two approaches for personal dev:

1. **Manual "top-up before I work":** re-run the sync command from
   0A.4 whenever you want a fresh index. Steady-state sync is
   seconds (manifest diff is a single HTTP GET + JSON compare; only
   changed shards fetch).
2. **cron:** add a 5-min cron entry. No grokmirror / trigger file
   dance — one command, one writer lock:

   ```crontab
   */5 * * * * cd /home/you/kernel-lore-mcp && \
       ./target/release/kernel-lore-sync \
           --data-dir /home/you/klmcp-data \
           --with-over \
           --include '/lkml/*' --include '/netdev/*' \
           >> /home/you/klmcp-data/logs/cron.log 2>&1
   ```

If you ever want the full systemd treatment (multi-user box,
monitoring, alerts), proceed to §1.

### 0Z. Legacy grokmirror path (archival only)

The pre-v0.2.0 shape (external grokmirror + separate ingest) is
kept documented in
[`docs/plans/2026-04-15-internalize-grokmirror.md`](../plans/2026-04-15-internalize-grokmirror.md)
and the `scripts/klmcp-grok-pull.sh` / `scripts/post-pull-hook.sh`
helpers still work for historical reference. Prefer
`kernel-lore-sync` for every new deployment.

## 0. What you are deploying

Two systemd units that together keep a local `lore.kernel.org`
mirror + index + MCP server running:

- `klmcp-sync.timer` → `klmcp-sync.service` — one binary that
  fetches the lore manifest, diffs against local fingerprints,
  gix-fetches changed shards, ingests them, writes `over.db`, and
  bumps the generation marker under one writer lock. Default cadence
  5 min. Slower derived rebuilds such as BM25 stay explicit via
  `kernel-lore-reindex`.
- `klmcp-mcp.service` — serves MCP over stdio or Streamable HTTP.

Everything is anonymous read-only. No API keys, no OAuth, no login.
See CLAUDE.md § "Non-negotiable product constraints" for why.

## 1. Install prerequisites

```sh
sudo apt-get install -y \
    git python3-venv python3-pip build-essential curl nginx
# No grokmirror package needed — kernel-lore-sync does manifest
# fetch + git fetch in-process via ureq + gix.
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
cargo build --release \
    --bin kernel-lore-sync \
    --bin kernel-lore-reindex \
    --bin kernel-lore-doctor
sudo install -o root -g root -m 0755 \
    .venv/bin/kernel-lore-mcp /usr/local/bin/
sudo install -o root -g root -m 0755 \
    target/release/kernel-lore-sync /usr/local/bin/
sudo install -o root -g root -m 0755 \
    target/release/kernel-lore-reindex /usr/local/bin/
sudo install -o root -g root -m 0755 \
    target/release/kernel-lore-doctor /usr/local/bin/
```

## 3. Drop the scripts + systemd units

```sh
# v0.2.0 units: klmcp-sync.{service,timer} + klmcp-mcp.service.
# The legacy klmcp-grokmirror.* + klmcp-ingest.{path,service} files
# still ship for operators who opt into §0Z's path.
sudo install -o root -g root -m 0644 \
    scripts/systemd/klmcp-sync.service \
    scripts/systemd/klmcp-sync.timer \
    scripts/systemd/klmcp-mcp.service \
    /etc/systemd/system/
sudo install -o root -g kernel-lore-mcp -m 0640 \
    scripts/systemd/etc-kernel-lore-mcp-env.sample \
    /etc/kernel-lore-mcp/env
sudoedit /etc/kernel-lore-mcp/env
```

Generate the cursor HMAC key (server-side secret; callers never see
it — see [`../mcp/transport-auth.md`](../mcp/transport-auth.md)):

```sh
openssl rand -hex 32
# paste into /etc/kernel-lore-mcp/env as KLMCP_CURSOR_KEY=...
```

## 4. Enable + start

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now klmcp-sync.timer
sudo systemctl enable --now klmcp-mcp.service
```

First sync fires 60 s after service start. Cold-start takes
~30–60 min depending on network + disk. Set `KLMCP_MODE=hosted`,
`KLMCP_BIND`, and `KLMCP_PORT` in `/etc/kernel-lore-mcp/env` before
you expose the HTTP service. The timer keeps firing
during cold-start but the `flock` in
`state.rs::acquire_writer_lock` guarantees single-writer safety —
any overlapping invocation fails the lock and exits cleanly
without touching state.

## 5. Verify

```sh
# Timer active + next trigger listed:
systemctl list-timers klmcp-sync.timer

# One-shot sync (for debugging / forced refresh):
sudo systemctl start klmcp-sync.service
journalctl -u klmcp-sync.service -f   # follow the structured JSON log stream

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
2. **Manifest fetch or shard fetch failing.** Check
   `journalctl -u klmcp-sync.service --since "30 min ago"` for
   upstream/network errors.
3. **Poisoned local shard repo.** Run
   `sudo -u kernel-lore-mcp /usr/local/bin/kernel-lore-doctor --data-dir /var/lib/kernel-lore-mcp`
   and, if needed, rerun with `--heal`, then start
   `klmcp-sync.service` again.
4. **Clock skew on the box.** `timedatectl status`. Freshness math
   uses `datetime.now(UTC)`; a clock drifted >15 min will
   falsely alert.

## 8. Rotate the cursor HMAC key

```sh
sudoedit /etc/kernel-lore-mcp/env
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
# The next sync tops up the delta.
```

## 10. Graceful shutdown + restart

```sh
# Drain MCP, stop the timer, let ingest finish.
sudo systemctl stop klmcp-mcp.service
sudo systemctl stop klmcp-sync.timer
while pgrep -u kernel-lore-mcp kernel-lore-sync >/dev/null; do
    sleep 1
done
```

Reverse for restart.

## 10A. Build / rebuild `over.db`

`over.db` is the SQLite metadata point-lookup tier (see
[`../architecture/over-db.md`](../architecture/over-db.md)). It is
a pure projection of the metadata Parquet — Parquet is the source
of truth — so it's always safe to delete and rebuild.

```sh
# Local dev:
./target/release/kernel-lore-build-over --data-dir "$KLMCP_DATA_DIR"

# Hosted:
sudo -u kernel-lore-mcp /usr/local/bin/kernel-lore-build-over \
    --data-dir /var/lib/kernel-lore-mcp
```

| Property | Value |
|---|---|
| Wall-clock (full 17.6M-row corpus) | ~30 min |
| Throughput | ~2 min per million rows |
| Disk footprint | ~19 GB for 17.6M messages (~1.1 KB/row including indices) |
| Atomicity | Builds to `over.db.tmp.<run_id>`, atomic rename on success. Crash leaves the tempfile behind for inspection — no half-written `over.db`. |
| Fallback when absent | Reader paths fall through to legacy Parquet scans (slow but correct). |

**When to rebuild:**

1. **Schema migration.** `OverDb::SCHEMA_VERSION` bump; the
   Reader refuses to open a stale DB and the build is the
   migration.
2. **File corruption.** `sqlite3 over.db "PRAGMA integrity_check"`
   reports anything other than `ok`. Just `rm over.db` and rerun
   the build — Parquet is the source of truth, no data loss.
3. **Forced rebuild for performance.** If you've heavily mutated
   the corpus (e.g. re-ingested with a fresh schema), a clean
   rebuild reclaims space that incremental writes leave fragmented.

**Incremental sync writes to over.db automatically when
`kernel-lore-sync --with-over` is used** (or when `over.db` already
exists and auto-detection keeps the tier on). No separate cron or
timer entry is required for steady-state metadata freshness — the
rebuild step above is only needed for the cases listed.

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
sudo systemctl start klmcp-sync.service             # kick a fresh tick
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
