# kernel-lore-mcp

Public MCP + REST server exposing fast, structured search over the
Linux kernel mailing list archives at
[lore.kernel.org](https://lore.kernel.org) to LLM-backed developer
tools — Claude Code, Codex, Cursor, and anything else that speaks
the Model Context Protocol.

## Status

Early scaffold. Not yet operational. See [`CLAUDE.md`](./CLAUDE.md)
for the current project state and [`docs/`](./docs) for the full
design.

## Why

Linux kernel development lives on ~350 public mailing lists. Tools
like `lei` and `b4` do a great job for humans with terminals, but
LLM-backed developer tools have no equivalent: they can't answer
"who fixed a bug in `ksmbd_alloc_user` in the last six months" or
"has anyone touched `arch/um/drivers/vector_kern.c` on linux-um"
without being fed curated context by hand.

This project closes that gap. One MCP server, one REST API, over
the full corpus — so an agent working on kernel code has the same
research surface a senior maintainer has.

## Architecture in one paragraph

Three-tier search index, purpose-built per query class:
**columnar metadata** (Arrow/Parquet) for structured fields,
**trigram** for patch and code content, **BM25** for prose. Rust
core (via PyO3) does the heavy lifting; Python serves MCP over
Streamable HTTP + a parallel FastAPI REST surface. Ingestion is
incremental from `grokmirror`-managed public-inbox git shards via
`gix`. See [`docs/architecture/`](./docs/architecture/) for the
full design rationale.

## Getting started

*Not yet runnable.* Once the scaffold solidifies:

```bash
# dev setup
uv sync
uv run maturin develop

# serve locally over stdio (for Claude Code / Cursor local config)
uv run kernel-lore-mcp --transport stdio

# serve over HTTP (for hosted deploy)
uv run kernel-lore-mcp --transport http --host 0.0.0.0 --port 8080
```

## Documentation

- [`CLAUDE.md`](./CLAUDE.md) — top-level project state and proscriptions
- [`docs/architecture/`](./docs/architecture/) — design rationale
- [`docs/ingestion/`](./docs/ingestion/) — how data flows in
- [`docs/indexing/`](./docs/indexing/) — the three tiers, tokenizer spec
- [`docs/mcp/`](./docs/mcp/) — MCP tool schemas and query routing
- [`docs/ops/`](./docs/ops/) — EC2 sizing, cost model, update cadence
- [`docs/research/`](./docs/research/) — April 2026 investigations
  that produced the current design

## License

MIT. See [`LICENSE`](./LICENSE).

Data from lore.kernel.org is re-hosted under the same terms as
lore itself (public archive). Attribution to lore.kernel.org is
preserved in all responses.
