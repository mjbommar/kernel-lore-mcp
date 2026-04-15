//! Shared field definitions for the metadata tier.
//!
//! Column names live here, nowhere else. Metadata writer + Parquet
//! reader + query router all import from this module. This is the
//! single source of truth; drift is a bug.

// Column-name constants are exported for downstream query + reader
// modules that land in later phases. Silence dead_code at module
// level so clippy doesn't trip on unused-yet pub items.
#![allow(dead_code)]

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

/// Bump when the on-disk schema breaks. Stored as a column on every
/// Parquet row so mixed-version readers bail loudly instead of
/// returning garbage.
pub const SCHEMA_VERSION: u32 = 1;

// ---- column names (use these constants, never string literals) ----

pub const COL_MESSAGE_ID: &str = "message_id";
pub const COL_LIST: &str = "list";
pub const COL_SHARD: &str = "shard";
pub const COL_COMMIT_OID: &str = "commit_oid";
pub const COL_FROM_ADDR: &str = "from_addr";
pub const COL_FROM_NAME: &str = "from_name";
pub const COL_SUBJECT_RAW: &str = "subject_raw";
pub const COL_SUBJECT_NORMALIZED: &str = "subject_normalized";
pub const COL_SUBJECT_TAGS: &str = "subject_tags";
pub const COL_DATE: &str = "date";
pub const COL_IN_REPLY_TO: &str = "in_reply_to";
pub const COL_REFERENCES: &str = "references";
pub const COL_TID: &str = "tid";
pub const COL_SERIES_VERSION: &str = "series_version";
pub const COL_SERIES_INDEX: &str = "series_index";
pub const COL_SERIES_TOTAL: &str = "series_total";
pub const COL_IS_COVER_LETTER: &str = "is_cover_letter";
pub const COL_HAS_PATCH: &str = "has_patch";
pub const COL_TOUCHED_FILES: &str = "touched_files";
pub const COL_TOUCHED_FUNCTIONS: &str = "touched_functions";
pub const COL_FILES_CHANGED: &str = "files_changed";
pub const COL_INSERTIONS: &str = "insertions";
pub const COL_DELETIONS: &str = "deletions";
pub const COL_SIGNED_OFF_BY: &str = "signed_off_by";
pub const COL_REVIEWED_BY: &str = "reviewed_by";
pub const COL_ACKED_BY: &str = "acked_by";
pub const COL_TESTED_BY: &str = "tested_by";
pub const COL_CO_DEVELOPED_BY: &str = "co_developed_by";
pub const COL_REPORTED_BY: &str = "reported_by";
pub const COL_FIXES: &str = "fixes";
pub const COL_LINK: &str = "link";
pub const COL_CLOSES: &str = "closes";
pub const COL_CC_STABLE: &str = "cc_stable";
pub const COL_BODY_SEGMENT_ID: &str = "body_segment_id";
pub const COL_BODY_OFFSET: &str = "body_offset";
pub const COL_BODY_LENGTH: &str = "body_length";
pub const COL_BODY_SHA256: &str = "body_sha256";
pub const COL_SCHEMA_VERSION: &str = "schema_version";

/// Build the canonical Arrow schema used by the metadata tier.
///
/// Column docs:
/// - `list` / `shard` are dictionary-encoded at write time (low-card).
/// - `date` is nanos-since-epoch UTC; missing values stay null.
/// - `series_version` uses 0 for "unversioned" (plain `[PATCH]`).
/// - `cc_stable` stores the raw stable tag text (e.g. `5.15+`, `# any`).
pub fn metadata_schema() -> Arc<Schema> {
    // List items are nullable=true to match the default ListBuilder<StringBuilder>
    // behaviour. We simply never append null item values in practice.
    let utf8_list = DataType::List(Arc::new(Field::new("item", DataType::Utf8, true)));

    Arc::new(Schema::new(vec![
        Field::new(COL_MESSAGE_ID, DataType::Utf8, false),
        Field::new(COL_LIST, DataType::Utf8, false),
        Field::new(COL_SHARD, DataType::Utf8, false),
        Field::new(COL_COMMIT_OID, DataType::Utf8, false),
        Field::new(COL_FROM_ADDR, DataType::Utf8, true),
        Field::new(COL_FROM_NAME, DataType::Utf8, true),
        Field::new(COL_SUBJECT_RAW, DataType::Utf8, true),
        Field::new(COL_SUBJECT_NORMALIZED, DataType::Utf8, true),
        Field::new(COL_SUBJECT_TAGS, utf8_list.clone(), true),
        Field::new(
            COL_DATE,
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            true,
        ),
        Field::new(COL_IN_REPLY_TO, DataType::Utf8, true),
        Field::new(COL_REFERENCES, utf8_list.clone(), true),
        Field::new(COL_TID, DataType::Utf8, true),
        Field::new(COL_SERIES_VERSION, DataType::UInt32, false),
        Field::new(COL_SERIES_INDEX, DataType::UInt32, true),
        Field::new(COL_SERIES_TOTAL, DataType::UInt32, true),
        Field::new(COL_IS_COVER_LETTER, DataType::Boolean, false),
        Field::new(COL_HAS_PATCH, DataType::Boolean, false),
        Field::new(COL_TOUCHED_FILES, utf8_list.clone(), true),
        Field::new(COL_TOUCHED_FUNCTIONS, utf8_list.clone(), true),
        Field::new(COL_FILES_CHANGED, DataType::UInt32, true),
        Field::new(COL_INSERTIONS, DataType::UInt32, true),
        Field::new(COL_DELETIONS, DataType::UInt32, true),
        Field::new(COL_SIGNED_OFF_BY, utf8_list.clone(), true),
        Field::new(COL_REVIEWED_BY, utf8_list.clone(), true),
        Field::new(COL_ACKED_BY, utf8_list.clone(), true),
        Field::new(COL_TESTED_BY, utf8_list.clone(), true),
        Field::new(COL_CO_DEVELOPED_BY, utf8_list.clone(), true),
        Field::new(COL_REPORTED_BY, utf8_list.clone(), true),
        Field::new(COL_FIXES, utf8_list.clone(), true),
        Field::new(COL_LINK, utf8_list.clone(), true),
        Field::new(COL_CLOSES, utf8_list.clone(), true),
        Field::new(COL_CC_STABLE, utf8_list, true),
        Field::new(COL_BODY_SEGMENT_ID, DataType::UInt32, false),
        Field::new(COL_BODY_OFFSET, DataType::UInt64, false),
        Field::new(COL_BODY_LENGTH, DataType::UInt64, false),
        Field::new(COL_BODY_SHA256, DataType::Utf8, false),
        Field::new(COL_SCHEMA_VERSION, DataType::UInt32, false),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_builds_and_field_names_are_unique() {
        let schema = metadata_schema();
        let mut names = std::collections::HashSet::new();
        for f in schema.fields() {
            assert!(
                names.insert(f.name().clone()),
                "duplicate column {}",
                f.name()
            );
        }
    }

    #[test]
    fn schema_version_column_present() {
        let schema = metadata_schema();
        assert!(schema.field_with_name(COL_SCHEMA_VERSION).is_ok());
    }
}
