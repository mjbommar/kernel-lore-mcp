//! Compressed raw message store — the source of truth.
//!
//! Layout under `<data_dir>/store/<list>/`:
//!     dict.zstd              -- zstd dictionary trained on a 100 MB sample
//!     segment-NNNNN.zst      -- append-only, one zstd frame per message
//!                               compressed with the dict
//!     index.parquet          -- message_id -> (segment_id, offset, length,
//!                                              body_sha256)
//!
//! Design notes:
//!   - Per-list dictionaries beat a single global dict.
//!   - Each message is its own zstd frame so random access by offset
//!     is one decompress call.
//!   - The store is the rebuildability contract: nuke all three index
//!     tiers and `reindex` walks the store, not lore, to rebuild.
//!   - Trigram tier's "confirm with real regex" step decompresses
//!     per candidate. See docs/indexing/trigram-tier.md for the
//!     candidate-count cap that keeps p95 bounded.
//!
//! Implementation lands in a follow-up PR.
