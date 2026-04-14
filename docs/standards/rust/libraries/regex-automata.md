# regex-automata 0.4

Rust-specific (no Python parallel).

Pinned: `regex-automata = "0.4"`. The DFA-only subset of
regex's feature surface. This is what we use for every regex in
the system that touches user input.

Rationale: our MCP server exposes `/regex/` predicates to LLM
callers. Arbitrary regex from an LLM is an untrusted input;
backreferences and ambiguity can produce catastrophic backtracking.
`regex` (the full crate) guards against *some* of this but
still supports features we don't want. `regex-automata` with
DFA backends is explicit about its constraints — if a pattern
compiles to a DFA, it runs in linear time; if it doesn't, we
reject at parse time with an actionable error.

See `router.rs` for the policy: regex that doesn't compile
returns `Error::RegexComplexity`; no silent degrade.

---

## Two DFA flavors; we use `dense` by default

| Engine | Memory | Build time | Search speed |
|---|---|---|---|
| `dfa::dense::DFA` | High (O(|Σ| × |states|)) | Slower build | Fastest search |
| `dfa::sparse::DFA` | Low | Faster build | 2-3× slower search |
| `hybrid::dfa::DFA` | Bounded cache | Fastest build | Fast once warm |

Our patterns are small (< 200 chars), and we compile once per
query. `dense` is the right trade-off:

```rust
use regex_automata::dfa::{dense, Automaton};
use regex_automata::util::syntax;

fn compile_dfa(pattern: &str) -> crate::Result<dense::DFA<Vec<u32>>> {
    let syntax = syntax::Config::new()
        .utf8(true)                 // we emit valid UTF-8 text
        .allow_invalid_utf8(false);

    let config = dense::Config::new()
        // Anchor unknown; we control anchoring in the grammar.
        // Reject any pattern that lacks required anchors at callsites
        // that need them.
        .minimize(false)            // minimized DFAs cost too much to build
        ;

    let dfa = dense::Builder::new()
        .syntax(syntax)
        .configure(config)
        .build(pattern)
        .map_err(|e| crate::Error::RegexComplexity(format!(
            "pattern {pattern:?} did not compile to a DFA: {e}. \
             DFA-only engine rejects backreferences and lookaround. \
             Rewrite without them, or use substring predicates \
             (dfa:, dfb:, dfctx:) instead of /regex/."
        )))?;

    Ok(dfa)
}
```

---

## Why not `regex::Regex`

`regex::Regex` is great for known-safe patterns. For untrusted
input, it still:

- Falls back to a bounded-backtracking NFA for patterns the
  DFA can't handle, without explicitly refusing.
- Permits unicode features (`\w`, `\s`, `\d`) that balloon
  DFA state counts.
- Hides the build-time cost of Unicode classes — a tiny-looking
  pattern can compile to a huge DFA.

`regex-automata` lets us be explicit:

- `syntax::Config::unicode(false)` for byte-class patterns (our
  trigram tier operates on bytes).
- `dense::Config::dfa_size_limit(...)` caps memory at build
  time; exceeding it → error → we return `RegexComplexity`.

---

## Rejecting non-DFA patterns

The policy: **if it doesn't compile to a DFA, we reject.**
This is enforced at two layers:

1. **Syntax-level rejection** — backreferences (`\1`),
   lookaround (`(?=...)`, `(?<=...)`), atomic groups, possessive
   quantifiers. `regex-syntax`'s `Parser` errors on these.
2. **Build-level rejection** — patterns whose NFA is too large
   for the configured DFA size limit. Set
   `dense::Config::dfa_size_limit(Some(16 * 1024 * 1024))` and
   `determinize_size_limit` for an 16 MB cap on per-pattern DFA
   size.

Both surface the same `Error::RegexComplexity` to the user,
with a message that names the feature:

```rust
// Example error message:
// "pattern r"\1foo" did not compile to a DFA: backreferences
//  unsupported. Rewrite without backrefs, or use substring
//  predicates (dfa:, dfb:, dfctx:) instead of /regex/."
```

See `../design/errors.md` for the three-part message rule.

---

## Bounded-memory search

DFA search is single-pass, linear-time, bounded-memory *at
search time*. The build-time memory is the cap we set above.

At search time:

```rust
use regex_automata::dfa::Automaton;

fn confirm_match(dfa: &dense::DFA<Vec<u32>>, haystack: &[u8]) -> bool {
    dfa.try_search_fwd(&regex_automata::Input::new(haystack))
        .ok()
        .flatten()
        .is_some()
}
```

`try_search_fwd` returns `Result<Option<HalfMatch>, MatchError>`.
We ignore match position; we only need presence/absence for
the trigram-tier confirm step.

---

## `hybrid::regex::Regex` vs `dfa::Regex`

`hybrid` is lazy-DFA: builds on demand, caches states. Good
when patterns are cheap but searches are many. Bad when we need
a bounded worst-case: lazy-DFA cache thrash degrades to NFA
speeds.

`dfa` (what we use) is eager-DFA: pay the build cost once,
get predictable linear-time search.

Our workload:

- Build once per query (not cached).
- Search per candidate message (up to `TRIGRAM_CONFIRM_LIMIT`
  = 4096).

Eager `dense::DFA` wins: 4096 searches amortizes the build
cost; lazy cache gets no re-hit benefit within one query.

If we ever cache compiled regexes across queries, reconsider
`hybrid`. Today we don't.

---

## Safety for untrusted input — the contract

User-provided regex (from MCP `/pattern/` predicates) is
evaluated only after:

1. **Parse succeeds with the non-Unicode, non-lookaround,
   non-backreference syntax config.**
2. **Build succeeds within the `dfa_size_limit` cap.**
3. **`router.rs` has verified the call is narrowed by `list:`
   or `rt:`** — unanchored regex on the full term-dict is
   blocked at the routing layer.

All three must hold. Any failure → `Error::RegexComplexity`.

Catastrophic-pattern defense is structural, not heuristic: the
DFA's linear-time guarantee eliminates the backtracking class
of DoS. The size limit caps the DFA-state-explosion class.
Unicode-off eliminates the \w-explosion class.

---

## FST bridge — `fst::Automaton` adapter

The trigram tier uses the DFA to walk the FST term dict
(see `roaring-fst.md`). `fst` has its own `Automaton` trait;
`regex-automata::DFA` doesn't implement it directly but the
adapter is ~30 LOC:

```rust
// Adapter sketch — full version in src/trigram.rs (TODO).
struct DfaAsFstAutomaton<'a>(&'a dense::DFA<Vec<u32>>);

impl<'a> fst::Automaton for DfaAsFstAutomaton<'a> {
    type State = regex_automata::util::primitives::StateID;

    fn start(&self) -> Self::State {
        self.0.start_state_forward(&Input::new(&[] as &[u8])).unwrap()
    }

    fn is_match(&self, state: &Self::State) -> bool {
        self.0.is_match_state(*state)
    }

    fn can_match(&self, state: &Self::State) -> bool {
        !self.0.is_dead_state(*state)
    }

    fn accept(&self, state: &Self::State, byte: u8) -> Self::State {
        self.0.next_state(*state, byte)
    }
}
```

This bridges "which trigram *keys* does this regex match" —
the FST streams only matching keys, skipping the rest. The
postings at those keys union into the candidate docid set.

---

## Build-time knobs we pin

```rust
dense::Config::new()
    .minimize(false)              // minimize is O(states²); usually not worth it.
    .dfa_size_limit(Some(16 * 1024 * 1024))
    .determinize_size_limit(Some(16 * 1024 * 1024))
    .byte_classes(true)           // groups equivalent bytes; shrinks state count.
```

`byte_classes(true)` is the default but we pin it to make the
choice explicit. For patterns operating on ASCII code, it
shrinks DFAs ~4× with no semantic change.

---

## 0.4 changes to be aware of

- 0.4 merged `regex-syntax` 0.8 as the parser. Syntax is stable;
  error messages are more detailed than 0.3.
- `dense::DFA` generic parameter changed to
  `dense::DFA<A: StateArray>`; most code uses the default
  `Vec<u32>` state-id size.
- `hybrid::dfa::DFA` API added `Cache::new` for explicit cache
  lifetimes. We don't use hybrid.
- `Automaton::try_search_fwd` (vs `search_fwd`) is the
  non-panicking variant — always prefer.

---

## Don't-do list

| Anti-pattern | Why |
|---|---|
| Using `regex::Regex` for user-provided patterns | Permits Unicode / bounded-backtrack; not a hard DFA guarantee. |
| Skipping `dfa_size_limit` | Some patterns DFA-compile to hundreds of MB. |
| Caching compiled DFAs across untrusted queries without capping cache size | Cache-poisoning DoS. |
| `hybrid` for our hot loop | Cache thrash can degrade to NFA speeds. |
| Returning `regex::Error` directly to the user | Loses the three-part message advice. |

---

## Checklist for a regex change

1. All user-provided regex paths compile via
   `compile_dfa(pattern)`.
2. Errors surface as `Error::RegexComplexity` with the
   three-part message.
3. `dfa_size_limit` and `determinize_size_limit` set.
4. `syntax::Config::unicode(false)` for byte-level patterns.
5. Unanchored regex rejected unless narrowed by `list:` or
   `rt:` at router layer.
6. FST bridge adapter covered by a test over a known FST.

See also:
- `roaring-fst.md` — FST consumer of the DFA.
- `../design/errors.md` — `RegexComplexity` message rule.
- `../../../indexing/trigram-tier.md` — regex-in-query policy.
