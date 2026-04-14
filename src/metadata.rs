//! Metadata tier: writer. Parquet readers / query land in a follow-up.

#![allow(dead_code)]
//!
//! Responsibilities:
//!   * build `arrow::RecordBatch` batches from ingested messages
//!   * emit one Parquet file per (list, ingest_run) with zstd + bloom
//!     on `message_id`
//!   * stamp `schema_version` on every row

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanBuilder, ListBuilder, RecordBatch, StringBuilder, TimestampNanosecondBuilder,
    UInt32Builder, UInt64Builder,
};

use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use crate::error::{Error, Result};
use crate::parse::ParsedMessage;
use crate::schema::{self, SCHEMA_VERSION, metadata_schema};
use crate::store::StoreOffset;

/// A fully-flattened row ready for the Parquet writer.
pub struct MetadataRow<'a> {
    pub list: &'a str,
    pub shard: &'a str,
    pub commit_oid: &'a str,
    pub offset: StoreOffset,
    pub body_sha256_hex: String,
    pub body_length: u64,
    pub parsed: ParsedMessage,
}

/// Accumulating builder. Call `push` per message; `finish` emits one
/// Parquet file under `<data_dir>/metadata/<list>/<run_id>.parquet`.
pub struct MetadataBatch {
    message_id: StringBuilder,
    list: StringBuilder,
    shard: StringBuilder,
    commit_oid: StringBuilder,
    from_addr: StringBuilder,
    from_name: StringBuilder,
    subject_raw: StringBuilder,
    subject_normalized: StringBuilder,
    subject_tags: ListBuilder<StringBuilder>,
    date: TimestampNanosecondBuilder,
    in_reply_to: StringBuilder,
    references: ListBuilder<StringBuilder>,
    tid: StringBuilder,
    series_version: UInt32Builder,
    series_index: UInt32Builder,
    series_total: UInt32Builder,
    is_cover_letter: BooleanBuilder,
    has_patch: BooleanBuilder,
    touched_files: ListBuilder<StringBuilder>,
    touched_functions: ListBuilder<StringBuilder>,
    files_changed: UInt32Builder,
    insertions: UInt32Builder,
    deletions: UInt32Builder,
    signed_off_by: ListBuilder<StringBuilder>,
    reviewed_by: ListBuilder<StringBuilder>,
    acked_by: ListBuilder<StringBuilder>,
    tested_by: ListBuilder<StringBuilder>,
    co_developed_by: ListBuilder<StringBuilder>,
    reported_by: ListBuilder<StringBuilder>,
    fixes: ListBuilder<StringBuilder>,
    link: ListBuilder<StringBuilder>,
    closes: ListBuilder<StringBuilder>,
    cc_stable: ListBuilder<StringBuilder>,
    body_offset: UInt64Builder,
    body_length: UInt64Builder,
    body_sha256: StringBuilder,
    schema_version: UInt32Builder,

    rows: usize,
}

impl Default for MetadataBatch {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataBatch {
    pub fn new() -> Self {
        Self {
            message_id: StringBuilder::new(),
            list: StringBuilder::new(),
            shard: StringBuilder::new(),
            commit_oid: StringBuilder::new(),
            from_addr: StringBuilder::new(),
            from_name: StringBuilder::new(),
            subject_raw: StringBuilder::new(),
            subject_normalized: StringBuilder::new(),
            subject_tags: ListBuilder::new(StringBuilder::new()),
            date: TimestampNanosecondBuilder::new().with_timezone("UTC"),
            in_reply_to: StringBuilder::new(),
            references: ListBuilder::new(StringBuilder::new()),
            tid: StringBuilder::new(),
            series_version: UInt32Builder::new(),
            series_index: UInt32Builder::new(),
            series_total: UInt32Builder::new(),
            is_cover_letter: BooleanBuilder::new(),
            has_patch: BooleanBuilder::new(),
            touched_files: ListBuilder::new(StringBuilder::new()),
            touched_functions: ListBuilder::new(StringBuilder::new()),
            files_changed: UInt32Builder::new(),
            insertions: UInt32Builder::new(),
            deletions: UInt32Builder::new(),
            signed_off_by: ListBuilder::new(StringBuilder::new()),
            reviewed_by: ListBuilder::new(StringBuilder::new()),
            acked_by: ListBuilder::new(StringBuilder::new()),
            tested_by: ListBuilder::new(StringBuilder::new()),
            co_developed_by: ListBuilder::new(StringBuilder::new()),
            reported_by: ListBuilder::new(StringBuilder::new()),
            fixes: ListBuilder::new(StringBuilder::new()),
            link: ListBuilder::new(StringBuilder::new()),
            closes: ListBuilder::new(StringBuilder::new()),
            cc_stable: ListBuilder::new(StringBuilder::new()),
            body_offset: UInt64Builder::new(),
            body_length: UInt64Builder::new(),
            body_sha256: StringBuilder::new(),
            schema_version: UInt32Builder::new(),
            rows: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn push(&mut self, row: MetadataRow<'_>) {
        // Require a message_id. Drop rows without one (can't thread or
        // cite them anyway).
        let Some(mid) = row.parsed.message_id.clone() else {
            return;
        };

        self.message_id.append_value(&mid);
        self.list.append_value(row.list);
        self.shard.append_value(row.shard);
        self.commit_oid.append_value(row.commit_oid);
        append_opt(&mut self.from_addr, row.parsed.from_addr.as_deref());
        append_opt(&mut self.from_name, row.parsed.from_name.as_deref());
        append_opt(&mut self.subject_raw, row.parsed.subject_raw.as_deref());
        append_opt(
            &mut self.subject_normalized,
            row.parsed.subject_normalized.as_deref(),
        );
        append_list(&mut self.subject_tags, &row.parsed.subject_tags);
        match row.parsed.date_unix_ns {
            Some(ns) => self.date.append_value(ns),
            None => self.date.append_null(),
        }
        append_opt(&mut self.in_reply_to, row.parsed.in_reply_to.as_deref());
        append_list(&mut self.references, &row.parsed.references);
        // tid is computed later (cross-message join); leave null for now.
        self.tid.append_null();
        self.series_version.append_value(row.parsed.series_version);
        append_u32_opt(&mut self.series_index, row.parsed.series_index);
        append_u32_opt(&mut self.series_total, row.parsed.series_total);
        self.is_cover_letter
            .append_value(row.parsed.is_cover_letter);
        self.has_patch.append_value(row.parsed.has_patch);
        append_list(&mut self.touched_files, &row.parsed.touched_files);
        append_list(&mut self.touched_functions, &row.parsed.touched_functions);
        append_u32_opt(&mut self.files_changed, row.parsed.files_changed);
        append_u32_opt(&mut self.insertions, row.parsed.insertions);
        append_u32_opt(&mut self.deletions, row.parsed.deletions);
        append_list(&mut self.signed_off_by, &row.parsed.signed_off_by);
        append_list(&mut self.reviewed_by, &row.parsed.reviewed_by);
        append_list(&mut self.acked_by, &row.parsed.acked_by);
        append_list(&mut self.tested_by, &row.parsed.tested_by);
        append_list(&mut self.co_developed_by, &row.parsed.co_developed_by);
        append_list(&mut self.reported_by, &row.parsed.reported_by);
        append_list(&mut self.fixes, &row.parsed.fixes);
        append_list(&mut self.link, &row.parsed.link);
        append_list(&mut self.closes, &row.parsed.closes);
        append_list(&mut self.cc_stable, &row.parsed.cc_stable);

        self.body_offset.append_value(row.offset.offset);
        self.body_length.append_value(row.body_length);
        self.body_sha256.append_value(&row.body_sha256_hex);
        self.schema_version.append_value(SCHEMA_VERSION);

        self.rows += 1;
    }

    pub fn finish(mut self) -> Result<RecordBatch> {
        let schema = metadata_schema();
        let arrays: Vec<ArrayRef> = vec![
            Arc::new(self.message_id.finish()),
            Arc::new(self.list.finish()),
            Arc::new(self.shard.finish()),
            Arc::new(self.commit_oid.finish()),
            Arc::new(self.from_addr.finish()),
            Arc::new(self.from_name.finish()),
            Arc::new(self.subject_raw.finish()),
            Arc::new(self.subject_normalized.finish()),
            Arc::new(self.subject_tags.finish()),
            Arc::new(self.date.finish()),
            Arc::new(self.in_reply_to.finish()),
            Arc::new(self.references.finish()),
            Arc::new(self.tid.finish()),
            Arc::new(self.series_version.finish()),
            Arc::new(self.series_index.finish()),
            Arc::new(self.series_total.finish()),
            Arc::new(self.is_cover_letter.finish()),
            Arc::new(self.has_patch.finish()),
            Arc::new(self.touched_files.finish()),
            Arc::new(self.touched_functions.finish()),
            Arc::new(self.files_changed.finish()),
            Arc::new(self.insertions.finish()),
            Arc::new(self.deletions.finish()),
            Arc::new(self.signed_off_by.finish()),
            Arc::new(self.reviewed_by.finish()),
            Arc::new(self.acked_by.finish()),
            Arc::new(self.tested_by.finish()),
            Arc::new(self.co_developed_by.finish()),
            Arc::new(self.reported_by.finish()),
            Arc::new(self.fixes.finish()),
            Arc::new(self.link.finish()),
            Arc::new(self.closes.finish()),
            Arc::new(self.cc_stable.finish()),
            Arc::new(self.body_offset.finish()),
            Arc::new(self.body_length.finish()),
            Arc::new(self.body_sha256.finish()),
            Arc::new(self.schema_version.finish()),
        ];
        Ok(RecordBatch::try_new(schema, arrays)?)
    }
}

/// Write a finished RecordBatch to disk as Parquet + zstd.
pub fn write_parquet(
    data_dir: &Path,
    list: &str,
    run_id: &str,
    batch: &RecordBatch,
) -> Result<PathBuf> {
    let list_dir = data_dir.join("metadata").join(list);
    fs::create_dir_all(&list_dir)?;
    let path = list_dir.join(format!("{run_id}.parquet"));

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).map_err(|e| Error::State(format!("zstd level: {e}")))?,
        ))
        .set_bloom_filter_enabled(false)
        .set_column_bloom_filter_enabled(schema::COL_MESSAGE_ID.into(), true)
        .build();

    let file = File::create(&path)?;
    let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(props))?;
    writer.write(batch)?;
    writer.close()?;
    Ok(path)
}

fn append_opt(b: &mut StringBuilder, v: Option<&str>) {
    match v {
        Some(s) => b.append_value(s),
        None => b.append_null(),
    }
}

fn append_u32_opt(b: &mut UInt32Builder, v: Option<u32>) {
    match v {
        Some(x) => b.append_value(x),
        None => b.append_null(),
    }
}

fn append_list(b: &mut ListBuilder<StringBuilder>, values: &[String]) {
    if values.is_empty() {
        b.append(false);
        return;
    }
    for v in values {
        b.values().append_value(v);
    }
    b.append(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_message;
    use tempfile::tempdir;

    #[test]
    fn batch_and_write() {
        let mut batch = MetadataBatch::new();

        let msg = b"\
From: Alice <alice@example.com>\r\n\
Subject: [PATCH v2 1/3] ksmbd: tighten ACL bounds\r\n\
Date: Mon, 14 Apr 2026 12:00:00 +0000\r\n\
Message-ID: <m1@x>\r\n\
In-Reply-To: <cover@x>\r\n\
\r\n\
Prose here.\r\n\
\r\n\
Signed-off-by: Alice <alice@example.com>\r\n\
---\r\n\
diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c\r\n\
--- a/fs/smb/server/smbacl.c\r\n\
+++ b/fs/smb/server/smbacl.c\r\n\
@@ -1,1 +1,2 @@ int foo(int x)\r\n\
 a\r\n\
+b\r\n\
";
        let parsed = parse_message(msg);
        batch.push(MetadataRow {
            list: "linux-cifs",
            shard: "0",
            commit_oid: "0000000000000000000000000000000000000000",
            offset: StoreOffset {
                segment_id: 0,
                offset: 0,
                length: 42,
            },
            body_sha256_hex: "deadbeef".repeat(8),
            body_length: msg.len() as u64,
            parsed,
        });

        assert_eq!(batch.len(), 1);
        let rb = batch.finish().unwrap();
        assert_eq!(rb.num_rows(), 1);

        let tmp = tempdir().unwrap();
        let path = write_parquet(tmp.path(), "linux-cifs", "run-001", &rb).unwrap();
        assert!(path.exists());
        assert!(path.metadata().unwrap().len() > 0);
    }

    #[test]
    fn drops_rows_without_message_id() {
        let mut batch = MetadataBatch::new();
        let parsed = parse_message(b"Subject: no mid\r\n\r\nbody\r\n");
        batch.push(MetadataRow {
            list: "x",
            shard: "0",
            commit_oid: "0".repeat(40).as_str(),
            offset: StoreOffset {
                segment_id: 0,
                offset: 0,
                length: 0,
            },
            body_sha256_hex: "0".repeat(64),
            body_length: 0,
            parsed,
        });
        assert!(batch.is_empty());
    }
}
