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
4. **`lore_activity` at file granularity is gated behind a free
   API key.** The tool's full surface (who-touched-what by file,
   grouped by series, with trailer chains) is a scraping
   amplifier if left wide-open; we rate-limit it per-key. The
   key is free — fill a form, receive a key, no approval queue.
   Coarser-granularity queries (list-wide, thread-wide) stay
   anonymous.

## Same binary, runtime switch

Mode selection lives in `kernel_lore_mcp.config` (pydantic-settings,
env-driven). Proposed:

```
KLMCP_MODE=local     # default
KLMCP_MODE=hosted    # enables embargo quarantine + key-gated tools
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
