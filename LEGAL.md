# LEGAL

Re-hosting posture for `kernel-lore-mcp`. Covers both the local
self-host mode (where these are defaults the operator can adjust)
and the hosted public instance we operate.

## Source

All indexed content is derived from
[lore.kernel.org](https://lore.kernel.org) public archives,
fetched via `grokmirror` from public-inbox git shards. Lore is
the Linux Foundation's public, author-attributed mirror of the
kernel mailing lists. Its mirroring stance is the Linux
Foundation's baseline for public-list re-hosting; we inherit it.

## What we re-host

As they appear in the public lore archive:

- Author names, email addresses, and dates.
- Subjects, bodies, patches, diffs.
- Trailer chains: `Signed-off-by:`, `Reviewed-by:`,
  `Tested-by:`, `Acked-by:`, `Co-developed-by:`, `Reported-by:`,
  `Fixes:`, `Link:`, `Closes:`.
- Message-IDs, `In-Reply-To:`, `References:`.

This is the same data lore serves at
`lore.kernel.org/<list>/<message-id>/`. We do not augment it with
external PII sources, and we do not attempt to resolve pseudonyms.

## Redaction

If lore redacts a message upstream, we propagate the redaction on
the next reindex cycle (ingest is incremental; see
[`src/ingest.rs`](./src/ingest.rs) and
[`docs/architecture/reciprocity.md`](./docs/architecture/reciprocity.md)).
If you need a message redacted from the hosted public instance
faster than lore's cadence, or if lore has redacted but we have
not yet caught up:

- Email **michael@bommaritollc.com** with the Message-ID.
- We remove from the hosted instance within 72 hours.
- Local self-hosters remove on their next reindex; we do not
  have a channel to push redactions into third-party deployments.

## Query logging (hosted instance only)

Policy for the hosted public instance we operate:

- **HTTP access logs** keep `path` + `status` + `timing` only.
  No query string, no MCP tool arguments, no request body.
- **Query strings are scrubbed at the structlog processor
  level** before any log record is serialized. See
  [`src/kernel_lore_mcp/logging_.py`](./src/kernel_lore_mcp/logging_.py)
  and
  [`docs/standards/python/libraries/structlog.md`](./docs/standards/python/libraries/structlog.md).
- Scrubbed fields never land in persistent storage (neither
  CloudWatch nor local disk).
- Prometheus metrics are aggregate only: per-tool call counts,
  latency histograms, error counts. No per-query detail.
- The threat model for query logging (reviewer-interest leak)
  is enumerated in
  [`docs/ops/threat-model.md`](./docs/ops/threat-model.md).

Local self-hosters configure their own logging. `KLMCP_BIND`
defaults to `127.0.0.1` for a reason — see
[`CLAUDE.md`](./CLAUDE.md).

## Not covered

This document is not legal advice. It is a statement of
operating policy. If your jurisdiction imposes obligations on us
or on a self-hoster that exceed lore's own posture, those
obligations take precedence over this file.

## Cross-references

- [`CLAUDE.md`](./CLAUDE.md)
- [`SECURITY.md`](./SECURITY.md)
- [`docs/architecture/deployment-modes.md`](./docs/architecture/deployment-modes.md)
- [`docs/architecture/reciprocity.md`](./docs/architecture/reciprocity.md)
- [`docs/ops/threat-model.md`](./docs/ops/threat-model.md)
