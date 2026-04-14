# Governance

## Model

**BDFL** (Benevolent Dictator For Life). Michael Bommarito
(michael@bommaritollc.com) is the project lead until further
notice. Final call on design decisions, pin bumps, proscription
changes ([`CLAUDE.md`](./CLAUDE.md)), and the hosted-instance
policy surface ([`LEGAL.md`](./LEGAL.md),
[`SECURITY.md`](./SECURITY.md)).

This is a deliberate choice for v1. The project is small, the
design space is deep, and the discipline around "do not
over-engineer, do not under-engineer" survives better under a
single decider than under consensus-by-committee.

## Contributions

- MIT-licensed. PRs welcomed via GitHub
  ([mjbommar/kernel-lore-mcp](https://github.com/mjbommar/kernel-lore-mcp)).
- Before sending a PR: read the relevant standards under
  [`docs/standards/`](./docs/standards/) and the checklist that
  matches your change class
  ([Python](./docs/standards/python/checklists/),
  [Rust](./docs/standards/rust/checklists/)).
- Proscription changes
  ([`CLAUDE.md`](./CLAUDE.md) "What NOT to use" +
  tokenizer rules) need explicit lead sign-off and their own
  commit — do not bury them in a feature PR.

## Bus-factor plan

If the lead is unavailable or unwilling to continue, and no named
successor has taken over, the project reverts to a fiscal host
(placeholder: **Software Freedom Conservancy** or a similar
501(c)(3) that accepts kernel-adjacent OSS projects). The MIT
license guarantees that forks remain unencumbered regardless.

The hosted public instance is a separate concern from the code
repo: it can wind down independently (shut off DNS + archive
snapshot bundles) without affecting self-hosters.

## Cross-references

- [`CLAUDE.md`](./CLAUDE.md)
- [`LEGAL.md`](./LEGAL.md)
- [`SECURITY.md`](./SECURITY.md)
