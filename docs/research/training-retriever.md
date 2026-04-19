# Training a kernel-specific retriever

**Status.** v1.1 work. v0.5 (now) captures the training signal
for free by writing the right columns to Parquet — **already
done** in [`../../src/schema.rs`](../../src/schema.rs) and
[`../../src/metadata.rs`](../../src/metadata.rs). The columns we
need for self-supervised pair mining are all there.

## North star

A <200 MB int8-quantized, CPU-inferable dense retriever trained
on the lore corpus, beating a generic `bge-small` baseline on
kernel-specific retrieval tasks. No human labels. Self-supervised
from corpus structure.

## Why bother

General-purpose embeddings tokenize `__skb_unlink` as
`_`, `_`, `skb`, `_`, `unlink` or worse. They treat `Fixes:
abc123` as three tokens. They have no idea that
`Reviewed-by: Miguel Ojeda` matters differently from
`Cc: Miguel Ojeda`. A kernel-corpus-trained retriever has cheap
access to all of that signal because our ingest already structured
it.

## Contrastive-pair recipes

All pairs derive from columns we already populate at ingest (see
[`../../src/schema.rs`](../../src/schema.rs) for the canonical
names):

### Recipe 1: subject ↔ body

- **Anchor.** `COL_SUBJECT_NORMALIZED` (prefixes stripped, tags
  extracted).
- **Positive.** Same row's prose body (split from patch at
  ingest — see [`../../src/parse.rs`](../../src/parse.rs)).
- **Negative.** In-batch random body from a different `tid`.

Cheap, high-volume, directly trains the "given a question-shaped
subject, recover the discussion" task.

### Recipe 2: series version chain

- **Anchor.** Body of `[PATCH vN]` at
  `(COL_SERIES_VERSION=N, COL_SERIES_INDEX=k)`.
- **Positive.** Body of `[PATCH v(N+1)]` at
  `(COL_SERIES_VERSION=N+1, COL_SERIES_INDEX=k)`, matched by
  subject normalization + `from_addr`.
- **Negative.** Body of a different series at the same
  `series_index`.

Teaches "same change, different version" — the `lore_patch_diff`
use case.

### Recipe 3: fixes-trailer → target SHA

- **Anchor.** Body of a message whose `COL_FIXES` trailer
  contains SHA `X`.
- **Positive.** Body of the earlier message that introduces `X`
  (same `tid` chain; `series_index` joined on shared
  `touched_files`).
- **Negative.** Body of a random unrelated patch touching
  different files.

Teaches "find the patch that broke this." Highest-value task for
a kernel security agent.

### Recipe 4: reply graph

- **Anchor.** Body of root message `m`.
- **Positive.** Body of any message with `COL_IN_REPLY_TO=m` or
  `m ∈ COL_REFERENCES`.
- **Negative.** Body from a different thread.

Teaches discussion coherence. Cheap and abundant.

### Recipe 5: trailer-authored ↔ reviewer-pattern

- **Anchor.** Body of a patch authored by
  `COL_SIGNED_OFF_BY[0]=A`.
- **Positive.** Body of a later message where
  `A ∈ COL_REVIEWED_BY` or `A ∈ COL_ACKED_BY` of some patch
  touching overlapping `COL_TOUCHED_FILES`.
- **Negative.** Random patch touching unrelated files.

Teaches maintainer-interest patterns. Useful for `lore_activity`
ranking.

## Held-out evaluation

Reserve a fraction of rows (say 5%, most recent) as a labeled
eval set:

- For Recipe 3, the `COL_FIXES` trailer is an **exact** labeled
  query→target pair. Hold 5% of these out of training; measure
  Recall@10 and MRR against baseline `bge-small`.
- For Recipe 2, the series-version transitions similarly
  provide labeled positives.

This gives us an honest eval without humans in the loop.

## Model-size budget

- <200 MB on disk after int8 quantization.
- CPU-inferable on the same `r7g.xlarge` that serves MCP. No
  GPU dependency at query time.
- Training on a rented GPU; inference in Rust via `candle` or
  via Python `onnxruntime`. Deferred decision.

## Why this is v1.1, not v1.0

v1 must ship the four-tier index and the MCP surface. A learned
retriever is a bolt-on atop that — it lands as an optional fifth
tier (`neural`), not a replacement for BM25 / trigram / metadata
Parquet / over.db.
The structured tiers win on precision for the queries we actually
get; the neural tier earns its keep on the "describe what you
want in English" surface.

v0.5 discipline: **write the columns now**. Retraining is cheap
once the signal is in Parquet; reconstructing signal we threw
away at ingest is not.

## Cross-references

- [`../../CLAUDE.md`](../../CLAUDE.md)
- [`../../src/schema.rs`](../../src/schema.rs) — canonical
  column names used above.
- [`../../src/parse.rs`](../../src/parse.rs) — prose/patch split.
- [`../architecture/overview.md`](../architecture/overview.md)
- [`../architecture/four-tier-index.md`](../architecture/four-tier-index.md)
- [`../architecture/over-db.md`](../architecture/over-db.md)
