# Standards

Engineering standards for `kernel-lore-mcp`. Two parallel trees:

- [`python/`](./python/) — Python 3.12+ standards covering the MCP
  surface, pydantic models, FastMCP server, and the Python side of
  the PyO3 boundary.
- [`rust/`](./rust/) — Rust 1.85+ standards covering the native core
  (ingestion, indexing, query routing) and the Rust side of the
  PyO3 boundary.

These are adapted from the KAOS module standards at
`273v/kaos-modules/docs/python/` and reshaped for the
kernel-lore-mcp context (MCP server, tantivy, gix, lore archives).
Where a KAOS rule carries unchanged, we cite it. Where it diverges,
we say why.

## How to use

- **Before writing Python code:** read `python/index.md` + the
  guide most relevant to your change (language, code-quality,
  testing, etc.).
- **Before writing Rust code:** read `rust/index.md` + the
  relevant guide.
- **Before committing:** run the pre-commit checks from
  `python/code-quality.md` and `rust/code-quality.md`.
- **Before a PR:** walk the checklist in `*/checklists/` that
  matches your change class.

## Authority

When these standards disagree with `../../CLAUDE.md`, CLAUDE.md
wins — it's the project-specific authority. The standards are the
shared floor; CLAUDE.md is the project contract on top.

## Drift discipline

If you're tempted to break a standard, either:
1. Document the exception in the affected file with a `--> reason`
   pointer, or
2. Update the standard (with a commit that says why).

Silent drift is how conventions die.
