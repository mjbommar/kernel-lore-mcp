# kernel-lore-mcp — Kernel-developer workflow gap analysis

**Date:** 2026-04-14
**Scope:** What does a kernel maintainer / contributor / security researcher
actually need on a normal day that the current server does not provide?
**Inputs:** `CLAUDE.md`, `docs/plans/2026-04-15-mcp-spec-coverage-and-uplift.md`,
the kernel-network-simulatory canonical-user CLAUDE.md, plus web research.

The current server does one thing well: structured search + retrieval over
`lore.kernel.org` public-inbox archives ingested via grokmirror. A real
kernel researcher's day — as captured in the simulatory project — spans at
least eight other data surfaces. Below is the inventory, classification,
effort, and agentic value for each, followed by a "ship next" list.

Legend:
- **Shape**: `tool` / `resource` / `prompt` / `not-MCP` (a CLI or doc we reference but never wrap).
- **Data**: `pub-scrape` (HTML/JSON, no auth) / `pub-api` (official REST, no auth) / `pub-api-key` (requires key) / `embargoed` / `local-install` (requires a binary/clone on the host).
- **Cost**: h / d / w (hours, days, weeks).
- **AV**: agentic value, 1–5, where 5 = used on most sessions.

---

## 1. Read-side workflows we're missing

### 1.1 Source-tree and maintainer metadata

| # | Item | Shape | Data | Cost | AV | Notes |
|---|---|---|---|---|---|---|
| 1 | **MAINTAINERS file parser** — `lore_maintainer(file=, function=)` returns the `M:/R:/L:/S:/T:/F:/K:/N:/X:` block, current mtime + churn, and the list of Acked-by-ing humans in the last 90 days for that block. | tool + resource (`lore://maintainer/{file}`) | pub-scrape (live from `torvalds/linux:MAINTAINERS` — plain text, simple line-record format) | 2–3 d | 5 | Every security-report and every patch submission starts with `get_maintainer.pl`. Our `lore_activity` already knows who touched a file; this closes the loop to "who is officially on the hook." |
| 2 | **get_maintainer.pl equivalence** — run against a diff or a file list, return the same recipient set. | tool | local-install (script is `scripts/get_maintainer.pl`, Perl) OR reimplement F:/X:/N:/K: rules in Rust | 3–4 d (reimpl) | 5 | Having this in-server means the agent can propose `--to/--cc` lists without shelling out. Shell-out path acceptable behind a feature flag. |
| 3 | **checkpatch.pl wrapper** — run checkpatch over a candidate patch, return structured findings `[{level, line, rule, msg}]`. | tool | local-install | 1–2 d | 4 | Relevant mainly for the drafting half of the workflow. |
| 4 | **MAINTAINERS churn / ownership mtime** — who currently owns ksmbd? = recent signoffs + most-recent MAINTAINERS edit for that block. | tool | pub-scrape + our own metadata tier | 1 d | 4 | Already half-built in `lore_activity`; needs MAINTAINERS tie-in. |

### 1.2 Patch-series lifecycle (`b4`, `format-patch`, `send-email`, `patchwork`)

| # | Item | Shape | Data | Cost | AV | Notes |
|---|---|---|---|---|---|---|
| 5 | **b4 mbox-bundle retrieval** — given a cover message-id, return the mbox exactly as `b4 am` would build it. | tool | pub-scrape (mirrors `b4 am` against a public-inbox) | 1 d | 5 | `b4` is the universal patch workflow tool. |
| 6 | **b4 attestation check** — patatt + DKIM signature verification state. | tool | pub-scrape | 2 d | 3 | Nice-to-have for maintainers. |
| 7 | **Patchwork state lookup** — given a message-id, return `{state: Accepted/Rejected/Under Review/Changes Requested/..., delegate, series_id, checks[]}`. | tool + resource (`lore://patchwork/{msg_id}`) | pub-api (`patchwork.kernel.org/api/` v1.2, JSON, no auth for reads) | 2–3 d | 5 | Single biggest hole. "Did my patch land?" asked daily; lore cannot answer it. |
| 8 | **patch-id + cherry tracking** — given a message-id, compute `git patch-id --stable` and search every mirrored mainline / stable / subsystem tree. | tool | pub-api + local gix | 3–5 d | 5 | Directly supports the "is this already fixed?" novelty check. Uses `git-patch-id` semantics. |
| 9 | **series shape verification** — is `[PATCH 0/N]` complete? are all `N/N` present? does interdiff exist for vN→vN+1? | tool | local (metadata tier has `series_index`, `series_version`) | 1 d | 4 | Partially there via `lore_series_timeline`; output schema needs explicit gap surfacing. |

### 1.3 Upstream / subsystem / stable trees

| # | Item | Shape | Data | Cost | AV | Notes |
|---|---|---|---|---|---|---|
| 10 | **Tree-aware commit lookup** — `lore_commit(sha=, tree=mainline\|linux-next\|stable-6.x\|cel-linux\|cifs-2.6\|ksmbd)` → commit metadata + per-tree containment. | tool | local-install (shallow grokmirror of `gregkh/linux`, `next/linux-next`, subsystem remotes) | 1 w (ingest) + 2–3 d (query) | 5 | The canonical-user already maintains `/nas4/data/workspace-infosec/subsystem-trees/`. |
| 11 | **AUTOSEL candidate check** — has Sasha's AUTOSEL queued or rejected this mainline SHA for stable? | tool | pub-scrape (Sasha's `autosel` on `git.sr.ht/~sashal/autosel` + linux-stable list) | 3–4 d | 4 | Important for "is my fix backported?" |
| 12 | **git-log --grep / -G / --pickaxe** over a mirrored tree. | tool | local (gix) | 2 d | 4 | Trigram index today is over lore; tree-index is natural. |
| 13 | **Backport-diff** — given mainline fix X and stable branch Y, return the cherry-picked commit (or "not backported") and any fix-up delta. | tool | local | 3 d | 5 | Canonical-user does this by hand. |

### 1.4 CVE / vulnerability

| # | Item | Shape | Data | Cost | AV | Notes |
|---|---|---|---|---|---|---|
| 14 | **CVE → introducing + fixing commit + backport table** — `lore_cve_chain(CVE-YYYY-NNNN)` returning `{fix_sha, introducing_sha, mainline_in: tag, backports: {linux-6.1.y: sha, ...}, announce_msgid}`. | tool + resource | pub-api + pub-scrape (`lore.kernel.org/linux-cve-announce`; `nluedtke/linux_kernel_cves` JSON; cve.org JSON5) | 3–5 d | 5 | Headline user-visible feature. |
| 15 | **Distro tracker mirrors** — RH, SUSE, Ubuntu USN/CVE, Debian. "Is this CVE fixed in $distro?" | tool | pub-api (RHSA JSON, SUSE SMASH, Ubuntu CVE Tracker JSON, Debian tracker) | 1 w | 3 | Lower value (escapes lore) but unique — nobody else stitches these. |
| 16 | **Embargo-aware blind-spot surfacing** — extend `blind-spots://coverage` with per-query hints: "you just asked about $subsystem; security@kernel.org traffic is not in our corpus." | resource | local | 2 h | 4 | Cheap; policy win. |

### 1.5 Crash reports, fuzzing, CI

| # | Item | Shape | Data | Cost | AV | Notes |
|---|---|---|---|---|---|---|
| 17 | **syzbot bug state** — `{status, commits[], repro_c, repro_syz, crash_log, first_seen, last_seen}`. | tool + resource | pub-scrape — `syzkaller.appspot.com` has no documented public JSON but deterministic HTML + `lore.kernel.org/all/?q=syzbot` | 1 w | 5 | ~40% of kernel bug reports route through syzbot. |
| 18 | **Crash dump parser** — normalize KASAN/KMSAN/KCSAN/UBSAN/KFENCE splats + panic/oops headers into `{type, symbol, offset, access_size, allocated_by, freed_by, pc, stack[]}`. | tool | local (regex/parser; same shape syzkaller uses in `pkg/report/linux.go`) | 3–5 d | 5 | Every security report pastes a KASAN splat. |
| 19 | **syzkaller reproducer extraction** — pull inlined syz-prog or C reproducer from a lore message. | tool | local | 1 d | 4 | Pairs with 18. |
| 20 | **KernelCI / CKI / LKFT status for a commit or patch** — via shared KCIDB schema + BigQuery or per-project REST. | tool | pub-api (`api.kernelci.org`, `qa-reports.linaro.org`) | 1 w | 4 | "Did this series break any CI?" |
| 21 | **Intel 0-day bot / lkp-tests reports** — `kernel test robot` postings. | tool | our corpus (needs a tag facet) | 2 d | 3 | Cheap. |
| 22 | **oss-fuzz + Coverity kernel summaries** | not-MCP | pub-scrape but coarse | — | 1 | Skip. |

### 1.6 Regression tracking and bug trackers

| # | Item | Shape | Data | Cost | AV | Notes |
|---|---|---|---|---|---|---|
| 23 | **regzbot state** — any `#regzbot` command; cross-ref `linux-regtracking.leemhuis.info`. | tool + resource (`lore://regression/{id}`) | our corpus + pub-scrape | 2 d | 4 | Metadata tier has `link:`/`closes:` trailers; regzbot is the other half. |
| 24 | **bugzilla.kernel.org bug** — fetch via Bugzilla REST (JSON). | tool + resource (`lore://bug/{id}`) | pub-api (Bugzilla REST, JSON) | 2 d | 3 | Low traffic; canonical for ACPI, drm. |

### 1.7 Cross-reference, static nav, and context

| # | Item | Shape | Data | Cost | AV | Notes |
|---|---|---|---|---|---|---|
| 25 | **Bootlin Elixir proxy** — `lore_xref(identifier, version=)` returns definition + callers. | tool + resource | pub-api (Elixir REST, v2.0+) | 1 d | 5 | Single cheapest high-value integration. |
| 26 | **cregit blame** — commit-aware token-level blame. | tool | pub-scrape (cregit static HTML) | 3 d | 3 | Niche but unique. |
| 27 | **cscope / ctags over a local tree** | tool | local-install | 2 d | 2 | Skip in favor of Elixir. |
| 28 | **LWN kernel-index cross-ref** — files/functions/topics → LWN articles. | tool + resource | pub-scrape (static index pages; paywall for full articles) | 2 d | 3 | Title scraping + local cache is fine. |
| 29 | **kernelnewbies dictionary / DevelopmentStatistics** | resource | pub-scrape | 2 h | 2 | Trivial. |

### 1.8 Ad-hoc artifacts on lists (`ftrace`, `perf`, `gdb`, `dmesg`, `crash`, `kdump`)

| # | Item | Shape | Data | Cost | AV | Notes |
|---|---|---|---|---|---|---|
| 30 | **Artifact classifier on message attachments** — tag each message as carrying `ftrace-trace`, `perf-report`, `gdb-log`, `dmesg`, `coredump`, `kdump-backtrace`, `strace`. | tool (metadata enrichment) | local | 2–3 d | 4 | Enables `lore_search` filter "only reports that include a repro artifact." |
| 31 | **ftrace/perf/gdb parsers** — per-artifact structured extraction. | tool | local | 1–2 w per artifact | 2 | Defer; stay with classifier + raw text until demand. |

---

## 2. Write-side workflows (read-only discovery only)

| # | Item | Shape | Data | Cost | AV | Notes |
|---|---|---|---|---|---|---|
| 32 | **Patchwork delegate/reviewer suggestion** — for a file set, list humans with the highest Reviewed-by rate in last 12 months. | tool | our corpus | 1 d | 5 | Straight extension of `lore_activity`. |
| 33 | **Cover-letter template seed** (from similar accepted series) | prompt | our corpus | 2 h | 3 | Fits as `@mcp.prompt`. |
| 34 | **vger list inventory** — list-of-lists + moderator + archive URL. | resource (`lore://lists`) | pub-scrape (`vger.kernel.org/vger-lists.html`) | 2 h | 3 | |
| 35 | **git-format-patch series-shape linter** — confirm `--cover-letter`, `--thread`, `--base=` tags. | tool | local | 1 d | 3 | Pairs with 9. |

---

## 3. Adjacent research support

| # | Item | Shape | Data | Cost | AV | Notes |
|---|---|---|---|---|---|---|
| 36 | **Regression-fix finder** — given culprit SHA, find every patch citing it in `Fixes:` or bisect output. | tool | our corpus (`fixes:` trailer already indexed) | 0.5 d | 5 | Cheapest on the list; already latent in schema. |
| 37 | **Bisect-result miner** — pull bisect logs out of messages (pattern-match `# first bad commit`) as structured records. | tool | our corpus | 2 d | 4 | |
| 38 | **Who-fixed-the-regression** — 36 + regzbot (23) + PR-state (7). | prompt | composition | 0.5 d | 5 | Orchestration of primitives — Phase 11 prompt surface. |
| 39 | **Cross-discipline blast-radius** — "is this XDR overflow pattern in my NFS series also present in sunrpc/SCSI/RDMA?" | prompt | composition of `lore_regex` + Elixir | 0.5 d | 5 | Canonical-user's single most-valued workflow. |

---

## 4. Prioritization summary

Counting Shape × AV × (5 − cost-weeks):

- `tool` + AV=5 + ≤1w: **1, 7, 8, 14, 17, 25, 36, 38, 39** → ship first.
- `prompt` + AV=5 + ≤1d: **33, 38, 39** → ride the Phase 11 train.
- `resource` + AV≥3: **1, 7, 14, 17, 23, 24, 25, 28, 34** → one-liners after Phase 10.

---

## Top 10 to ship next

1. **MAINTAINERS + `lore_maintainer()` tool** (Item 1)
2. **Patchwork state lookup** (Item 7) — biggest hole; free REST, no auth.
3. **CVE chain tool** (Item 14) — headline demo, extends `lore_expand_citation`.
4. **patch-id-aware cherry tracker across mainline + stable + subsystem trees** (Item 8)
5. **syzbot state wrapper + KASAN/KMSAN/etc. crash-parser** (Items 17 + 18)
6. **Bootlin Elixir xref tool** (Item 25) — trivial wrap, unique value.
7. **Regression-fix finder + regzbot state** (Items 23 + 36) — latent in schema.
8. **b4-mbox retrieval + series-shape verification** (Items 5 + 9)
9. **Reviewer-recommendation tool** (Item 32)
10. **Cross-discipline prompt pair** (Items 38 + 39) — zero new infra.

All ten are `tool` or `resource` or `prompt`, all have public or already-local
data, and all fit inside two sprints alongside the Phase 10–17 plan. The only
one requiring meaningful new ingest is subsystem-tree mirror (Item 10), for
which the canonical user already has a proven shell script.

---

## Sources

- `docs.kernel.org/process/maintainers.html` — MAINTAINERS format
- `github.com/torvalds/linux/blob/master/scripts/get_maintainer.pl`
- `b4.docs.kernel.org/en/latest/maintainer/am-shazam.html`
- `patchwork.readthedocs.io/en/latest/api/rest/` — Patchwork REST (no-auth reads)
- `patchwork.kernel.org/api/` — kernel.org patchwork instance
- `kernel.org/pub/software/scm/git/docs/git-patch-id.html`
- `kernel.org/pub/software/scm/git/docs/git-format-patch.html`
- `docs.kernel.org/process/cve.html` + `lore.kernel.org/linux-cve-announce`
- `github.com/nluedtke/linux_kernel_cves` — CVE→commit JSON
- `github.com/google/syzkaller/blob/master/docs/syzbot.md`
- `github.com/google/syzkaller/blob/master/pkg/report/linux.go` — KASAN parser reference
- `github.com/google/syzkaller/blob/master/docs/reproducing_crashes.md`
- `api.kernelci.org/intro.html`
- `lkft.linaro.org/about/`
- `lists.linaro.org` AUTOSEL announce thread; `git.sr.ht/~sashal/autosel`
- `kernel.org/doc/html/next/process/handling-regressions.html`; `gitlab.com/knurd42/regzbot`; `linux-regtracking.leemhuis.info`
- `bugzilla.kernel.org` + Bugzilla REST docs
- `github.com/bootlin/elixir`; bootlin blog on Elixir v2.0/2.1 REST
- `lwn.net/Kernel/Index/`
- `vger.kernel.org/vger-lists.html`; `subspace.kernel.org/vger.kernel.org.html`
