# kernel-lore-mcp

Free (MIT) MCP server exposing structured search over the Linux
kernel mailing list archives at
[lore.kernel.org](https://lore.kernel.org) to LLM-backed developer
tools — Claude Code, Codex, Cursor, and anything else that speaks
the Model Context Protocol.

## Status — April 2026

- **Phase 1 complete.** The ingest pipeline is real: walk a
  public-inbox shard with `gix`, parse RFC822 with `mail-parser`,
  split prose from patch, extract trailers / subject tags / series
  numbering / touched files, append to the zstd-compressed raw
  store, write the metadata tier as Parquet. See
  [`src/ingest.rs`](./src/ingest.rs), [`src/parse.rs`](./src/parse.rs),
  [`src/store.rs`](./src/store.rs), [`src/metadata.rs`](./src/metadata.rs),
  [`src/schema.rs`](./src/schema.rs). Synthetic fixtures in
  `tests/python/fixtures/` + integration tests cover it.
- **Phase 2 in progress.** Query router + the MCP tool surface
  wired to real data. See [`TODO.md`](./TODO.md).
- **Explicitly deferred past v1:** trigram tier
  ([`src/trigram.rs`](./src/trigram.rs) is a stub), BM25 tier
  ([`src/bm25.rs`](./src/bm25.rs) is a stub), the full MCP tool
  surface, and the trained kernel-specific retrieval model (our
  north star — see
  [`docs/research/training-retriever.md`](./docs/research/training-retriever.md)).

## Deployment modes

One binary, two postures. See
[`docs/architecture/deployment-modes.md`](./docs/architecture/deployment-modes.md).

- **Local self-host (primary).** Anyone can run
  `kernel-lore-mcp` against their own grokmirror-managed shards
  or against a snapshot we publish. Zero policy constraints from
  us; the operator decides.
- **Hosted public instance (on the roadmap).** A free public
  instance we operate, with extra policy gates: embargo
  quarantine, query non-logging, redaction honoring,
  file-granularity `lore_activity` behind a free API key. See
  [`docs/ops/threat-model.md`](./docs/ops/threat-model.md) and
  [`LEGAL.md`](./LEGAL.md).

## Why

Linux kernel development lives on ~390 public mailing lists. Tools
like `lei` and `b4` do a great job for humans with terminals, but
LLM-backed developer tools have no equivalent: they can't answer
"who fixed a bug in `ksmbd_alloc_user` in the last six months" or
"has anyone touched `arch/um/drivers/vector_kern.c` on linux-um"
without being fed curated context by hand.

This project closes that gap. One MCP server over the full corpus,
so an agent working on kernel code has the same research surface a
senior maintainer has.

## Architecture in one paragraph

Three-tier index, purpose-built per query class: **columnar
metadata** (Arrow/Parquet, landed) for structured fields,
**trigram** (planned) for patch content, **BM25** (planned,
tantivy) for prose. Rust core (via PyO3 0.28) does the heavy
lifting; Python serves MCP over Streamable HTTP. Ingestion is
incremental from `grokmirror`-managed public-inbox git shards via
`gix`. The compressed raw store is the source of truth; all three
tiers rebuild from it. See
[`docs/architecture/overview.md`](./docs/architecture/overview.md).

## North star: a trained kernel retriever

v0.5 (now) captures the training signal for free by writing the
right columns to Parquet — subject/body pairs, series version
chains, `Fixes:` → target SHA, reply graphs via `in_reply_to` /
`references`, trailer co-occurrence. v1.1 trains a
<200 MB int8-quantized, CPU-inferable retriever on that self-
supervised signal. Recipe:
[`docs/research/training-retriever.md`](./docs/research/training-retriever.md).

## Getting started (dev)

```bash
uv sync
uv run maturin develop
uv run pytest tests/python -q
```

The MCP server entry point lives at
[`src/kernel_lore_mcp/__main__.py`](./src/kernel_lore_mcp/__main__.py).
Local stdio transport is the only mode wired today; the
streamable-HTTP surface lands with Phase 2.

## Documentation

- [`CLAUDE.md`](./CLAUDE.md) — project proscriptions + current state
- [`TODO.md`](./TODO.md) — execution contract
- [`docs/architecture/`](./docs/architecture/) — design rationale
  including [`deployment-modes.md`](./docs/architecture/deployment-modes.md)
  and [`reciprocity.md`](./docs/architecture/reciprocity.md)
- [`docs/ingestion/`](./docs/ingestion/) — how data flows in
- [`docs/indexing/`](./docs/indexing/) — the three tiers, tokenizer spec
- [`docs/mcp/`](./docs/mcp/) — MCP tool schemas and query routing
- [`docs/ops/`](./docs/ops/) — sizing, freshness, deploy, and
  [`threat-model.md`](./docs/ops/threat-model.md)
- [`docs/research/`](./docs/research/) — dated investigations
- [`docs/standards/`](./docs/standards/) — Python + Rust house style
- [`LEGAL.md`](./LEGAL.md) — re-hosting posture + redaction contact
- [`SECURITY.md`](./SECURITY.md) — responsible disclosure
- [`GOVERNANCE.md`](./GOVERNANCE.md) — who decides what

## License

MIT. See [`LICENSE`](./LICENSE).

Data from lore.kernel.org is re-hosted under the same terms as
lore itself (public archive). Attribution to lore.kernel.org is
preserved in all responses. Redaction policy:
[`LEGAL.md`](./LEGAL.md).
