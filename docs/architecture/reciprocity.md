# Reciprocity with kernel.org

The contract this project keeps with the lore infrastructure it
depends on. Non-optional for the hosted public instance. Strongly
recommended defaults for self-hosters.

## Principle

lore.kernel.org is a public good run by the Linux Foundation. We
consume a lot of it (all of it, eventually). We give back by
being a well-behaved client and by offloading bootstrap fan-out
from lore to us.

## Rate discipline

- **Never hit lore harder than a single well-behaved mirror.**
  Our default is `kernel-lore-sync` on a 5-minute timer, speaking the
  same manifest + fingerprint + delta-fetch protocol as grokmirror.
  We do not ask for exceptions.
- **Honor `Retry-After`.** Any 429 / 503 response from lore or
  a mirror means we wait. No retry-flood.
- **Named User-Agent.** Every outbound request identifies as
  `kernel-lore-mcp/<version> (+https://github.com/mjbommar/kernel-lore-mcp)`.
  Lore ops need to be able to find us if we misbehave.

## Mirror preference

- **Prefer tier-1 mirror** (`erol.kernel.org`) over
  `lore.kernel.org` direct. Lore's front door should be for
  low-volume interactive use; mirrors exist to absorb bulk.
- Configured at the manifest URL / sync-client layer. See `scripts/`
  and `docs/ops/update-frequency.md` for the reference posture.

## Snapshot-bundle distribution

A new self-hoster spinning up `kernel-lore-mcp` should not
re-fetch ~390 shards from lore just to get started. That would
make us a fan-out amplifier on lore's bandwidth.

Plan:

- We publish periodic **derived-index snapshots**: the
  compressed raw store + Parquet metadata tier + (once landed)
  the trigram and BM25 tiers, packaged as a single bundle.
- Snapshots ship from our infrastructure, not lore's. New
  self-hosters bootstrap from us, then catch up the tail with
  `kernel-lore-sync` like everyone else.
- Snapshot cadence target: weekly. Granularity: full corpus +
  deltas since the last full.
- Distribution channel: TBD (likely S3 + IPFS pin). Listed in
  `docs/ops/` once in place.

The corollary: do **not** teach the serving process to bulk-fetch
from lore directly. Mirror/sync traffic belongs in the dedicated
writer path, not in request handling.

## If lore asks us to stop

We stop. Hosted instance goes dark, snapshot bundles come down,
and self-hosters fall back to their own direct sync against lore
(or against mirrors). The MIT license guarantees forks are free
to continue, but this project's hosted posture respects
`security@kernel.org` requests without argument.

## Cross-references

- [`../../CLAUDE.md`](../../CLAUDE.md) — operational contract,
  ingestion pipeline.
- [`./deployment-modes.md`](./deployment-modes.md)
- [`../ingestion/`](../ingestion/) — grokmirror + gix specifics.
- [`../ops/threat-model.md`](../ops/threat-model.md) — category:
  scraping amplifier.
