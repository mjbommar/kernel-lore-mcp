# Security policy

## Reporting a vulnerability

Email **michael@bommaritollc.com** with `[kernel-lore-mcp
security]` in the subject. PGP key: _placeholder — to be published
at `https://bommaritollc.com/pgp.asc` before the hosted instance
goes live_. Expect an acknowledgment within 72 hours.

Do not open a public GitHub issue for a vulnerability report. Use
email; we will cut a GitHub security advisory once we have a fix
candidate.

## Scope

**In scope:**

- The server code in `src/` (Rust core + Python MCP surface).
- The ingest pipeline (`src/ingest.rs`, `src/parse.rs`,
  `src/store.rs`, `src/metadata.rs`).
- Query routing and the MCP tool surface once landed
  (`src/router.rs`, `src/kernel_lore_mcp/tools/`).
- The hosted public instance we operate (when it exists).
- Snapshot bundles we publish for self-host bootstrap.

**Out of scope:**

- `lore.kernel.org` itself. Report lore-side issues to
  `security@kernel.org` — kernel.org's own security team owns
  that surface. We are downstream.
- Third-party self-hosted deployments we do not operate. Contact
  the operator.
- Denial-of-service via volume against the hosted instance — we
  rate-limit at the edge; report traffic patterns not a DoS.

## Embargo-aware posture

The hosted instance ships with a quarantine policy for messages
that are likely to be still-embargoed:

- Any message whose `Fixes:` trailer references a commit younger
  than 7 days.
- Any message whose subject matches `CVE-YYYY-\d+`.

Such messages are held from hosted-instance responses for **72 h
after ingest**. The underlying compressed store still contains
them; only the query response surface filters. Self-hosters can
disable the quarantine via config. See
[`docs/ops/threat-model.md`](./docs/ops/threat-model.md) (category:
embargo leakage).

The threshold is deliberately conservative. If 72 h is wrong for
a specific class of disclosure (e.g. hardware vendor embargoes
that routinely run 90+ days), email us and we will widen it.

## Coordinated disclosure timeline

- **T+0** — report received; we acknowledge within 72 h.
- **T+7 d** — we confirm reproducibility and scope.
- **T+30 d** — target for patch availability. If a patch requires
  longer (e.g. a tantivy or gix upstream fix), we extend with
  reporter agreement.
- **T+90 d** — hard disclosure cap absent reporter-agreed
  extension.

Credit is given in the advisory unless the reporter requests
otherwise.

## Cross-references

- [`CLAUDE.md`](./CLAUDE.md)
- [`LEGAL.md`](./LEGAL.md)
- [`docs/ops/threat-model.md`](./docs/ops/threat-model.md)
- [`docs/architecture/deployment-modes.md`](./docs/architecture/deployment-modes.md)
