# arrow 58 + parquet 58

Rust-specific (no Python parallel). The Python side reads the
same Parquet via pyarrow, but build-side is Rust-only.

Pinned:

```toml
arrow   = { version = "58", default-features = false, features = ["ipc"] }
parquet = { version = "58", default-features = false,
            features = ["arrow", "zstd", "async"] }
```

Versions locked to the same major (58) — mixing arrow 58 with
parquet 57 or vice versa links but fails at type-coercion
runtime.

---

## What we use it for

The metadata tier (`src/metadata.rs`). Arrow is the in-memory
`RecordBatch`; Parquet is the on-disk format. See
`../../../indexing/metadata-tier.md` for the column list.

Not used for:

- Trigram tier (fst + roaring, raw files).
- BM25 tier (tantivy manages its own on-disk).
- Compressed store (zstd segments + index Parquet — the index
  Parquet is the only "small Parquet" in the project).

---

## Building a `RecordBatch`

Build column by column, assemble once per batch (~N thousand
rows).

```rust
use arrow::array::{
    Int64Array, StringArray, DictionaryArray, ListArray,
    builder::{
        Int64Builder, StringBuilder, StringDictionaryBuilder,
        ListBuilder,
    },
};
use arrow::datatypes::{DataType, Field, Schema, Int32Type};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;

pub fn build_metadata_batch(rows: &[MetadataRow])
    -> crate::Result<RecordBatch>
{
    let schema = Arc::new(build_schema());   // see schema.rs

    // Scalar columns
    let mut mid  = StringBuilder::with_capacity(rows.len(), 64 * rows.len());
    let mut date = Int64Builder::with_capacity(rows.len());
    let mut list = StringDictionaryBuilder::<Int32Type>::with_capacity(
        rows.len(), 512 /* dict cardinality hint */, rows.len() * 16,
    );

    // List columns
    let mut touched_files = ListBuilder::new(StringBuilder::new());

    for row in rows {
        mid.append_value(&row.message_id);
        date.append_value(row.date_ns);
        list.append_value(&row.list)?;

        for f in &row.touched_files {
            touched_files.values().append_value(f);
        }
        touched_files.append(true);   // mark the list itself non-null
    }

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(mid.finish()),
            Arc::new(date.finish()),
            Arc::new(list.finish()),
            Arc::new(touched_files.finish()),
            // ...remaining columns
        ],
    ).map_err(Into::into)
}
```

Key points:

- **Pre-size builders** via `with_capacity` — avoids vector
  reallocation on a build of 100k+ rows.
- **Pass `Arc<Schema>`** — `RecordBatch::try_new` is strict
  about schema identity; keep one `Arc` per writer session.
- **`Int32Type` on the dict** keeps key width at 4 bytes; even
  lore's 350 lists fit easily.

---

## DictionaryArray for `list` and `touched_files`

`list` (the mailing list name: `linux-kernel`,
`linux-cifs`, ...) has ~350 distinct values across all of lore.
`StringArray` would store each repetition; `DictionaryArray`
stores the dict once and indices per row.

Savings: ~30× on the `list` column.

```rust
use arrow::array::StringDictionaryBuilder;
use arrow::datatypes::Int32Type;

let mut list_builder = StringDictionaryBuilder::<Int32Type>::new();
list_builder.append_value("linux-kernel")?;
list_builder.append_value("linux-kernel")?;
list_builder.append_value("linux-cifs")?;
let array = list_builder.finish();   // DictionaryArray<Int32Type>
```

`touched_files` is a different question: it's a list-of-strings
(variable length). Arrow has no "ListOfDictionary" off the
shelf — but the repeated string values *within* a row's list
dedupe badly, and the row-over-row dedupe rate is low for
touched files (each patch touches different files).

Decision: **keep `touched_files` as `List<Utf8>`, not dict.**
`zstd` page compression recovers most of the redundancy in
Parquet.

If a column has:

- Cardinality < 10k across the whole corpus → Dictionary.
- Cardinality > 10k but high intra-row repetition → Dictionary
  with `Int32` keys.
- Otherwise → Utf8 / List\<Utf8\>, rely on Parquet zstd.

---

## Parquet writer config

```rust
use parquet::file::properties::{WriterProperties, WriterVersion};
use parquet::basic::{Compression, Encoding, ZstdLevel};
use parquet::arrow::ArrowWriter;

fn writer_props() -> WriterProperties {
    WriterProperties::builder()
        .set_writer_version(WriterVersion::PARQUET_2_0)

        // Compression: zstd, level 3 is the sweet spot for read speed.
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(3).unwrap()))

        // Row group size: 128k rows. Smaller = more predicate
        // pushdown granularity; larger = better compression.
        .set_max_row_group_size(128 * 1024)

        // Column stats and bloom filters for the columns we filter on.
        .set_statistics_enabled(parquet::file::properties::EnabledStatistics::Page)
        .set_bloom_filter_enabled(true)

        // Per-column overrides — bloom filter on message_id (lookups),
        // but not on body_sha256 (too-long, rarely filtered).
        .set_column_bloom_filter_enabled(
            parquet::schema::types::ColumnPath::from(vec!["message_id".into()]),
            true,
        )
        .build()
}

fn write_metadata(path: &Path, batches: &[RecordBatch])
    -> crate::Result<()>
{
    let schema = batches[0].schema();
    let file = File::create(path)?;
    let mut w = ArrowWriter::try_new(file, schema, Some(writer_props()))?;
    for batch in batches {
        w.write(batch)?;
    }
    w.close()?;
    Ok(())
}
```

Rationale:

- **zstd level 3**, not 19. Level 19 compresses ~10% better,
  ~10× slower. Our metadata rebuilds in minutes — not a bottleneck
  — but readers want fast page decode. Level 3 wins.
- **Row group 128k rows**. Our per-row size is ~300 bytes after
  compression → row group ≈ 40 MB. Good for predicate pushdown
  without too many groups.
- **Page stats on every column** — cheap and enables skip.
- **Bloom filter on `message_id`** — the primary-key lookup
  column. Other high-cardinality columns (`from_addr`,
  `subject_normalized`) could get bloom filters; measure first.

---

## Predicate pushdown on read

Parquet 58's Rust reader supports:

- **Row group skip** by page-stats: `list = "linux-cifs"` skips
  any row group whose stats don't intersect that value.
- **Page skip** within a selected row group by page index
  (column index) — faster than row-by-row filtering.

Pattern:

```rust
use parquet::arrow::arrow_reader::{
    ArrowReaderOptions, ParquetRecordBatchReaderBuilder,
    RowFilter, ArrowPredicateFn,
};
use parquet::arrow::ProjectionMask;

fn scan_by_list(path: &Path, list: &str) -> crate::Result<Vec<RecordBatch>> {
    let file = File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;

    let schema  = builder.parquet_schema();
    let mask    = ProjectionMask::leaves(schema, 0..);   // all cols

    // Row filter on `list` dict column.
    let pred = ArrowPredicateFn::new(
        ProjectionMask::leaves(schema, vec![col_index_of(schema, "list")]),
        move |batch| {
            let col = batch.column(0).as_any().downcast_ref::<...>().unwrap();
            // Build a BooleanArray for matching rows...
        },
    );
    let reader = builder
        .with_row_filter(RowFilter::new(vec![Box::new(pred)]))
        .with_projection(mask)
        .build()?;

    reader.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}
```

In practice, hot queries filter on `list` + `date` first — both
of which have strong stats. A query like
`list:linux-cifs rt:30d` should skip >95% of row groups and
scan ~5% of the file.

---

## Schema evolution — `SCHEMA_VERSION` column

Every metadata Parquet carries a `schema_version: u32` column
(literal `SCHEMA_VERSION` at write time). Readers reject
mismatched versions loudly — see `schema.rs` doc.

On bump:

1. Bump `schema::SCHEMA_VERSION`.
2. `reindex --from-store` rebuilds every Parquet.
3. Old Parquets fail validation at open; the error message
   says "rebuild with `reindex`."

Do not try to read old-version Parquets with a new schema.
Silent column rename or type-coercion is the worst kind of
corruption.

---

## 58-series breaking changes to remember

- **`arrow::array::builder` names** settled (40s-50s had
  renames).
- **`parquet::arrow::ArrowWriter::close()`** must be called;
  dropping the writer without close loses the footer.
- **`WriterProperties::set_column_*` API** stable since 53;
  minor-version changes are additive.
- **`ZstdLevel::try_new`** fallible since 52 — pattern
  `try_new(3).unwrap()` on a compile-time-known good level is
  fine; non-constant levels must handle the Result.
- **async reader** is behind our `async` feature on parquet.
  We don't use it yet (no tokio), but we keep the feature on so
  the Python side's pyarrow can consume our files via the same
  version suite.

---

## Don't-do list

| Anti-pattern | Why |
|---|---|
| Mixing arrow 57 with parquet 58 (or any version skew) | Type-id collisions at runtime. |
| `Compression::ZSTD(ZstdLevel::try_new(19).unwrap())` | Slow; read-side doesn't benefit. |
| Writing without `close()` | Footer missing; file unreadable. |
| Storing `Vec<String>` as `FixedSizeList` | Our file counts per patch vary; `List<Utf8>` is right. |
| Statistics disabled on filter columns | Row-group skip breaks. |
| Bloom filter on every column | Storage bloat; measure which keys get lookups. |
| DictionaryArray with `Int64` keys | Wastes 4 bytes/row. `Int32` for < 2^31 distinct values (we always fit). |

---

## Checklist for a metadata-tier change

1. `schema.rs` updated; `SCHEMA_VERSION` bumped if types changed.
2. Dict cardinality hints reviewed (`list`, `subject_tags`).
3. Row-group size still in the 64k-256k range.
4. Bloom filter added for any new lookup key.
5. Reader writes a filter that exercises page skip on the new
   column.
6. `reindex --from-store` rebuilds existing data.

See also:
- `../../../indexing/metadata-tier.md` — column specification.
- `../design/data-structures.md` — Arrow vs Vec choice.
- `../design/errors.md` — `Arrow` and `Parquet` variants.
