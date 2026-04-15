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

## Quick start (5 minutes, zero accounts)

```sh
# 1. install — one command, pre-built abi3 wheel, no Rust toolchain required
uv tool install kernel-lore-mcp
uv tool install grokmirror

# 2. fetch the scripts + grokmirror configs (they live in the git repo,
#    not in the wheel)
git clone --depth 1 https://github.com/mjbommar/kernel-lore-mcp.git
cd kernel-lore-mcp

# 3. first sync — narrow scope, ~1.5 GB, 3-10 min
export KLMCP_DATA_DIR=~/klmcp-data
mkdir -p "$KLMCP_DATA_DIR"
KLMCP_GROKMIRROR_CONF_TEMPLATE="$PWD/scripts/grokmirror-personal.conf" \
    KLMCP_POST_PULL_HOOK="$PWD/scripts/post-pull-hook.sh" \
    ./scripts/klmcp-grok-pull.sh

# 4. first ingest — ~10-30 min
kernel-lore-ingest --data-dir "$KLMCP_DATA_DIR" \
                   --lore-mirror "$KLMCP_DATA_DIR/shards"

# 5. confirm freshness (no HTTP server needed)
kernel-lore-mcp status --data-dir "$KLMCP_DATA_DIR"
# { "generation": >= 1, "freshness_ok": true, ... }

# 6. verify the MCP surface — zero API cost
./scripts/agentic_smoke.sh local
# PASS: 6/6 tools, 5/5 resource templates, 5/5 prompts.
```

Then pick your agent and copy its snippet from
[`docs/mcp/client-config.md`](./docs/mcp/client-config.md). All four
clients (Claude Code, Codex, Cursor, Zed) work over stdio against
the exact same server binary.

### Install from source

Contributing? Or want the faster rayon-fanout Rust ingest binary?

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain stable
git clone https://github.com/mjbommar/kernel-lore-mcp.git
cd kernel-lore-mcp
uv sync
uv run maturin develop --release
# optional: build the native ingest binary for multi-shard fan-out
cargo build --release --bin kernel-lore-ingest
./target/release/kernel-lore-ingest --data-dir $KLMCP_DATA_DIR \
    --lore-mirror $KLMCP_DATA_DIR/shards
```

### Going bigger

Want fuller coverage? Swap `grokmirror-personal.conf` for
`grokmirror.conf` to mirror all ~390 lists (~55 GB).

Want production-grade systemd deployment?
[`docs/ops/runbook.md`](./docs/ops/runbook.md) §1 onwards.

## Status — April 2026

Shipped:

- Ingest pipeline — gix + mail-parser + metadata/trigram/BM25/
  embedding tiers. Incremental from grokmirror shards; dangling-OID
  safe; single-writer flock.
- Full MCP surface: 19 tools (search, primitives, sampling-backed
  summarize/classify/explain), 5 RFC-6570 resource templates, 5
  slash-command prompts, populated KWIC snippets, freshness
  marker on every response, HMAC-signed pagination cursors.
- stdio + Streamable HTTP transports; no SSE.
- `/status` + `/metrics` (Prometheus) with freshness_ok signal.
- systemd units for hosted deploy; 5-min grokmirror cadence
  (docs/ops/update-frequency.md).
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

Three-tier index plus an embedding tier, purpose-built per query
class: **columnar metadata** (Arrow/Parquet) for structured fields;
**trigram** (`fst` + `roaring`) for patch/diff content with DFA-only
regex confirmation; **BM25** (tantivy) for prose; **semantic**
(HNSW via instant-distance) for "more like this." Rust core via
PyO3 0.28 does the heavy lifting; Python + FastMCP 3.2 serves
MCP over stdio + Streamable HTTP. Ingestion is incremental from
grokmirror-managed public-inbox git shards via gix. The
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
