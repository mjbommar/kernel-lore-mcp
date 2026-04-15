# Update frequency — 5-minute cadence is the answer

**Status:** authoritative. Referenced from `CLAUDE.md`,
[`deployment-modes.md`](../architecture/deployment-modes.md),
[`threat-model.md`](./threat-model.md),
[`reciprocity.md`](../architecture/reciprocity.md).

**TL;DR:** ingest every 5 minutes via `grokmirror`. Costs us
~20 GB/month of egress from kernel.org, ≤0.2% of one vCPU, ~12 MB/day
of local disk. Costs kernel.org under 0.2% of their monthly lore
egress. In exchange, a single well-adopted agent integration
replaces our entire daily cost many times over by subtracting direct
lore-scraping traffic.

## Why the question matters

Update cadence affects three things the server's value depends on:

1. **Freshness** (what the blind-spots resource promises callers).
2. **Fanout-to-one math** (whether we actually subtract load from
   lore, or whether our own pull cost erodes the savings).
3. **Operational budget** (egress, CPU, disk on the deploy box).

Picking the right number is a product decision, not an engineering
one. This doc pins the number + the reasoning so it doesn't drift.

## Corpus scale (April 2026 baseline)

| Signal | Value | Source |
|---|---|---|
| Lists tracked | ~300 | `lore.kernel.org/manifest.js.gz` |
| Full packed corpus | ~55–60 GB | grokmirror manifest, empirical |
| Messages in corpus | ~8M | rolling count across linux-* + subsystem lists |
| Daily message growth | ~5k–8k msgs/day | `lore.kernel.org/all/?x=A` rate |
| Per-message compressed size | ~2–8 KB | public-inbox v2 avg |
| Peak subsystem commit rate | net/mm ~1–3 commits/min | push bursts during merge windows |

## What `grok-pull` actually does every tick

Per the [grokmirror protocol docs](https://korg.docs.kernel.org/grokmirror.html):

1. **Fetch `manifest.js.gz`** from `https://lore.kernel.org/manifest.js.gz`
   (~2 MB gzipped). Standard HTTP cache-friendly.
2. **Diff against local state.** Each shard carries a `fingerprint`
   in the manifest. Unchanged fingerprints → skip entirely (no git
   fetch). Typical tick: 0–5 changed shards out of 300.
3. **For changed shards only**, `git fetch` over `git://lore.kernel.org/...`.
   Smart protocol + packfiles means each fetch is a tiny delta.

This is **pull-only, delta-only, push-compatible with kernel.org infra**.
It is the exact protocol Konstantin Ryabitsev (kernel.org infra lead)
built grokmirror for — mirrors like us are the target audience.

## Per-tick cost breakdown at 5-min cadence

### Network egress from kernel.org

- Manifest: **~2 MB/tick gzipped** (always).
- Changed-shard packfiles: **~50–200 KB/tick avg, ~500 KB/tick peak**.
- Total per tick: **~2–3 MB avg, ~5 MB peak**.
- Per day: 288 ticks × avg = **~600–900 MB/day**.
- Per month: **~20 GB egress from kernel.org**.

For scale: lore.kernel.org serves well over 10 TB/month from the HTTP
surface alone. Our pull is **<0.2% of their monthly egress**.

### Our CPU + IO per tick

- Manifest diff: ~50 ms (gzip + JSON parse).
- git fetch per changed shard: ~100–300 ms (network-bound).
- Ingest per new message: ~8–15 ms (mail-parser → schema → Parquet).
- Avg tick: ~17 new messages → **~200–500 ms total ingest CPU**.
- Idle tick (no changed shards): **~50 ms of stat + JSON parse**.

**≤0.5 s of one vCPU per tick = ~0.2% utilization on one core.**
Negligible on r7g.xlarge; fine on a Raspberry Pi.

### Our disk growth

- git shards: ~10 MB/day aggregate (delta-packed).
- Parquet metadata: ~300 KB/day.
- Trigram postings: ~1 MB/day.
- BM25 tantivy tier: ~500 KB/day.
- **Total: ~12 MB/day, ~4 GB/year.** Fits easily on the gp3 16000/1000
  we already sized in CLAUDE.md's operational contract.

### Freshness achieved

Lag chain:

| Hop | Contribution |
|---|---|
| vger → lore | 1–5 min (fixed, kernel.org infra) |
| lore → our grok-pull | 0–5 min (tick jitter) |
| grok-pull → indexed + visible to readers | ≤1 min (ingest pipeline + reader reload) |

- **p50 end-to-end: ~5 min.** (3 vger + 2 tick + <1 process.)
- **p95 end-to-end: ~11 min.**

This is dramatically better than Sprint 0's stated "10–20 min ingest
tail." The `blind-spots://coverage` resource text should be updated to
reflect the new number.

## Alternatives evaluated

| Cadence / protocol | Our cost / tick | Freshness p95 | Verdict |
|---|---|---|---|
| **grokmirror 5 min** | ~3 MB, ~0.5 s CPU | ~11 min | **ship** |
| grokmirror 1 min | ~3 MB × 5 | ~7 min | reject — 5× TCP sessions, no product win |
| grokmirror 15 min | ~3 MB / 3 | ~25 min | reject — freshness hurts security workflows |
| HTML scrape `/?x=A` every 5 min | ~hundreds of MB | ~11 min | reject — hits lore's web surface |
| NNTP pull | comparable | ~11 min | acceptable alt, no win over git protocol |
| pubsub (grokmirror v3) | ~0 idle, ~KB on event | ~30 s | future — not a Sprint-0 dep |

## Why 5 minutes is the defensible default

1. **Kernel.org's own recommendation.** grokmirror's default cron
   interval, as shipped by Konstantin, is 5-min-class.
2. **Fingerprint cache hit rate is maximal at this cadence.** Most
   ticks have 0 changed shards → pure manifest fetch → a single 2 MB
   transfer that's HTTP-cacheable.
3. **Freshness promise is honest.** "≤5 min p50, ≤11 min p95" is a
   number we can put in `blind-spots://coverage` and hit reliably.
4. **Fanout math is wildly positive.** A single agent integration
   doing novelty-check workflows typically makes 50–500 lore HTTP
   requests/hour that would otherwise hit the web surface. At 5-min
   grokmirror cadence, our total daily lore cost is ~800 MB. **One
   agent workload replaces our entire daily cost many times over.**
5. **Below 5 min is a shared-resource politeness problem** without a
   corresponding product win. The 3 min p50 vs 5 min p50 delta does
   not change any security-research workflow outcome.

## The fanout-to-one math

This is the key number to keep front of mind:

> Every agent pointed at kernel-lore-mcp is one fewer agent scraping
> `lore.kernel.org` directly. Our 5-min grokmirror pull is a **fixed
> cost that amortizes over every query we answer**.

A back-of-envelope model:

- Our fixed cost to kernel.org: ~20 GB/month egress, zero web-surface
  traffic.
- Typical agent doing a kernel-research workflow: 50–500 HTTP hits to
  `lore.kernel.org/<list>/?q=...` per hour of active use.
- Break-even: **one agent running at ~4 hr/week on kernel-lore-mcp
  replaces our entire monthly grokmirror cost**.
- At 10 active agent integrations, we're a net **~10× reduction** in
  lore web-surface traffic versus the no-MCP counterfactual.
- At 100 integrations (hosted-instance scale), we're a net
  **~100× reduction**.

This inverts the standard "mirroring adds load" intuition. Because
grokmirror is delta-only and HTTP-free, and because the queries we
answer are the ones agents would otherwise send to lore's HTTP
surface one-by-one, adoption of kernel-lore-mcp is
**monotonically-decreasing lore traffic**.

## Operator knobs

- `KLMCP_GROKMIRROR_INTERVAL_SECONDS` — cadence (default `300`).
  Self-hosters may tighten this for their own corpus if they want
  lower lag on specific lists; the hosted instance runs 300 by
  policy.
- `KLMCP_INGEST_DEBOUNCE_SECONDS` — minimum gap between ingest
  runs, regardless of grok-pull trigger rate (default `30`). Prevents
  overlapping writers from fighting over the flock.
- `KLMCP_INGEST_CONCURRENCY` — rayon worker count for the shard
  walker (default `min(8, num_cores)`). Tune down on small boxes.

None of these affect the caller-facing MCP surface.

## Policy guardrails

- **Do not go below 5 min on the hosted instance** without
  coordinating with kernel.org infra. Self-hosters can do whatever
  they want for their own mirror.
- **Do not go above 15 min on the hosted instance.** "Fresh enough
  to be useful for security-research workflows" is the product
  promise.
- **Never fall back to HTML scraping.** If grokmirror is failing,
  the answer is to fix the grokmirror link, not to hit the web
  surface.
- **Never advertise an auth-gated tier with higher cadence.** Same
  public read-only posture everywhere; see CLAUDE.md §
  "Non-negotiable product constraints."

## References

- [grokmirror — korg docs](https://korg.docs.kernel.org/grokmirror.html)
- [grokmirror — GitHub](https://github.com/mricon/grokmirror)
- [public-inbox v2 format](https://public-inbox.org/public-inbox-v2-format.txt)
- [lore.kernel.org FAQ — mirror yourself, don't scrape](https://www.kernel.org/lore.html)
- [`../../CLAUDE.md`](../../CLAUDE.md) § "Non-negotiable product constraints"
- [`../architecture/deployment-modes.md`](../architecture/deployment-modes.md)
- [`../architecture/reciprocity.md`](../architecture/reciprocity.md)
- [`./threat-model.md`](./threat-model.md)
