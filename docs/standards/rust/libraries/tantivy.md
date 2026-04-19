# tantivy 0.26

Rust-specific (no Python parallel).

Pinned: `tantivy = { version = "0.26", default-features = false, features = ["mmap"] }`.
Stemmer feature **never enabled** (see `CLAUDE.md` and
`../../../indexing/tokenizer-spec.md`).

tantivy owns the BM25 tier only. Metadata analytical reads are
Arrow/Parquet; metadata point lookups are SQLite (`over.db`);
trigrams are `fst`+`roaring`. See
`../../../architecture/four-tier-index.md`,
`../../../architecture/over-db.md`,
and `../../../indexing/bm25-tier.md`.

---

## Why 0.26 and not tantivy-py

We bind tantivy ourselves in the PyO3 module (`_core`). Reasons:

- tantivy-py lags upstream; we'd inherit its pin decisions.
- We want a custom analyzer (`kernel_prose`) plus strict control
  over `IndexRecordOption`.
- The Python boundary is already ours; adding another PyO3
  extension just for tantivy is noise.

See `CLAUDE.md` — "tantivy-py: NOT USED."

---

## Schema — lives in `src/schema.rs`

Central ownership of field definitions. The metadata tier and
the BM25 tier both reference `SCHEMA_VERSION` from this module
so a schema bump fails loudly (analyzer-fingerprint sidecar
check, Parquet schema-version column).

Shape (planned; not yet implemented):

```rust
use tantivy::schema::*;

pub struct BM25Schema {
    pub schema: Schema,
    pub f_message_id: Field,
    pub f_list: Field,
    pub f_subject_norm: Field,
    pub f_body_prose: Field,
    pub f_date: Field,    // i64 timestamp ns (also in metadata)
}

pub fn build_bm25_schema() -> BM25Schema {
    let mut b = SchemaBuilder::new();

    // Atomic identifiers — STRING field, not tokenized.
    let f_message_id = b.add_text_field("message_id", STRING | STORED);
    let f_list       = b.add_text_field("list", STRING);   // dict-low-card

    // Prose body + normalized subject — our kernel_prose analyzer.
    let text_opts = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("kernel_prose")
            .set_index_option(IndexRecordOption::WithFreqs),  // NO positions
    );
    let f_subject_norm = b.add_text_field("subject_norm", text_opts.clone());
    let f_body_prose   = b.add_text_field("body_prose",   text_opts);

    let f_date = b.add_i64_field("date", INDEXED);

    BM25Schema { schema: b.build(), f_message_id, f_list,
                 f_subject_norm, f_body_prose, f_date }
}
```

Key choices:

- **`IndexRecordOption::WithFreqs`** (not `WithFreqsAndPositions`).
  Positions cost 30-50% of tier size. We don't do phrase queries
  on prose in v1. Router rejects `"phrase"` on `body_prose`
  explicitly (see `router.rs`); no silent degrade.
- **No stemming, no stopwords, no asciifolding.** Enforced by
  the analyzer definition below.
- **`message_id` is `STRING | STORED`** — atomic, retrievable.

---

## Analyzer registration — MUST after every `Index::open`

Custom analyzers (`kernel_prose`) are runtime registrations
against the index's tokenizer manager. **They are not persisted
with the index.** Opening an index and skipping registration
silently produces wrong BM25 scores and missed matches.

```rust
use tantivy::tokenizer::*;

fn register_analyzers(index: &tantivy::Index) -> tantivy::Result<()> {
    let kernel_prose = TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(80))
        .filter(LowerCaser)
        // NO Stemmer — feature not enabled and never will be.
        // NO StopWordFilter.
        // NO AsciiFoldingFilter.
        .filter(KernelIdentTokenFilter::default())  // our custom filter
        .build();

    index.tokenizers().register("kernel_prose", kernel_prose);

    // Also register the "raw" / STRING-field identity analyzer for
    // message_id, cite_key, etc. tantivy includes this by default
    // as "raw" but re-registering is cheap and defensive.

    Ok(())
}
```

Our custom filter `KernelIdentTokenFilter` handles identifier
sub-token emission (`vector_mmsg_rx` → `vector_mmsg_rx`,
`vector`, `mmsg`, `rx` at `position_inc=0`; `__skb_unlink` stays
distinct from `skb_unlink`). Spec in
`../../../indexing/tokenizer-spec.md`.

### Fingerprint sidecar

Because analyzer registration is runtime, we guard against
drift:

- At index-create time, compute SHA-256 over the analyzer
  config (stringified: filter names, params) and write
  `analyzers.fingerprint` next to the index directory.
- Every read-open verifies the fingerprint matches. Mismatch →
  loud `Error::State` with guidance to run `reindex`.

---

## `IndexWriter` — single instance

One writer, system-wide, ever. Ingest process holds it. See
`../design/concurrency.md` (single-writer discipline).

```rust
let writer: IndexWriter = index.writer(heap_size)?;
// heap_size: ~128 MB for our corpus; each doc is small.
```

Notes:

- `index.writer(heap)` takes an exclusive meta lock. A second
  caller blocks. We layer our own lockfile
  (`state::writer_lockfile()`) on top so we fail fast with a
  clear message instead of hanging.
- **`writer.commit()`** is the durability point. After commit,
  bump `state::generation`.
- **Don't `writer.rollback()`** in our flow — we never want to
  leave a half-committed index. If ingest fails mid-batch, we
  abort the whole session without committing.

### Multi-threaded indexing within a writer

`IndexWriter` already uses multiple threads internally (default
`num_threads` proportional to available heap). We do NOT call
`writer.add_document` from multiple rayon tasks — the writer is
the single consumer of a channel we feed from one thread. The
parallelism in ingest comes from per-shard workers producing
records; a collector thread drains them into the writer.

---

## `ReloadPolicy::Manual` + generation stat

Readers don't auto-reload. Every query entry:

```rust
let gen = state::read_generation()?;
if gen != last_seen_gen {
    reader.reload()?;
    last_seen_gen = gen;
}
let searcher = reader.searcher();
```

Why manual:

- `ReloadPolicy::OnCommit` polls every 50ms; we have a
  generation file we can stat in ~µs.
- Multi-worker uvicorn deployments stay coherent via the same
  file.
- Explicit point where we can log "reader reloaded to
  generation N" for debugging.

See `bm25.rs` doc comment for the canonical spec.

---

## Query patterns we use

- **Term queries** on `STRING` fields (`message_id`, `list`).
- **BM25** on `body_prose`, `subject_norm`, combined via
  `BooleanQuery` with `Occur::Should`.
- **Range queries** on `date` (i64). Push most date filtering
  to the metadata tier, but tantivy can filter further.
- **No phrase queries on prose.** Router rejects.
- **No fuzzy queries.** Typo tolerance is explicitly off.

Scoring:

- Default BM25. `k1 = 1.2`, `b = 0.75` — tantivy defaults.
- Per-hit scores returned via `TopDocs::with_limit`. We merge
  with metadata/trigram results in `router.rs`.

---

## What 0.26 changed that tripped older code

- **`IndexRecordOption` names** clarified vs 0.22 (was
  `Position` now includes variants for freq-only combinations).
- **Tokenizer trait** took a `TokenizerManager` generic in
  internal APIs; public `register` API is stable.
- **Rust edition 2021 minimum.** We're on 2024 so no issue.
- **`max-parallelism` feature removed** — rayon is internal. We
  don't pass these features explicitly.
- **No default features we turn on** other than `mmap`. Keeps
  compile time and wheel size down.

If you see tutorials from 2022-2023 using `SchemaBuilder::new()`
idioms, they're still valid. The migration pain from earlier
majors (0.18 → 0.19) is behind us.

---

## Don't-do list

| Anti-pattern | Why |
|---|---|
| Enabling the `stemmer` feature | Mangles `vector_mmsg_rx`. Project-level policy. |
| Storing patch bodies in tantivy | They go to the trigram tier. |
| Running phrase queries on `body_prose` | No positions; would silently degrade. |
| Opening a writer in the server process | Breaks single-writer invariant. Use reader. |
| `ReloadPolicy::OnCommit` | We have a cheaper signal (generation file). |
| `forceMerge` on every commit | Write-amplification. Segments merge in background. |
| Skipping analyzer registration after `Index::open` | Silent scoring corruption. |

---

## Checklist for a tantivy change

1. Updated `schema.rs` if fields changed.
2. Bumped `SCHEMA_VERSION` if indexing semantics changed.
3. Analyzer registered after every `Index::open`, including in
   `reindex`.
4. Fingerprint sidecar regenerated.
5. Single-writer invariant preserved (no new `index.writer()`
   call outside ingest).
6. Reader-side reload unchanged (generation stat).
7. Benchmark committed (`criterion` on representative query set).

See also:
- `../../../indexing/bm25-tier.md` — tier-specific on-disk spec.
- `../../../indexing/tokenizer-spec.md` — tokenizer rules.
- `../design/boundaries.md` — tier interfaces.
- `../design/concurrency.md` — writer + reader discipline.
