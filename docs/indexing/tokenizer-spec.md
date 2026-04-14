# Indexing — tokenizer spec

The single most important doc in this directory. Every choice here
is a deliberate proscription; do not change without documenting in
`docs/research/`.

## Guiding principles

1. **Kernel identifiers are first-class.** `vector_mmsg_rx`,
   `SMB2_CREATE`, `__skb_unlink`, `nfs4_layout_types`. These are
   what people search for. The tokenizer's job is to make them
   findable whole AND by their parts.
2. **Patches and prose are different.** Apply different tokenizers
   (and different tiers entirely — trigram vs BM25).
3. **Atomic tokens for structured identifiers.** Email addresses,
   Message-IDs, commit SHAs, CVE IDs. Never split. Route through a
   dedicated `raw` analyzer or STRING field.
4. **Correctness over cleverness.** No stemming, no stopwords, no
   soundex, no typo tolerance.

## Analyzers

We register three tantivy analyzers plus a raw STRING passthrough
for the BM25 tier. Trigram tier doesn't use tantivy analyzers.

### `kernel_prose` (primary, body + subject)

Chain:

1. **Quoted-reply/signature strip** (pre-tokenize at message
   parse, not in tantivy). See
   [`../ingestion/mbox-parsing.md`](../ingestion/mbox-parsing.md).
2. **IdentifierSplitter** (custom `Tokenizer` impl):
   - Scan input; emit spans of `[A-Za-z0-9_]+` as tokens.
   - Non-matching chars = delimiters (discarded).
   - Emit the whole-span token at position N.
3. **SubtokenFilter** (custom `TokenFilter` impl):
   - For each input token, also emit:
     - snake_case splits on `_` (drop empties)
     - camelCase boundaries (`[a-z0-9]→[A-Z]`, `[A-Z]+→[A-Z][a-z]`)
   - All subtokens carry `position_inc=0` so phrase queries and
     the whole token share position N (if positions are ever
     re-enabled).
4. **LowerCaser**.
5. **No stemmer, no stopwords.**

```rust
use tantivy::tokenizer::*;
let kernel_prose = TextAnalyzer::builder(IdentifierSplitter)
    .filter(SubtokenFilter)
    .filter(LowerCaser)
    .build();
index.tokenizers().register("kernel_prose", kernel_prose);
```

### `raw` (atomic STRING-ish)

For fields where we want the whole string indexed once, lowercased,
no splitting: `message_id`, `list`, `from_addr`. Equivalent to
tantivy's built-in `raw` with a LowerCaser — tantivy's `raw` is
case-sensitive.

```rust
let raw_lc = TextAnalyzer::builder(RawTokenizer).filter(LowerCaser).build();
index.tokenizers().register("raw_lc", raw_lc);
```

### `email` (email addresses specifically)

`<foo@bar.com>` should be findable three ways:
- the whole address (`foo@bar.com`) — primary token, position N
- local-part (`foo`) — position N, `position_inc=0`
- domain (`bar.com`) — position N, `position_inc=0`

Registered but only applied to a separate `from_addr_fulltext`
field we add in v2 if needed. V1: `from_addr` uses `raw_lc`.

## Field → analyzer mapping

| Field | Type | Analyzer | Index option |
|-------|------|----------|--------------|
| `message_id` | STRING | raw (case-preserving) | STORED + indexed |
| `list` | STRING | raw_lc | indexed |
| `from_addr` | STRING | raw_lc | indexed, STORED |
| `from_name` | TEXT | kernel_prose | WithFreqs |
| `subject_raw` | - | - | STORED only |
| `subject_normalized` | TEXT | kernel_prose | WithFreqs |
| `body_prose` | TEXT | kernel_prose | WithFreqs (no positions) |
| `date` | DATE | - | FAST + STORED |

## Positional postings: off

`IndexRecordOption::WithFreqs` for all `kernel_prose` fields.
Rationale in
[`../architecture/three-tier-index.md`](../architecture/three-tier-index.md).

## Tokenizer-manager gotcha

**Tokenizer names are not persisted in `meta.json`.** Every
process that opens an index (reader OR writer) must re-register
the tokenizers by the same name before use. Centralize in one
function:

```rust
pub fn register_kernel_analyzers(index: &Index) {
    let mgr = index.tokenizers();
    mgr.register("kernel_prose", make_kernel_prose());
    mgr.register("raw_lc", make_raw_lc());
    mgr.register("email", make_email());
}
```

Call it after `Index::open` AND after `Index::create_in_dir`.

## Trigram tier tokenization

Not tantivy. Raw bytes of the patch content are sliced into every
overlapping 3-byte window. Storage:
- Term dict: `fst::Map<u64>` from trigram to posting-offset.
- Postings: per-trigram `roaring::RoaringBitmap` of docids.

ASCII-safe (kernel patches are effectively ASCII). For any
non-ASCII bytes: still index byte-trigrams; regex queries that
care about unicode are out of scope for v1.

## Do NOT add

- Stemmer. Ever.
- Stopword filter.
- Asciifolding. Kernel people write `µ` and `é` on purpose
  (author names, occasional unit notation). Folding hurts recall
  on the rare thing it'd help.
- Soundex / phonetic.
- NGram over prose. That's what the trigram tier is for, and it
  only makes sense on patch content.
