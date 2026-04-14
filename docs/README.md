# Documentation

Start here.

| Section | Read when |
|---|---|
| [architecture/](./architecture/) | You want the *why* — index design, data flow, trade-offs. |
| [ingestion/](./ingestion/) | You're touching how data gets from lore into the store. |
| [indexing/](./indexing/) | You're touching one of the three index tiers or the tokenizer. |
| [mcp/](./mcp/) | You're adding an MCP tool or REST endpoint, or changing query routing. |
| [ops/](./ops/) | You're deploying, sizing, or monitoring the server. |
| [research/](./research/) | You're re-evaluating a past decision. Every doc here cites April 2026 sources. |

The root of the project has:

- [`../CLAUDE.md`](../CLAUDE.md) — authoritative, terse project state
  and proscriptions. Read first.
- [`../README.md`](../README.md) — public-facing pitch.

## Discipline

1. Every significant decision lives in exactly one place.
   Cross-link; do not duplicate.
2. When you change the code, update the doc in the same commit.
   Doc drift is a bug.
3. `docs/research/` is append-only history. Never delete; annotate
   as superseded with a pointer forward.
4. If a doc doesn't fit the taxonomy, update the taxonomy.
   Scattering is the enemy.
