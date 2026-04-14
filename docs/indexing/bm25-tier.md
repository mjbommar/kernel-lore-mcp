# Indexing — BM25 tier (tantivy)

## Schema

```rust
use tantivy::schema::*;

let mut builder = Schema::builder();

let prose_options = TextOptions::default()
    .set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("kernel_prose")
            .set_index_option(IndexRecordOption::WithFreqs)  // NO positions
    )
    .set_stored();

let raw_options = TextOptions::default()
    .set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("raw_lc")
            .set_index_option(IndexRecordOption::Basic)
    )
    .set_stored();

builder.add_text_field("message_id",          raw_options.clone());
builder.add_text_field("list",                raw_options.clone());
builder.add_text_field("from_addr",           raw_options.clone());
builder.add_text_field("subject_normalized",  prose_options.clone());
builder.add_text_field("body_prose",          prose_options.clone());
builder.add_date_field ("date", FAST | STORED);

let schema = builder.build();
```

## Why no positions

See [`../architecture/trade-offs.md`](../architecture/trade-offs.md).
Summary: saves ~30–50% of BM25 disk; phrase queries fall back to
trigram for code / router-level literal-match for prose.

## IndexWriter sizing

```rust
let writer = index.writer(512 * 1024 * 1024)?;   // 512 MiB RAM budget
```

One `IndexWriter` per process. tantivy handles segment merges.
Under free-threaded Python, the writer is called from one Rust
thread (ingestion worker) so no contention.

## Searcher / reader

```rust
let reader = index
    .reader_builder()
    .reload_policy(ReloadPolicy::Manual)  // we reload explicitly after swap
    .try_into()?;
```

Manual reload so query threads see a stable snapshot until we
explicitly swap. `reader.reload()?` after each ingestion commit.

## Tokenizer registration

```rust
register_kernel_analyzers(&index);  // see ../indexing/tokenizer-spec.md
```

Call after every `Index::open` AND every `Index::create_in_dir`.

## Stemmer feature gate

`Cargo.toml` does **not** list tantivy's `stemmer` feature. Do not
add it. If someone does, reviewers reject.

## Query parser

We don't use tantivy's built-in `QueryParser` for the top-level
query — our router has its own grammar. But we use `QueryParser`
internally for the BM25-specific portion of a dispatched query.

## Index location

`<data_dir>/bm25/` — one directory. Not per-list. BM25 benefits
from cross-list statistics (idf) being global.
