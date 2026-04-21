//! Query router: parse a lei-compatible query, dispatch to tiers,
//! merge by message_id.
//!
//! Grammar in v1 (expanded per kernel-user review). See
//! docs/mcp/query-routing.md for the authoritative list.
//!
//! ```text
//! Metadata-only predicates:
//!     s:<term>  f:<term>  t:<term>  c:<term>  tc:<term>
//!     list:<name>  mid:<id>  rt:<since>..<until>  d:<rel>
//!     dfn:<file>  dfhh:<func>
//!     reviewed-by:<term>  acked-by:<term>  tested-by:<term>
//!     signed-off-by:<term>  co-developed-by:<term>
//!     fixes:<sha>  closes:<url>  link:<url>
//!     patchid:<sha>  applied:<bool>  cherry:<sha>
//!     tag:<RFC|RFT|GIT_PULL|ANNOUNCE|RESEND|PATCH>
//!     trailer:<name>:<value>
//!
//! Trigram predicates (patch content):
//!     dfpre:<term>   diff minus-side
//!     dfpost:<term>  diff plus-side
//!     dfa:<term>     either side
//!     dfb:<term>     hunk body incl. context
//!     dfctx:<term>   context lines only
//!     /<regex>/      arbitrary regex (DFA-only; rejected otherwise)
//!
//! BM25 predicates (prose body):
//!     b:<term>       body term
//!     nq:<term>      body minus quoted-reply lines (v2; see TODO)
//!     "phrase"       REJECTED on body_prose in v1 (no positions);
//!                    router returns actionable error, never
//!                    silent degradation to conjunction.
//!
//! Boolean: AND, OR, NOT, +, -.
//! ```
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
//!   - Per-query wall-clock hard cap (default 5s; override via
//!     `KLMCP_QUERY_WALL_CLOCK_MS`).
//!   - Unanchored regex on full term-dict is blocked unless `list:`
//!     or `rt:` narrows candidate set first.
//!
//! Cursor format: HMAC-signed base64 of
//! `(last_seen_score: f32 | null, last_seen_date: i64 ns,
//! last_seen_mid: String, query_hash: u64)`.
//! HMAC key from env `KLMCP_CURSOR_KEY` (generated at first run).
//! Cursor with mismatched HMAC returns `Error::InvalidCursor`.
//!
//! v0.5 implementation: a small grammar (lei-compatible subset) +
//! per-tier dispatch + reciprocal rank fusion. HMAC-signed cursors
//! provide opaque pagination resilient to tampering.

#![allow(dead_code)]

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::{Error, Result};
use crate::reader::{MessageRow, Reader};

type HmacSha256 = Hmac<Sha256>;

/// Reciprocal Rank Fusion smoothing constant. 60 is the standard.
const RRF_K: f32 = 60.0;

/// Default per-query wall-clock cap. Routers reject queries that take
/// longer. Production can override via `KLMCP_QUERY_WALL_CLOCK_MS`.
const DEFAULT_QUERY_WALL_CLOCK_MS: u64 = 5_000;

/// Parsed query AST. Each field is optional; the router fills in
/// what the grammar produced.
#[derive(Debug, Default, Clone)]
pub struct ParsedQuery {
    /// Free-text terms (BM25 tier).
    pub free_text: Vec<String>,
    /// Mailing list filter.
    pub list: Option<String>,
    /// `dfn:<file>` exact filename match (metadata tier).
    pub touched_file: Option<String>,
    /// `dfhh:<func>` exact function match (metadata tier).
    pub touched_function: Option<String>,
    /// `dfb:<term>` literal substring in patch content (trigram tier).
    pub patch_substring: Option<String>,
    /// `mid:<id>` exact message-id match (metadata tier).
    pub message_id: Option<String>,
    /// `f:<term>` from address substring.
    pub from_addr: Option<String>,
    /// `fixes:<sha>` reverse fixes lookup.
    pub fixes_sha: Option<String>,
    /// `since:<unix-ns>` lower bound on date.
    pub since_unix_ns: Option<i64>,
}

/// Parse a tiny grammar: space-separated tokens of the form
/// `key:value` (with `value` quoted-or-bare) plus bare free-text.
/// This is intentionally simpler than lei's full grammar — phase
/// 5c+ will expand. Quoted strings on body_prose are rejected (the
/// BM25 layer does the actual rejection; we just propagate them).
pub fn parse_query(q: &str) -> Result<ParsedQuery> {
    let mut out = ParsedQuery::default();
    for (tok, was_quoted) in tokenize_tagged(q) {
        if let Some((key, value)) = tok.split_once(':') {
            match key {
                "list" => out.list = Some(value.to_owned()),
                "dfn" => out.touched_file = Some(value.to_owned()),
                "dfhh" => out.touched_function = Some(value.to_owned()),
                "dfb" => out.patch_substring = Some(value.to_owned()),
                "mid" => out.message_id = Some(value.to_owned()),
                "f" => out.from_addr = Some(value.to_owned()),
                "fixes" => out.fixes_sha = Some(value.to_owned()),
                "since" => {
                    out.since_unix_ns = Some(value.parse::<i64>().map_err(|e| {
                        Error::QueryParse(format!("since: not an integer ns: {e}"))
                    })?);
                }
                "b" => {
                    if was_quoted {
                        return Err(Error::QueryParse(
                            "phrase queries on body_prose are not supported in v0.5: this \
                             field is indexed WithFreqs (no positions). Use \
                             dfb:\"<phrase>\" for literal substrings in patch content, or \
                             split the phrase into AND-ed bare terms."
                                .to_owned(),
                        ));
                    }
                    out.free_text.push(value.to_owned());
                }
                _ => {
                    return Err(Error::QueryParse(format!(
                        "unknown predicate {key:?}; supported: list dfn dfhh dfb mid f fixes since b"
                    )));
                }
            }
        } else if was_quoted {
            return Err(Error::QueryParse(
                "phrase queries on body_prose are not supported in v0.5: this \
                 field is indexed WithFreqs (no positions). Use dfb:\"<phrase>\" \
                 for literal substrings in patch content, or split the phrase \
                 into AND-ed bare terms."
                    .to_owned(),
            ));
        } else {
            out.free_text.push(tok);
        }
    }
    Ok(out)
}

/// Tokenize and tag each token with whether it came from inside
/// quotes. The router uses the tag to reject phrase queries that
/// would otherwise silently fall through to BM25 (whose tier is
/// position-less by design).
fn tokenize_tagged(q: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_quote = false;
    let mut current_was_quoted = false;
    for c in q.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                if in_quote {
                    current_was_quoted = true;
                }
            }
            ' ' | '\t' | '\n' if !in_quote => {
                if !buf.is_empty() {
                    out.push((std::mem::take(&mut buf), current_was_quoted));
                    current_was_quoted = false;
                }
            }
            _ => buf.push(c),
        }
    }
    if !buf.is_empty() {
        out.push((buf, current_was_quoted));
    }
    out
}

/// One scored hit returned from the router after fusion.
#[derive(Debug, Clone)]
pub struct RankedHit {
    pub row: MessageRow,
    pub fused_score: f32,
    pub tier_provenance: Vec<String>,
    pub is_exact_match: bool,
}

/// Run a parsed query against all available tiers and fuse via RRF.
///
/// Strategy:
///   1. metadata tier — runs structured predicates (mid, f, dfn,
///      dfhh, fixes, list, since) returning newest-first rows.
///   2. trigram tier — runs `dfb:` patch substring search.
///   3. BM25 tier — joins free_text + non-empty subject into a
///      space-separated query string.
///
/// Each tier returns a Vec<MessageRow>. We assign ranks per tier
/// (1-based), compute RRF score = sum(1 / (RRF_K + rank)), and
/// merge by message_id. Caller specifies `limit`.
pub fn dispatch(
    reader: &Reader,
    parsed: &ParsedQuery,
    limit: usize,
) -> Result<(Vec<RankedHit>, Vec<String>)> {
    let mut tier_results: HashMap<&'static str, Vec<MessageRow>> = HashMap::new();
    let mut default_applied: Vec<String> = Vec::new();

    // Metadata tier — fire if any structured predicate is present
    // OR if there's no free_text and no patch_substring (we still
    // want SOME results).
    let metadata_relevant = parsed.message_id.is_some()
        || parsed.touched_file.is_some()
        || parsed.touched_function.is_some()
        || parsed.from_addr.is_some()
        || parsed.fixes_sha.is_some()
        || (parsed.free_text.is_empty() && parsed.patch_substring.is_none());

    // Fan out the three tiers in parallel. `std::thread::scope` lets
    // each closure borrow `reader` + `parsed` without requiring
    // 'static. The Reader internals (over.db pool, Store cache, BM25
    // reader) are all Sync, so parallel reads are safe.
    //
    // Mixed-tier queries (e.g. `list:lkml security`) pay
    // max(metadata, bm25) + merge instead of the former sum; pure
    // single-tier queries cost ~one thread spawn (~microseconds on
    // Linux) and otherwise behave identically.
    type TierOut = Result<(Option<Vec<MessageRow>>, Option<bool>)>;
    let run_metadata = || -> TierOut {
        if !metadata_relevant {
            return Ok((None, None));
        }
        let rows = if let Some(mid) = &parsed.message_id {
            reader
                .fetch_message(mid)?
                .map_or_else(Vec::new, |r| vec![r])
        } else if let Some(sha) = &parsed.fixes_sha {
            reader.expand_citation(sha, limit)?
        } else if parsed.touched_file.is_some() || parsed.touched_function.is_some() {
            reader.activity(
                parsed.touched_file.as_deref(),
                parsed.touched_function.as_deref(),
                parsed.since_unix_ns,
                parsed.list.as_deref(),
                limit,
            )?
        } else if let Some(from) = &parsed.from_addr {
            // f:<addr> — route through eq() which has early termination
            // on `limit` matches. all_rows() would materialize every row
            // in the corpus before post-filtering (29M rows = OOM on
            // realistic corpora).
            reader.eq(
                crate::reader::EqField::FromAddr,
                from,
                parsed.since_unix_ns,
                parsed.list.as_deref(),
                limit,
            )?
        } else if parsed.list.is_some() || parsed.since_unix_ns.is_some() {
            // Pure list:/since: predicate. Cap is tight (RRF merge
            // below only uses the first `limit * 2` anyway); loading
            // a million rows per query piles up memory under load.
            let cap = limit.saturating_mul(20).max(10_000);
            reader.all_rows(parsed.list.as_deref(), parsed.since_unix_ns, Some(cap))?
        } else {
            Vec::new()
        };
        Ok((Some(rows), None))
    };

    let run_trigram = || -> TierOut {
        match &parsed.patch_substring {
            Some(needle) => {
                let rows = reader.patch_search(needle, parsed.list.as_deref(), limit)?;
                Ok((Some(rows), None))
            }
            None => Ok((None, None)),
        }
    };

    let run_bm25 = || -> TierOut {
        if parsed.free_text.is_empty() {
            return Ok((None, None));
        }
        let (q, hyphen_split) = rewrite_free_text_for_bm25(&parsed.free_text);
        let scored = reader.prose_search_filtered(&q, parsed.list.as_deref(), limit * 2)?;
        let rows: Vec<MessageRow> = scored.into_iter().map(|(r, _)| r).collect();
        Ok((Some(rows), Some(hyphen_split)))
    };

    // rayon::scope uses the already-warm global worker pool — one
    // well-known trap per the research brief is oversubscribing
    // tantivy's internal pool; tantivy uses its OWN `Executor` (not
    // rayon's global), so dispatching 3 tier tasks on the global
    // pool doesn't contend with it. Cost per dispatch: ~microseconds
    // vs ~hundreds of microseconds to spawn 3 fresh OS threads.
    //
    // Deadline propagation: rayon workers don't inherit the calling
    // thread's TLS, so a DeadlineGuard set upstream in PyO3 doesn't
    // reach the spawned tiers. Snapshot the deadline (Deadline is
    // Copy) before spawn and re-install it inside each worker via
    // DeadlineGuard::install so scan() checks within the tier honor
    // the same budget.
    let deadline = crate::timeout::current_deadline();
    let mut meta_out: TierOut = Ok((None, None));
    let mut trigram_out: TierOut = Ok((None, None));
    let mut bm25_out: TierOut = Ok((None, None));
    rayon::scope(|s| {
        s.spawn(|_| {
            let _g = deadline.map(crate::timeout::DeadlineGuard::install);
            meta_out = run_metadata();
        });
        s.spawn(|_| {
            let _g = deadline.map(crate::timeout::DeadlineGuard::install);
            trigram_out = run_trigram();
        });
        s.spawn(|_| {
            let _g = deadline.map(crate::timeout::DeadlineGuard::install);
            bm25_out = run_bm25();
        });
    });

    if let (Some(rows), _) = meta_out? {
        tier_results.insert("metadata", rows);
    }
    if let (Some(rows), _) = trigram_out? {
        tier_results.insert("trigram", rows);
    }
    if let (Some(rows), hyphen) = bm25_out? {
        tier_results.insert("bm25", rows);
        if hyphen == Some(true) {
            default_applied.push("hyphen-split".to_owned());
        }
    }

    let mut merged = rrf_merge(tier_results, limit * 2);

    // Post-filter: apply list/from/since predicates that not all
    // tiers honored natively. This ensures a query like
    // "list:linux-cifs ksmbd" never returns results from other lists,
    // even though BM25 searched unfiltered.
    if let Some(list) = &parsed.list {
        merged.retain(|h| h.row.list == *list);
    }
    if let Some(from) = &parsed.from_addr {
        let lc = from.to_lowercase();
        merged.retain(|h| {
            h.row
                .from_addr
                .as_ref()
                .is_some_and(|a| a.to_lowercase().contains(&lc))
        });
    }
    if let Some(since) = parsed.since_unix_ns {
        merged.retain(|h| h.row.date_unix_ns.is_some_and(|d| d >= since));
    }
    merged.truncate(limit);

    Ok((merged, default_applied))
}

/// Rewrite a free-text BM25 query so hyphenated terms don't trip
/// tantivy's phrase-query path.
///
/// Tantivy's `QueryParser` interprets `use-after-free` as a phrase
/// of three tokens. Our `body_prose` field is indexed WithFreqs
/// (positions OFF by design — see CLAUDE.md), so any phrase query
/// gets rejected. The `KernelIdentSplitter` tokenizer already splits
/// on hyphens at index time, so rewriting the query `use-after-free`
/// → `use after free` produces the same token set via an implicit
/// AND instead. Returns `(rewritten, changed)` so the router can
/// surface `hyphen-split` in `default_applied`.
fn rewrite_free_text_for_bm25(free_text: &[String]) -> (String, bool) {
    let raw = free_text.join(" ");
    let rewritten = raw.replace('-', " ");
    let changed = rewritten != raw;
    (rewritten, changed)
}

fn rrf_merge(tiers: HashMap<&'static str, Vec<MessageRow>>, limit: usize) -> Vec<RankedHit> {
    let mut acc: HashMap<String, (MessageRow, f32, Vec<String>)> = HashMap::new();
    for (tier, rows) in tiers {
        for (rank, row) in rows.into_iter().enumerate() {
            let mid = row.message_id.clone();
            let contrib = 1.0 / (RRF_K + (rank as f32 + 1.0));
            acc.entry(mid)
                .and_modify(|(_, score, prov)| {
                    *score += contrib;
                    if !prov.iter().any(|p| p == tier) {
                        prov.push(tier.to_owned());
                    }
                })
                .or_insert_with(|| (row, contrib, vec![tier.to_owned()]));
        }
    }
    let mut hits: Vec<RankedHit> = acc
        .into_values()
        .map(|(row, score, prov)| {
            let exact = prov.iter().any(|p| p == "metadata" || p == "trigram");
            RankedHit {
                row,
                fused_score: score,
                tier_provenance: prov,
                is_exact_match: exact,
            }
        })
        .collect();
    hits.sort_by(|a, b| {
        b.fused_score
            .partial_cmp(&a.fused_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(limit);
    hits
}

// --------------------------------------------------------------------
// Cursor: HMAC-signed opaque pagination token.

/// Pagination cursor payload.
///
/// `last_seen_score` is overloaded to carry either a relevance
/// score (`lore_search` RRF) or a `date_unix_ns` tiebreak (the
/// newest-first tools like `lore_patch_search`, `lore_activity`,
/// `lore_regex`, `lore_author_footprint`). `f64` precision keeps
/// nanosecond dates exact; for relevance scores in `[0, 1]` it's
/// overkill but harmless.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CursorPayload {
    pub query_hash: u64,
    pub last_seen_score: f64,
    pub last_seen_mid: String,
}

pub fn sign_cursor(secret: &[u8], payload: &CursorPayload) -> Result<String> {
    let body =
        serde_json::to_vec(payload).map_err(|e| Error::State(format!("cursor serialize: {e}")))?;
    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|e| Error::State(format!("hmac init: {e}")))?;
    mac.update(&body);
    let sig = mac.finalize().into_bytes();
    let token = [body.as_slice(), &sig].concat();
    Ok(URL_SAFE_NO_PAD.encode(token))
}

pub fn verify_cursor(secret: &[u8], token: &str) -> Result<CursorPayload> {
    let raw = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|e| Error::InvalidCursor(format!("base64: {e}")))?;
    if raw.len() < 32 {
        return Err(Error::InvalidCursor("token too short".to_owned()));
    }
    let (body, sig) = raw.split_at(raw.len() - 32);
    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|e| Error::State(format!("hmac init: {e}")))?;
    mac.update(body);
    mac.verify_slice(sig)
        .map_err(|_| Error::InvalidCursor("hmac mismatch".to_owned()))?;
    let payload: CursorPayload =
        serde_json::from_slice(body).map_err(|e| Error::InvalidCursor(format!("json: {e}")))?;
    Ok(payload)
}

// QUERY_WALL_CLOCK_MS is consumed by the Python tool layer where
// asyncio.wait_for can interrupt the call without unsafe panics in
// the Rust thread. Re-exported so the tool layer stays in sync.
#[allow(dead_code)]
pub fn query_wall_clock_ms() -> u64 {
    std::env::var("KLMCP_QUERY_WALL_CLOCK_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_QUERY_WALL_CLOCK_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_free_text() {
        let q = parse_query("ksmbd dacl bounds").unwrap();
        assert_eq!(q.free_text, vec!["ksmbd", "dacl", "bounds"]);
        assert!(q.list.is_none());
    }

    #[test]
    fn parse_mixed_predicates_and_free_text() {
        let q = parse_query("dfn:fs/x.c list:linux-cifs ksmbd").unwrap();
        assert_eq!(q.touched_file.as_deref(), Some("fs/x.c"));
        assert_eq!(q.list.as_deref(), Some("linux-cifs"));
        assert_eq!(q.free_text, vec!["ksmbd"]);
    }

    #[test]
    fn parse_quoted_value() {
        let q = parse_query(r#"dfb:"some literal" list:linux-cifs"#).unwrap();
        assert_eq!(q.patch_substring.as_deref(), Some("some literal"));
        assert_eq!(q.list.as_deref(), Some("linux-cifs"));
    }

    #[test]
    fn parse_unknown_predicate_rejected() {
        let err = parse_query("nope:foo bar").unwrap_err();
        match err {
            Error::QueryParse(_) => {}
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn rrf_fuses_two_tiers() {
        // Two identical rows: one shows up rank-1 in metadata, rank-3
        // in trigram. Score should be 1/(60+1) + 1/(60+3).
        let row = MessageRow {
            message_id: "m1@x".to_owned(),
            list: "l".to_owned(),
            ..Default::default()
        };
        let mut tiers = HashMap::new();
        tiers.insert("metadata", vec![row.clone()]);
        tiers.insert(
            "trigram",
            vec![
                MessageRow {
                    message_id: "x@x".to_owned(),
                    ..Default::default()
                },
                MessageRow {
                    message_id: "y@x".to_owned(),
                    ..Default::default()
                },
                row.clone(),
            ],
        );
        let hits = rrf_merge(tiers, 10);
        let m1 = hits.iter().find(|h| h.row.message_id == "m1@x").unwrap();
        let expected = 1.0_f32 / 61.0 + 1.0 / 63.0;
        assert!((m1.fused_score - expected).abs() < 1e-6);
        assert_eq!(m1.tier_provenance.len(), 2);
        assert!(m1.is_exact_match);
    }

    #[test]
    fn rewrite_free_text_splits_hyphens() {
        let free_text = vec!["use-after-free".to_owned(), "cifs".to_owned()];
        let (rewritten, changed) = rewrite_free_text_for_bm25(&free_text);
        assert_eq!(rewritten, "use after free cifs");
        assert!(changed);
    }

    #[test]
    fn rewrite_free_text_idempotent_when_no_hyphens() {
        let free_text = vec!["ksmbd".to_owned(), "dacl".to_owned()];
        let (rewritten, changed) = rewrite_free_text_for_bm25(&free_text);
        assert_eq!(rewritten, "ksmbd dacl");
        assert!(!changed);
    }

    #[test]
    fn cursor_signs_and_verifies() {
        let secret = b"my-test-secret-32-bytes-or-more!";
        let payload = CursorPayload {
            query_hash: 12345,
            last_seen_score: 0.0123,
            last_seen_mid: "abc@x".to_owned(),
        };
        let token = sign_cursor(secret, &payload).unwrap();
        let got = verify_cursor(secret, &token).unwrap();
        assert_eq!(got.query_hash, 12345);
        assert_eq!(got.last_seen_mid, "abc@x");
    }

    #[test]
    fn cursor_rejects_tampered_token() {
        let secret = b"secret";
        let payload = CursorPayload {
            query_hash: 1,
            last_seen_score: 0.0,
            last_seen_mid: "a".to_owned(),
        };
        let mut token = sign_cursor(secret, &payload).unwrap();
        // Flip a base64 character to invalidate.
        token.replace_range(0..1, "z");
        assert!(verify_cursor(secret, &token).is_err());
    }

    #[test]
    fn cursor_rejects_wrong_secret() {
        let payload = CursorPayload {
            query_hash: 1,
            last_seen_score: 0.0,
            last_seen_mid: "a".to_owned(),
        };
        let token = sign_cursor(b"secret-1", &payload).unwrap();
        assert!(verify_cursor(b"secret-2", &token).is_err());
    }
}
