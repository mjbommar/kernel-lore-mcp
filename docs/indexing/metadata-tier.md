# Indexing — metadata tier

Columnar (Arrow in-memory → Parquet on disk). Answers structured
queries without touching body tiers.

## Schema (v1)

| Column | Arrow type | Nullable | Notes |
|---|---|---|---|
| `message_id` | Utf8 | no | Unique within `list`; used as join key everywhere |
| `list` | DictionaryArray<Int32, Utf8> | no | ~350 distinct, dictionary-encoded |
| `from_addr` | Utf8 | yes | lowercased at ingest |
| `from_name` | Utf8 | yes | UTF-8 decoded, NFC |
| `subject_raw` | Utf8 | yes | verbatim |
| `subject_normalized` | Utf8 | yes | prefixes stripped |
| `date` | Timestamp<Nanosecond, UTC> | yes | from `Date:` header |
| `in_reply_to` | Utf8 | yes | angle brackets stripped |
| `references` | List<Utf8> | yes | flat list |
| `touched_files` | List<DictionaryArray<Int32, Utf8>> | yes | path strings |
| `touched_functions` | List<DictionaryArray<Int32, Utf8>> | yes | identifier strings |
| `has_patch` | Boolean | no | convenience |
| `body_offset` | UInt64 | no | byte offset into compressed store |
| `body_length` | UInt64 | no | uncompressed length |

## Why columnar + Parquet

- **From**, **list**, **touched_files** all have strong cardinality
  locality → dictionary encoding compresses 15–20×.
- Range scans on `date` are the most common query constraint;
  Parquet page stats let us skip most row groups.
- Row-group size: 256K rows (~tantivy segment size). zstd -9.
- Partition by `list` directory at the filesystem level. Queries
  that pin `list:` skip whole directories.

## Writes

Append-only per ingestion run. Each run emits one Parquet file per
`(list, ingest_date)`; we run `parquet-tools merge` monthly to
coalesce small files.

## Reads

Two patterns:

1. **Point lookup** by `message_id`: Bloom filter on `message_id`
   per row group → linear scan within the matched row group.
2. **Predicate + range**: Arrow predicate pushdown via Parquet
   row-group stats. `DataFusion` or hand-rolled; hand-rolled is
   faster for our narrow query set (we don't need SQL).

## Delete handling

Public-inbox `d` blobs produce a soft-delete row with just
`message_id` + `list`. The reader excludes message_ids present in
the tombstone set at query time. A compaction pass (v2) rewrites
Parquet files dropping tombstoned rows.
