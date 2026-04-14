//! Query router: parse a lei-compatible query, dispatch to tiers,
//! merge by message_id.
//!
//! Grammar in v1 (expanded per kernel-user review). See
//! docs/mcp/query-routing.md for the authoritative list.
//!
//!     Metadata-only predicates:
//!         s:<term>  f:<term>  t:<term>  c:<term>  tc:<term>
//!         list:<name>  mid:<id>  rt:<since>..<until>  d:<rel>
//!         dfn:<file>  dfhh:<func>
//!         reviewed-by:<term>  acked-by:<term>  tested-by:<term>
//!         signed-off-by:<term>  co-developed-by:<term>
//!         fixes:<sha>  closes:<url>  link:<url>
//!         patchid:<sha>  applied:<bool>  cherry:<sha>
//!         tag:<RFC|RFT|GIT_PULL|ANNOUNCE|RESEND|PATCH>
//!         trailer:<name>:<value>
//!
//!     Trigram predicates (patch content):
//!         dfpre:<term>   diff minus-side
//!         dfpost:<term>  diff plus-side
//!         dfa:<term>     either side
//!         dfb:<term>     hunk body incl. context
//!         dfctx:<term>   context lines only
//!         /<regex>/      arbitrary regex (DFA-only; rejected otherwise)
//!
//!     BM25 predicates (prose body):
//!         b:<term>       body term
//!         nq:<term>      body minus quoted-reply lines (v2; see TODO)
//!         "phrase"       REJECTED on body_prose in v1 (no positions);
//!                        router returns actionable error, never
//!                        silent degradation to conjunction.
//!
//!     Boolean: AND, OR, NOT, +, -.
//!
//! Dispatch rules (canonical in docs/mcp/query-routing.md):
//!   1. Hoist metadata predicates first; intersect to narrow.
//!   2. Trigram and BM25 run in parallel via rayon on the narrowed
//!      candidate set.
//!   3. Merge by message_id; score precedence = BM25 > recency.
//!   4. Per-hit `tier_provenance` carried through so the LLM knows
//!      why a hit matched (`metadata | trigram | bm25` or combo).
//!
//! Safety limits:
//!   - Regex must compile to DFA via `regex-automata` (no backrefs,
//!     no catastrophic patterns). Rejected with `Error::RegexComplexity`.
//!   - Per-query wall-clock 5s hard cap.
//!   - Unanchored regex on full term-dict is blocked unless `list:`
//!     or `rt:` narrows candidate set first.
//!
//! Cursor format: HMAC-signed base64 of
//!     (last_seen_score: f32 | null, last_seen_date: i64 ns,
//!      last_seen_mid: String, query_hash: u64).
//! HMAC key from env `KLMCP_CURSOR_KEY` (generated at first run).
//! Cursor with mismatched HMAC returns `Error::InvalidCursor`.
//!
//! Implementation lands in a follow-up PR.
