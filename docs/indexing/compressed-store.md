# Indexing — compressed raw store

The source of truth. Indices are derived and rebuildable; the
store is not.

## Why a separate store

- tantivy can store bodies, but its `STORED` is not great at huge
  text blobs and couples body retrieval to the BM25 tier's life
  cycle.
- We want to recompute the metadata tier or trigram tier without
  re-walking lore shards. The compressed store lets us.

## Layout

```
<data_dir>/store/<list>/
    dict.zstd              # zstd dictionary trained on a 100MB sample
    segment-NNNNN.zst      # append-only; zstd long-range mode
    index.parquet          # message_id -> (segment, offset, length)
```

Per-list because compression dictionaries are per-corpus and one
dict trained on "all of lore" performs worse than per-list
dicts — each list's conventions (subject patterns, reviewer
trailer style) differ.

## Compression choice

- `zstd --train` on 100 MB sample per list → 64 KB dict.
- Each message independently compressed with `-19 --long=27` using
  the dict.
- Random access: read `(offset, length)` from index, decompress
  that message alone.

Expected ratios on lore mail:
- Headers: 15–20× (highly repetitive).
- Prose bodies: 4–6×.
- Patch bodies: 8–12× (context lines repeat across resends).

Overall ~6–8× on the full corpus → **20–35 GB** for all-of-lore
raw bodies.

## Writes

Append-only segment files. Roll a new segment at 1 GB. Segment is
sealed (fsync) before we commit the `index.parquet` row that
references it.

## Reads

Always via the metadata tier's `body_offset` / `body_length`
columns. Store is purely positional — no scans.

## Rebuilding indices

```bash
# Nuke all three index tiers but keep the store
rm -rf data/metadata data/trigram data/bm25
cargo run --release --bin reindex -- --from-store data/store
```

`reindex` walks the store (not lore) and reconstructs everything.
Takes ~1 hour on the reference box for full lore.
