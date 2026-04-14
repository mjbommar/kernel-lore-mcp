//! Trigram tier — Zoekt-style index over patch/diff content.
//!
//! Built on `fst` (term dict) + `roaring` (posting bitmaps). Answers
//! regex / substring / identifier-fragment queries over code. Does
//! NOT use tantivy; scoring is irrelevant for patch content.
//!
//! Query-time confirm discipline:
//!   1. Parse query -> required set of byte trigrams via
//!      `regex-automata` DFA analysis (reject patterns that don't
//!      compile to a DFA).
//!   2. FST lookup -> roaring bitmaps; intersect to get candidate
//!      docids.
//!   3. Cap candidates at `TRIGRAM_CONFIRM_LIMIT` (see below).
//!   4. For each candidate, decompress the patch body from the
//!      compressed store (one zstd frame) and re-run the real DFA
//!      against it. Return only confirmed hits.
//!
//! `TRIGRAM_CONFIRM_LIMIT`: N_CANDIDATES after FST intersection.
//! Tuned so that p95 confirmation stays under 500ms on the reference
//! box. If exceeded, the router returns a partial result with a flag
//! (`truncated_by_candidate_cap: true`) so LLM callers know.
//!
//! See docs/indexing/trigram-tier.md for on-disk layout.
//!
//! Implementation lands in a follow-up PR.
#[allow(dead_code)] // wired in a follow-up PR
pub const TRIGRAM_CONFIRM_LIMIT: usize = 4096;
