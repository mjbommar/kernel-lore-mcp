//! Metadata tier — columnar (Arrow in-memory, Parquet on disk).
//!
//! Answers structured queries (`f:`, `dfn:`, `dfhh:`, `rt:`, `list:`,
//! `mid:`, trailer filters, series-version lookups) without touching
//! the body tiers. See docs/indexing/metadata-tier.md for the full
//! column list; highlights for reviewer context:
//!
//!     schema_version       bumped on breaking changes (see `schema`)
//!     message_id           unique within list; stored verbatim
//!     list                 dictionary-encoded (~350 distinct)
//!     from_addr, from_name
//!     subject_raw, subject_normalized
//!     subject_tags         [RFC], [RFT], [GIT PULL], [ANNOUNCE], [RESEND]
//!     series_version       v1/v2/v3/... from subject
//!     series_index         N of M
//!     is_cover_letter
//!     date, in_reply_to, references[]
//!     tid                  thread id precomputed at ingest
//!     touched_files[], touched_functions[]
//!     has_patch
//!     patch_stats          files_changed / insertions / deletions
//!     trailers             signed_off_by[], reviewed_by[], acked_by[],
//!                          tested_by[], co_developed_by[], reported_by[],
//!                          fixes[], link[], closes[]
//!     cross_posted_to[]    lists beyond this one carrying the same mid
//!     body_offset, body_length, body_sha256  (pointer into store)
//!
//! Implementation lands in a follow-up PR.
