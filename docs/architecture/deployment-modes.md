# Deployment modes

Two postures. One binary. Features gated at runtime by mode, not
by build.

## Mode 1: local self-host (primary)

The design target. Anyone can run `kernel_lore_mcp` against their
own grokmirror-managed shards (or against a snapshot bundle we
publish — see
[`reciprocity.md`](./reciprocity.md)). Zero policy constraints
imposed by us. The operator decides.

- Default bind `127.0.0.1` (see
  [`../../CLAUDE.md`](../../CLAUDE.md) operational contract).
- No authentication required; the operator layers auth if they
  expose the HTTP surface.
- No quarantine, no redaction-propagation SLA beyond "reindex
  when you feel like it."
- Logs go where structlog is configured to send them — the
  operator decides retention.
- stdio transport is the typical integration path for Claude
  Code / Codex / Cursor running on the same box.

Primary user: a kernel contributor or security researcher who
wants this running under their own control, on their own hardware,
against their own lore mirror.

## Mode 2: hosted public instance (on the roadmap)

A single public instance we operate. Free. Streamable HTTP only.
Accepts stdio-style MCP over HTTP from remote clients.

**The hosted instance uses no authentication.** No API keys, no
OAuth, no bearer tokens — same posture as local self-host. See
CLAUDE.md § "Non-negotiable product constraints" for the reasoning:
every agent pointed at kernel-lore-mcp is one fewer agent scraping
lore directly, and the whole point of hosting it is to make
integration frictionless.

Extra runtime policy gates on top of the local posture:

1. **Embargo quarantine.** Messages matching the rules in
   [`../../SECURITY.md`](../../SECURITY.md) are held from responses
   for 72 h after ingest. The underlying compressed store still
   contains them; only the response surface filters.
2. **Query non-logging.** HTTP access logs keep path + status +
   timing only. Query strings scrubbed at the structlog processor
   level before serialization. Policy:
   [`../../LEGAL.md`](../../LEGAL.md). Threat:
   [`../ops/threat-model.md`](../ops/threat-model.md) (category:
   reviewer-interest leak).
3. **Redaction honoring.** Lore-upstream redactions propagate on
   next reindex. Direct redaction requests processed within 72 h
   of receipt ([`../../LEGAL.md`](../../LEGAL.md)).
4. **Per-IP rate limits.** nginx `limit_req_zone` at 60/min/ip,
   `burst=30 nodelay`. Same limit for every caller. IPv6 truncated
   to /64 so /128 sweeps don't trivially escape the quota. Abusive
   sources get absorbed by the limit — no key to revoke, no
   partner tier to gate, no business logic keyed on identity.
   The rate limit is generous by design: fanout-to-one means we're
   replacing N lore-scraping agents with 1 MCP request each, so
   our aggregate cost to the kernel infrastructure goes DOWN as
   adoption goes up.

## Same binary, runtime switch

Mode selection lives in `kernel_lore_mcp.config` (pydantic-settings,
env-driven). Proposed:

```
KLMCP_MODE=local     # default
KLMCP_MODE=hosted    # enables embargo quarantine + rate limits
```

No feature flag hides the code paths. A hosted instance that
misconfigures to `local` still gets the `127.0.0.1` bind default
as a safety net; exposing it publicly takes an explicit
`KLMCP_BIND=0.0.0.0`.

## What is NOT mode-gated

- The ingest pipeline. Both modes ingest identically.
- The compressed raw store format. Snapshot bundles are portable
  across modes.
- The three-tier index schemas. See
  [`../../src/schema.rs`](../../src/schema.rs).
- The tokenizer. See
  [`../indexing/tokenizer-spec.md`](../indexing/tokenizer-spec.md).
- The query grammar.

Gating is a response-surface concern only. The underlying data
layout is identical.

## Cross-references

- [`../../CLAUDE.md`](../../CLAUDE.md)
- [`../../LEGAL.md`](../../LEGAL.md)
- [`../../SECURITY.md`](../../SECURITY.md)
- [`./reciprocity.md`](./reciprocity.md)
- [`../ops/threat-model.md`](../ops/threat-model.md)
