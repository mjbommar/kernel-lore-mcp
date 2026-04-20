# kernel-lore-mcp

Free (MIT) MCP server exposing structured search over the Linux
kernel mailing list archives at
[lore.kernel.org](https://lore.kernel.org) to LLM-backed developer
tools — Claude Code, Codex, Cursor, Zed, anything else that speaks
the Model Context Protocol.

**No authentication, ever.** No API keys, no OAuth, no login flow.
Same anonymous posture on every deployment — local, hosted,
everywhere. Every agent that asks us a question is one fewer
agent scraping lore directly; fanout-to-one is the value
proposition.

## Quick start

Install is one command. The *first sync* is where real time goes —
budget honestly depending on what you want to cover:

| Shape | Disk | First-sync wall-clock |
|---|---|---|
| 1–2 small lists (`wireguard`, `xdp-newbies`) | ~1 GB | 1–5 min |
| Subsystem slice (lkml + netdev + linux-cifs) | ~25 GB | 15–60 min |
| Full lore (390 shards, every list) | ~100 GB | 4–12 h |

Steady-state syncs on the 5-min timer after cold-start are seconds.

```sh
# 1. install — one command, pre-built abi3 wheel, no Rust toolchain required
uv tool install kernel-lore-mcp

# 2. first sync — manifest fetch + gix fetch + ingest in one process
#    under one writer lock. Pick a small slice for a first experiment:
export KLMCP_DATA_DIR=~/klmcp-data
mkdir -p "$KLMCP_DATA_DIR"
kernel-lore-sync \
    --data-dir "$KLMCP_DATA_DIR" \
    --with-over \
    --include '/wireguard/*' --include '/linux-cifs/*'
# Drop --include to mirror all ~390 lists. Plan the disk + time.

# 3. (optional, recommended) build the path-mention index. Tiny, fast.
python -c 'from kernel_lore_mcp import _core; \
           print(_core.rebuild_path_vocab("'"$KLMCP_DATA_DIR"'"))'

# 4. confirm freshness + which capabilities are provisioned
kernel-lore-mcp status --data-dir "$KLMCP_DATA_DIR"
# Look at `capabilities`: each over_db / bm25 / path_vocab / embedding /
# maintainers / git_sidecar boolean tells you which tools will actually
# return data on this deployment.

# 5. verify the MCP surface — zero API cost
git clone --depth 1 https://github.com/mjbommar/kernel-lore-mcp.git
cd kernel-lore-mcp && ./scripts/agentic_smoke.sh local
# PASS: 6/6 tools, 5/5 resource templates, 5/5 prompts.
```

Then pick your agent and copy its snippet from
[`docs/mcp/client-config.md`](./docs/mcp/client-config.md). All four
clients (Claude Code, Codex, Cursor, Zed) work over stdio against
the exact same server binary.

### Optional capabilities — opt in when you need them

The baseline sync gives you everything a typical query asks for.
Three tiers are explicitly opt-in because they cost disk or time
and not every deployment wants them:

| Capability | Build | When you want it |
|---|---|---|
| BM25 prose search (`b:` / free text) | `kernel-lore-ingest --rebuild-bm25` | semantic-free text search over prose bodies |
| Semantic embeddings (`lore_nearest`, `lore_similar`) | `kernel-lore-embed --data-dir $KLMCP_DATA_DIR` | "more like this" / free-text → vector ANN |
| Git-sidecar (authoritative `merged` + `picked_up`) | `kernel-lore-build-git-sidecar --repo linux-stable --path /path/to/linux-stable.git` | upgrades `lore_stable_backport_status` + `lore_thread_state` from lore heuristic to git-history truth |
| MAINTAINERS snapshot | drop a `MAINTAINERS` file into `$KLMCP_DATA_DIR` or point `$KLMCP_MAINTAINERS_FILE` at one | `lore_maintainer_profile` declared-vs-observed ownership |

`kernel-lore-mcp status` reports which are ready via the
`capabilities` field, and tools that need an un-provisioned tier
return a `setup_required` error naming the exact command to fix it
(no silent empty results).

### Install from source

Contributing? Building a custom binary?

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain stable
git clone https://github.com/mjbommar/kernel-lore-mcp.git
cd kernel-lore-mcp
uv sync
uv run maturin develop --release
cargo build --release --bin kernel-lore-sync --bin kernel-lore-ingest
./target/release/kernel-lore-sync --data-dir $KLMCP_DATA_DIR --with-over
```

### Going bigger

Want fuller coverage? Drop `--include` flags to mirror all ~390
lists (~100+ GB first run).

Want production-grade systemd deployment (single `klmcp-sync.timer`
replacing the pre-v0.2.0 grokmirror + ingest pair)?
[`docs/ops/runbook.md`](./docs/ops/runbook.md) §1 onwards.

## Status — April 2026

Shipped:

- Ingest pipeline — gix + mail-parser + metadata/trigram/BM25/
  embedding tiers. Incremental; dangling-OID safe; single-writer
  flock.
- **v0.2.0 `kernel-lore-sync`** — one Rust binary that internalized
  the legacy `grokmirror` + separate-ingest two-process chain.
  HTTPS manifest fetch, gix smart-HTTP clone-or-fetch (rayon-
  fanned across shards), ingest, tid rebuild, generation bump —
  all under one writer lock so there's no trigger/debounce race.
- Full MCP surface: 19 tools (search, primitives, sampling-backed
  summarize/classify/explain), 5 RFC-6570 resource templates, 5
  slash-command prompts, populated KWIC snippets, freshness
  marker on every response. (HMAC-signed pagination cursors are
  designed but not yet wired through tool responses — v0.2.0.)
- stdio + Streamable HTTP transports; no SSE.
- `/status` + `/metrics` (Prometheus) with freshness_ok signal.
- systemd units for hosted deploy; 5-min `klmcp-sync.timer`
  cadence (docs/ops/update-frequency.md).
- Live-tested against real `claude --print` and `codex exec`
  every commit via `scripts/agentic_smoke.sh`.

Deferred past v1: trained kernel-specific retrieval model
([`docs/research/training-retriever.md`](./docs/research/training-retriever.md)),
cross-list maintainers graph, CVE-chain tool, Patchwork state
integration (all planned; see
[`docs/plans/2026-04-14-best-in-class-kernel-mcp.md`](./docs/plans/2026-04-14-best-in-class-kernel-mcp.md)).

## Why

Linux kernel development lives on ~390 public mailing lists. `lei`
and `b4` work well for humans with terminals, but LLM-backed
developer tools have no equivalent: they can't answer "who touched
`fs/smb/server/smbacl.c` in the last 90 days, grouped by series,
with trailers" or "has this XDR overflow pattern been reported
before" without being fed curated context by hand.

This project closes that gap. One MCP server over the full corpus,
so an agent working on kernel code has the same research surface a
senior maintainer has. And because it's all mirrored + indexed
once, every agent query is zero HTTP load on lore.kernel.org.

## Architecture in one paragraph

Four-tier index plus an embedding tier, purpose-built per query
class: **columnar metadata** (Arrow/Parquet) for analytical scans;
**SQLite `over.db`** (public-inbox pattern) for sub-millisecond
metadata point lookups and predicate scans; **trigram** (`fst` +
`roaring`) for patch/diff content with DFA-only regex confirmation;
**BM25** (tantivy) for prose; **semantic** (HNSW via
instant-distance) for "more like this." Rust core via
PyO3 0.28 does the heavy lifting; Python + FastMCP 3.2 serves
MCP over stdio + Streamable HTTP. Ingestion is incremental from
public-inbox git shards pulled via `kernel-lore-sync` (gix smart-
HTTP + lore manifest-diff), replacing the pre-v0.2.0 grokmirror
dependency. The
zstd-compressed raw store is the source of truth; all four
tiers rebuild from it.

## North star: a trained kernel retriever

The Parquet metadata tier captures the training signal for free —
subject/body pairs, series version chains, `Fixes:` → target SHA,
reply graphs via `in_reply_to` / `references`, trailer co-occurrence.
A future phase trains a <200 MB int8-quantized CPU-inferable
retriever on that self-supervised signal. Recipe:
[`docs/research/training-retriever.md`](./docs/research/training-retriever.md).

## Documentation

- [`CLAUDE.md`](./CLAUDE.md) — authoritative project state +
  non-negotiable product constraints
- [`docs/ops/runbook.md`](./docs/ops/runbook.md) — local dev (§0A)
  + hosted deploy (§1+)
- [`docs/ops/update-frequency.md`](./docs/ops/update-frequency.md) —
  5-min cadence policy + fanout-to-one cost analysis
- [`docs/mcp/client-config.md`](./docs/mcp/client-config.md) —
  copy-paste snippets for Claude Code, Codex, Cursor, Zed
- [`docs/mcp/transport-auth.md`](./docs/mcp/transport-auth.md) —
  transport + why no auth
- [`docs/architecture/`](./docs/architecture/) — design rationale
- [`docs/plans/2026-04-14-best-in-class-kernel-mcp.md`](./docs/plans/2026-04-14-best-in-class-kernel-mcp.md) —
  6-month roadmap
- [`docs/research/`](./docs/research/) — dated investigations that
  fed the plan

## License

MIT. See [`LICENSE`](./LICENSE).

Data from lore.kernel.org is re-hosted under the same terms as
lore itself (public archive). Attribution preserved in every
response. Redaction policy: [`LEGAL.md`](./LEGAL.md).
