# Architecture — trade-offs

Every decision in this project trades something. This doc records
the trades explicitly so we can revisit them honestly.

## Custom three-tier vs one tantivy index

- **Chose:** three tiers.
- **Cost:** ~2–3 weeks more engineering; three index formats to
  maintain; three merge stories.
- **Gain:** ~50% smaller total index; purpose-fit query
  performance; future-proof for tier-level replacement (swap
  trigram for a faster code-substring engine without touching BM25).
- **Revisit if:** operational burden dominates. A single-tier
  tantivy baseline would still be *useful*; trading three-tier for
  one-tier is a valid simplification if the team shrinks.

## No stemming, no stopwords

- **Chose:** neither.
- **Cost:** "link" vs "linked" don't collapse; BM25 has more terms.
- **Gain:** kernel identifiers and technical terms don't get
  mangled; results are predictable.
- **Revisit if:** users complain about English-language prose
  recall on discussion threads (not patches).

## abi3-py312 vs per-version wheels

- **Chose:** abi3-py312.
- **Cost:** ~5–15% lost on hot paths vs specialized wheels; abi3
  NOT yet compatible with free-threaded 3.14t (PEP 803 pending).
- **Gain:** one wheel covers 3.12/3.13/3.14; deployment trivial.
- **Revisit if:** PEP 803 lands AND free-threaded 3.14t becomes
  the default interpreter on target distros.

## Drop positional postings in BM25

- **Chose:** `WithFreqs` only (no positions).
- **Cost:** no native phrase queries; `"exact quoted string"` on
  prose must fall back to trigram or a later narrow positional
  field.
- **Gain:** 30–50% smaller BM25 tier.
- **Revisit if:** phrase-over-prose becomes a hot query class.
  Mitigation is adding a narrow positional subfield, not flipping
  the whole index.

## gix over git2-rs

- **Chose:** gix.
- **Cost:** smaller community, pre-1.0 API churn.
- **Gain:** `ThreadSafeRepository` lets us fan out rayon without
  per-thread repo opens; faster linear-history walks; mmap pack
  cache tunable.
- **Revisit if:** gix breaks or stalls. git2-rs remains a viable
  fallback.

## Streamable HTTP over SSE

- **Chose:** Streamable HTTP.
- **Cost:** none; SSE is deprecated as of April 1 2026.
- **Gain:** compatible with current Claude Code, Cursor, Codex.
- **Revisit:** never.

## Single-box EC2 vs multi-instance + object storage

- **Chose:** single `r7i.xlarge` / `c7g.xlarge` + gp3.
- **Cost:** no HA; disk is the scale ceiling.
- **Gain:** simple; fits the corpus with headroom; costs
  ~$100/mo steady.
- **Revisit if:** traffic grows past a couple hundred QPS or we
  want multi-region. CloudFront caching in front is the next step,
  not multi-instance.
