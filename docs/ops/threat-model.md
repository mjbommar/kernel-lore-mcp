# Threat model

STRIDE-ish enumeration of the concrete ways `kernel-lore-mcp` can
hurt lore, hurt its users, or be weaponized through LLM callers.
Each category lists: threat, mitigation, **status** (live today
vs pending).

## 1. Scraping amplifier

**Threat.** Our hosted instance becomes a fan-out path that
re-serves lore content at higher aggregate volume than
lore.kernel.org would see directly, or lets callers scrape lore
through us to evade lore-side rate limits.

**Mitigations.**
- Named User-Agent on every outbound request. → _pending (no
  outbound client yet; ingest reads local shards)._
- Never fetch lore directly from the server process; ingest only
  reads from grokmirror-managed local shards. → **live** (no
  HTTP client in the Rust core targets lore; see
  [`../../src/ingest.rs`](../../src/ingest.rs)).
- Publish snapshot bundles so new self-hosters bootstrap from us
  rather than fan out on lore. → _pending_. See
  [`../architecture/reciprocity.md`](../architecture/reciprocity.md).
- Per-IP rate limit on the hosted instance (60/min, IPv6 /64
  bucket). No API-key gate — see CLAUDE.md § "Non-negotiable product
  constraints". → _pending (Phase 2)._

## 2. Rate-limit evasion

**Threat.** Client rotates IPs / User-Agents to blow past
hosted-instance limits.

**Mitigations.**
- fail2ban on 429 signal at nginx. → _pending (Phase 4)._
- Per-query wall-clock cap 5 s (regex DoS + resource). →
  _pending (Phase 4)._
- `lore_activity` at file-granularity uses the same anonymous
  rate limit as every other tool. The threat budget: if someone
  wants the "who touched fs/smb/ in the last 90 days" view at
  scale they will get it from raw lore + a GitHub dump anyway;
  we only lose when we make the legit users' path harder than
  the scraping path. See
  [`../architecture/deployment-modes.md`](../architecture/deployment-modes.md).

## 3. Embargo leakage

**Threat.** A message that will be retroactively restricted
(declassification-in-flight) gets surfaced by our response before
lore catches up. CVE discussions in particular.

**Mitigations.**
- 72 h quarantine on messages whose `Fixes:` trailer points at a
  commit younger than 7 days, or whose subject matches
  `CVE-YYYY-\d+`. → _pending (Phase 2; policy defined in
  [`../../SECURITY.md`](../../SECURITY.md))._
- Quarantine is response-surface only; underlying store still
  holds the data so we do not need re-ingest to lift it. →
  **design-locked**.
- Propagate lore-upstream redactions on next reindex. → **live
  by construction** (ingest is incremental from the same shards
  lore redacts in).

## 4. Reviewer-interest leak (query logging)

**Threat.** Query logs reveal which CVE / file / maintainer a
researcher is investigating. That metadata is itself sensitive.

**Mitigations.**
- Hosted-instance HTTP access logs keep path + status + timing
  only. No query strings. → _pending (Phase 2; policy in
  [`../../LEGAL.md`](../../LEGAL.md))._
- Structlog processor scrubs query-string / tool-argument fields
  at the record level before serialization. → _pending (Phase 2;
  processor lives in
  [`../../src/kernel_lore_mcp/logging_.py`](../../src/kernel_lore_mcp/logging_.py))._
- Prometheus metrics aggregate-only. → _pending (Phase 2)._
- Local self-hosters are unaffected; they bind `127.0.0.1` and
  own their logs.

## 5. Redaction non-propagation

**Threat.** A message lore has redacted remains visible in the
hosted instance, or in a self-hoster's snapshot, long after the
takedown.

**Mitigations.**
- Reindex incrementally on the grokmirror cadence; lore-side
  deletions replicate. → **live** (ingest is commit-incremental
  via `gix::rev_walk`; see
  [`../../src/ingest.rs`](../../src/ingest.rs)).
- Direct-request redaction SLA: 72 h on hosted instance. →
  _pending_. See [`../../LEGAL.md`](../../LEGAL.md).
- No mechanism to push redactions into third-party self-hosted
  deployments. Documented limitation.

## 6. LLM citation laundering

**Threat.** An LLM caller fabricates a `cite_key` or
`message_id` in its output; downstream reader trusts it because
our tools return cite-shaped data.

**Mitigations.**
- `cite_key` in every response must **round-trip through a
  stored message_id**. The router verifies the `message_id` is
  present in the metadata tier before emitting a citation.
  Never echo a user-supplied cite back as "found." → _pending
  (Phase 2 — tool layer)._
- Every hit carries `tier_provenance[]` so consumers see which
  tier surfaced it (metadata / trigram / bm25). Missing
  provenance is an `isError`. → _pending (Phase 2)._

## 7. Prompt injection via tool arguments

**Threat.** A message body retrieved by the server contains
adversarial instructions that the LLM caller then executes.

**Mitigations.**
- We return raw content; we do not strip or rewrite. The LLM
  caller is responsible for its own prompt hygiene. → **live by
  policy**; we are a retrieval surface, not a sanitizer.
- Responses are strictly-typed pydantic models with content
  fields clearly labeled (`snippet.text`, `body`). The tool
  client can structurally distinguish content from metadata. →
  _pending (Phase 2 — requires
  [`../../src/kernel_lore_mcp/models.py`](../../src/kernel_lore_mcp/models.py)
  wired into live tools)._
- Documented in `docs/mcp/` that tool output contains untrusted
  content. → _pending_.

## 8. Input validation at the MCP boundary

**Threat.** A pathological query crashes the router, OOMs the
process, or triggers regex catastrophic backtracking.

**Mitigations.**
- Regex queries compile to DFA via `regex-automata`. No
  backrefs. → _pending (Phase 3)._
- `query` bounded to `min_length=1, max_length=512`. →
  _pending (Phase 2)._
- HMAC-signed pagination cursor; tampered cursors rejected with
  `INVALID_PARAMS`. → _pending (Phase 2)._
- Per-tool response byte ceilings (e.g. `lore_thread` ≤ 5 MB,
  then paginate). → _pending (Phase 4)._

## Cross-references

- [`../../CLAUDE.md`](../../CLAUDE.md)
- [`../../SECURITY.md`](../../SECURITY.md)
- [`../../LEGAL.md`](../../LEGAL.md)
- [`../architecture/deployment-modes.md`](../architecture/deployment-modes.md)
- [`../architecture/reciprocity.md`](../architecture/reciprocity.md)
- [`../research/training-retriever.md`](../research/training-retriever.md)
