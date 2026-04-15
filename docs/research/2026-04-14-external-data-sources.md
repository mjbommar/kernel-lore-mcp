# kernel-lore-mcp — External data source catalog + 6-month roadmap

**Date:** 2026-04-14
**Scope:** Every kernel-adjacent data source we could integrate beyond lore.
Research-only; no code changes.

Scale: **Cost** L/M/H ≈ hours / days / weeks. **Value** 1–5 = fraction of
kernel-research sessions that benefit. **Mode** = `ingest` / `proxy` / `link`.

---

## Tier 1 — Git trees

All kernel.org git lives on `git.kernel.org` / `pub/scm/linux/kernel/git/`.
Sanctioned automation: **grokmirror**.

| Source | Shape | Fresh | Cost | Value | Mode |
|---|---|---|---|---|---|
| `linux.git` (mainline) | git | minutes | L | 5 | ingest (grokmirror) |
| `linux-next` | git | daily | L | 4 | ingest |
| `linux-stable` | git | per release | L | 4 | ingest |
| Subsystem trees (~200: net-next, mm, kvm, …) | git | varies | M | 3 | ingest via MANIFEST |
| sourceware / chromium mirrors | git | varies | L | 1 | link |

## Tier 2 — Bug / vuln / regression trackers

### syzbot dashboard (`syzkaller.appspot.com`)
Informal JSON; throttled. Repros + configs + disk images on per-bug pages.
No ToS; App Engine defaults. **Shape:** HTML + informal JSON. **Fresh:** real-time.
**Cost:** M. **Value:** 5. **Mode:** ingest bug metadata hourly, proxy repro assets.

### CVE List V5 (`github.com/CVEProject/cvelistV5`)
Canonical CVE 5.x JSON in git. Updated ~every 7 minutes via CVE Services API;
daily baseline zip + hourly delta zips. **ToU:** attribution required.
**Cost:** L. **Value:** 5. **Mode:** ingest (git clone + `deltaLog.json`).

### openwall oss-security / linux-distros
Public list w/ openwall web archive + MARC mirror + SecLists RSS. Also on
`mirrors.kernel.org/openwall/`. linux-distros is closed; post-embargo → oss-sec.
Wiki is CC-BY-NC-SA 3.0; posts are author copyright. **NOT on lore.**
**Cost:** M. **Value:** 4. **Mode:** ingest (polite crawl) + RSS.

### Red Hat CVE database (`access.redhat.com/security/data`)
CSAF/VEX v2 (current), OSV (mirrored to osv.dev), SBOM (SPDX), legacy OVAL v2,
RHSA RSS, CPE dict. **License:** CC-BY 4.0. **Cost:** L. **Value:** 5. **Mode:** ingest CSAF.

### Ubuntu CVE tracker (`git.launchpad.net/ubuntu-cve-tracker`)
Authoritative data in git. Anonymous clone. Per-CVE flat files + CVE-to-package
status matrix. Launchpadlib only for writing. **Cost:** M (custom parser).
**Value:** 4. **Mode:** ingest.

### Debian security tracker (`salsa.debian.org/security-tracker-team/security-tracker`)
Public git; `data/CVE/list` + `data/DSA/list` plain-text, pre-aggregated JSON at
`security-tracker.debian.org/tracker/data/json` (>10 MB). **Cost:** L. **Value:** 4.

### SUSE security (`suse.com/support/security/csaf/`)
CSAF 2.0 + VEX on FTP, OVAL at `ftp.suse.com/pub/projects/security/oval/`.
**License:** CC-BY 4.0. **Cost:** L. **Value:** 3. **Mode:** ingest.

### bugzilla.kernel.org
Bugzilla REST at `/rest/`. Fronted by **Anubis** anti-bot PoW.
**Cost:** M-H. **Value:** 2 (traffic migrated to lore + syzbot + GitHub).
**Mode:** proxy only, aggressive caching; request allowlist if we ingest.

### regzbot (`gitlab.com/knurd42/regzbot`)
Thorsten Leemhuis's regression tracker. Source on GitLab; data as text artifacts +
tracked-regressions at `linux-regtracking.leemhuis.info`. **Cost:** L. **Value:** 4.

### kernel-newbies wiki
MoinMoin. `known_regressions` stale since 2017. `LinuxVersions` + `LinuxChanges`
still updated per release. CC-BY-SA. **Cost:** L. **Value:** 3 (LinuxChanges only).

## Tier 3 — Patchwork + CI

### patchwork.kernel.org
Django REST at `/api/`: `projects, users, people, patches, covers, series,
events, bundles`. JSON; pagination; anonymous read. Behind Anubis but slow
paginated crawls succeed. States: `New`, `Under Review`, `Accepted`, `Superseded`,
`Rejected`, `RFC`, `Not Applicable`, `Changes Requested`, `Awaiting Upstream`,
`Deferred`. **Cost:** L-M. **Value:** 5. **Mode:** ingest nightly via `events`.

### patchwork.ozlabs.org
Same software, different project set. **Cost:** L once kernel.org done. **Value:** 3.

### KernelCI + KCIDB (`dashboard.kernelci.org`, BigQuery)
Two surfaces: Maestro API + KCIDB (common-schema aggregator in Google BigQuery,
schema v3). ~100k report objects/day from KernelCI, CKI, syzbot, Linaro TuxSuite,
Gentoo, ARM. BigQuery requires GCP project. KCIDB schema Apache-2.0.
**Cost:** M (BigQuery + billing). **Value:** 4. **Mode:** proxy via BigQuery;
optionally ingest daily "failed builds" slice.

### Linaro LKFT (`qa-reports.linaro.org` — SQUAD)
SQUAD REST API at `/api/`, anonymous read. Also submits to KCIDB (dup).
**Cost:** L. **Value:** 3. **Mode:** proxy.

### Red Hat CKI (`datawarehouse.cki-project.org`)
Public warehouse with REST + Grafana. Flows into KCIDB. **Cost:** L. **Value:** 3.

### Intel 0-day / LKP (`lore.kernel.org/lkp/`)
Already mirrored via lore. Dashboard behind Anubis. **Cost:** 0. **Value:** 4 —
expose `lkp` list as first-class data source with parsed build-failure fields.

### syzbot pre-public
**Out of scope** — non-public by design.

## Tier 4 — Coverage / cross-reference

### Elixir Bootlin (`elixir.bootlin.com`)
AGPLv3, self-hostable, Docker-available. **REST API** at
`/api/ident/<Project>/<Ident>?version=<v>&family=<f>` returning defs+refs JSON.
Self-hosting sidesteps rate-limit questions. **Cost:** M (self-host) / L (proxy).
**Value:** 5. **Mode:** self-host alongside kernel-lore-mcp.

### cregit (`cregit.linuxsources.org`)
Token-level blame, updated through 6.18. No API, no explicit license.
Reproducible from scratch. **Cost:** H re-derive / M scrape. **Value:** 3 (overlaps
`git blame --follow`). **Mode:** link-only.

### LXR — superseded by Elixir. **Value:** 1. Link.

### Coverity Scan — ToS forbids redistribution. **Out of bounds.** Link only.

### LWN (`lwn.net`)
Articles are CC-BY-SA **after** 1-week subscriber window. RSS at
`lwn.net/headlines/rss`. Scraping during embargo violates norms. Post-window
text reusable with attribution. **Cost:** M. **Value:** 4 — LWN's kernel index
+ article-to-patch annotations are uniquely valuable. **Mode:** ingest
post-embargo + headlines feed; link-only during embargo. **Operator-side
subscription only** — the LWN credential lives in the server's deployment
env, callers never need an LWN account to query our MCP. See CLAUDE.md §
"Non-negotiable product constraints" point 3.

## Tier 5 — Write-adjacent docs

### MAINTAINERS, Documentation/, htmldocs
Parse from already-mirrored `linux.git`. **Cost:** L parser, 0 hosting.
**Value:** 5 for `maintainers_for(path)` alone. **Mode:** ingest into derived
Parquet rebuilt per commit. **Highest-ROI item in the roadmap.**

### kernel source as corpus
Also free — `linux.git` mirrored. Value depends on query layer (Elixir solves
most).

## Tier 6 — Reference / metadata

### kernel.org release feeds
`feeds/all.atom.xml` + `feeds/kdist.xml`. RSS/Atom. **Cost:** L. **Value:** 3.

### public-inbox manifest + list-metadata
`lore.kernel.org/manifest.js.gz` (already consumed) + per-inbox
`list-metadata.json` (subject tags, list address, description). **Cost:** L.
**Value:** 3. **Mode:** ingest.

---

## Prioritized top-12 — value × 1/cost

Score = `(value × 2) / cost_weight` (L=1, M=2, H=4).

| # | Source | V | C | Score | New tool surface |
|---|---|---|---|---|---|
| 1 | **MAINTAINERS + Documentation/** | 5 | L | 10 | `maintainers_for(path)`, `doc_lookup(topic)` |
| 2 | **CVE List V5** | 5 | L | 10 | `cve(id)`, `cves_touching(file_or_commit)` via `Fixes:` join |
| 3 | **Red Hat CSAF** | 5 | L | 10 | `vendor_advisory(cve, vendor="rhel")`, `cve_status_matrix` |
| 4 | **kernel.org release feeds** | 3 | L | 6 | `latest_releases(branch)` |
| 5 | **patchwork.kernel.org** | 5 | M | 5 | `lore_patch_state(message_id)`, `series_for(cover)` |
| 6 | **syzbot dashboard** | 5 | M | 5 | `syzbot_bug(id)`, `syzbot_search(subsystem)` |
| 7 | **Debian security tracker** | 4 | L | 8 | `vendor_advisory(cve, vendor="debian")` |
| 8 | **Ubuntu CVE tracker** | 4 | M | 4 | `vendor_advisory(cve, vendor="ubuntu")` |
| 9 | **regzbot** | 4 | L | 8 | `regression_status(subject_or_url)` |
| 10 | **Elixir Bootlin (self-host)** | 5 | M | 5 | `symbol_defs`, `symbol_xrefs`, `file_symbols` |
| 11 | **LWN (post-embargo) + RSS** | 4 | M | 4 | `lwn_articles_for(commit_or_symbol)` |
| 12 | **openwall oss-security** | 4 | M | 4 | `disclosure_thread(cve)` |

Runners-up: SUSE CSAF, kernelnewbies LinuxChanges, SQUAD/LKFT, CKI warehouse.

Out-of-bounds: Coverity Scan, syzbot pre-public, bugs.chromium, bugzilla.kernel.org
(Anubis — proxy only).

## 6-month phasing

- **Month 1** — Items 1, 4, 2, 3. Cheapest, highest hit-rate. All pure-git/file. Unlocks "who owns / is there a CVE / which distro fixed it."
- **Month 2** — Items 5 (Patchwork) + 6 (syzbot). Polite crawlers + incremental state. Build shared "external API sink" abstraction. Coordinate with kernel.org infra on Anubis.
- **Month 3** — Items 7, 8, 9. Cross-distro CVE triangulation + regression status. Deliverable: `cve_matrix(id)`.
- **Month 4** — Item 10 (self-host Elixir). Storage review. Unlocks code-aware queries tantivy can't answer.
- **Month 5** — Items 11 + 12. Ethics-sensitive. Build rate-limit/cache/attribution story. Subscribe to LWN first.
- **Month 6** — KCIDB integration (BigQuery proxy), runners-up, and a **reciprocity pass**: push list-metadata improvements upstream, publish grokmirror config, file PRs on public-inbox where we hit scale bugs.

## Reciprocity & ethics

- Prefer APIs over scrapes.
- Respect Anubis on kernel.org; file infra requests, don't solve PoW.
- LWN: corporate subscription; ingest only post-embargo; CC-BY-SA attribution.
- oss-security wiki: CC-BY-NC-SA 3.0; list posts: author copyright — index + excerpts + links, never redistribute full-text API.
- Coverity Scan: do not ingest — ToS.
- syzbot pre-public: hard-forbidden.
