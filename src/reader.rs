//! Metadata tier — read path.
//!
//! Scans all Parquet files under `<data_dir>/metadata/<list>/*.parquet`
//! and exposes small, composable queries the router + MCP tools need:
//!
//!   * `fetch_message(message_id)` — point lookup
//!   * `activity(file|function, since)` — file or function touches over a
//!     date range, grouped by tid (tid null = own group per message)
//!   * `series_timeline(message_id)` — every message with matching
//!     subject_normalized + series_version, ordered by series_index
//!   * `expand_citation(sha_or_mid)` — universal lookup: match
//!     `message_id`, or scan `fixes[]` for a SHA, or scan prose references
//!
//! Scanning discipline: we open every Parquet file under the list dir
//! once per query, apply predicates in-memory (arrow compute kernels),
//! and short-circuit. With per-list directories and row-group stats
//! this is fine at our scale (few hundred MB of Parquet per list).

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use arrow::array::{
    Array, BooleanArray, Int64Array, ListArray, RecordBatch, StringArray, TimestampNanosecondArray,
    UInt32Array, UInt64Array,
};

use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::error::{Error, Result};
use crate::schema as sc;

/// One row's worth of metadata, flattened for consumption.
#[derive(Debug, Clone, Default)]
pub struct MessageRow {
    pub message_id: String,
    pub list: String,
    pub shard: String,
    pub commit_oid: String,
    pub from_addr: Option<String>,
    pub from_name: Option<String>,
    pub subject_raw: Option<String>,
    pub subject_normalized: Option<String>,
    pub subject_tags: Vec<String>,
    pub date_unix_ns: Option<i64>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub tid: Option<String>,
    pub series_version: u32,
    pub series_index: Option<u32>,
    pub series_total: Option<u32>,
    pub is_cover_letter: bool,
    pub has_patch: bool,
    pub touched_files: Vec<String>,
    pub touched_functions: Vec<String>,
    pub files_changed: Option<u32>,
    pub insertions: Option<u32>,
    pub deletions: Option<u32>,
    pub signed_off_by: Vec<String>,
    pub reviewed_by: Vec<String>,
    pub acked_by: Vec<String>,
    pub tested_by: Vec<String>,
    pub co_developed_by: Vec<String>,
    pub reported_by: Vec<String>,
    pub fixes: Vec<String>,
    pub link: Vec<String>,
    pub closes: Vec<String>,
    pub cc_stable: Vec<String>,
    pub suggested_by: Vec<String>,
    pub helped_by: Vec<String>,
    pub assisted_by: Vec<String>,
    pub trailers_json: Option<String>,
    pub body_segment_id: u32,
    pub body_offset: u64,
    pub body_length: u64,
    pub body_sha256: String,
    pub schema_version: u32,
}

/// Reader over all Parquet metadata files. Cheap to construct;
/// per-query scans re-open files so we get fresh mmap-backed reads
/// after a writer commit.
pub struct Reader {
    data_dir: PathBuf,
}

impl Reader {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_owned(),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Current index generation counter. Mirrors `State::generation()`;
    /// duplicated here so query-path callers don't need the full
    /// ingest-side `State` wrapper.
    pub fn generation(&self) -> Result<u64> {
        let path = self.data_dir.join("state").join("generation");
        match fs::read_to_string(&path) {
            Ok(s) => s
                .trim()
                .parse::<u64>()
                .map_err(|e| crate::error::Error::State(format!("generation parse: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Last-mutation time of the generation file, in nanoseconds since
    /// the Unix epoch (UTC). `None` if the file has never been written
    /// (fresh data_dir).
    pub fn generation_mtime_ns(&self) -> Result<Option<i64>> {
        let path = self.data_dir.join("state").join("generation");
        match fs::metadata(&path) {
            Ok(md) => {
                let mtime = md.modified()?;
                let dur = mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| crate::error::Error::State(format!("mtime pre-epoch: {e}")))?;
                Ok(Some(dur.as_nanos() as i64))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Enumerate every `.parquet` file under `<data_dir>/metadata/`.
    ///
    /// Files are sorted by **mtime descending** so the most recently
    /// written file comes first. This is load-bearing: `scan()`
    /// deduplicates by message_id and keeps the first occurrence, so
    /// mtime-descending guarantees freshest-wins regardless of what
    /// `run_id` the caller passed (run_id is caller-controlled on the
    /// PyO3 surface and only happens to be monotone in the default
    /// CLI path). File mtime is set by `fs::rename` in
    /// `metadata::write_parquet` (the atomic-rename step) and is
    /// monotone with real wall-clock time.
    fn parquet_files(&self) -> Result<Vec<PathBuf>> {
        let root = self.data_dir.join("metadata");
        let mut out: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        if !root.exists() {
            return Ok(Vec::new());
        }
        for list_entry in fs::read_dir(&root)? {
            let list_entry = list_entry?;
            if !list_entry.file_type()?.is_dir() {
                continue;
            }
            for file in fs::read_dir(list_entry.path())? {
                let file = file?;
                let path = file.path();
                if path.extension().and_then(|s| s.to_str()) == Some("parquet") {
                    let mtime = file
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH);
                    out.push((path, mtime));
                }
            }
        }
        // Sort by mtime descending — newest first. Deterministic
        // regardless of run_id naming conventions.
        out.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(out.into_iter().map(|(p, _)| p).collect())
    }

    /// Collect every row in the metadata tier into `out`. Used by the
    /// tid-rebuild pass which needs the full corpus in memory.
    pub fn scan_all(&self, out: &mut Vec<MessageRow>) -> Result<()> {
        self.scan(
            |_| true,
            |r| {
                out.push(r);
                true
            },
        )
    }

    /// Collect all rows with optional list + since filters. Used by
    /// the path tier's brute-force scan in v0.1.x; will be replaced
    /// by posting-list reads in v0.2.x.
    pub fn all_rows(
        &self,
        list: Option<&str>,
        since_unix_ns: Option<i64>,
    ) -> Result<Vec<MessageRow>> {
        let mut out = Vec::new();
        self.scan(
            |_| true,
            |r| {
                if let Some(l) = list {
                    if r.list != l {
                        return true;
                    }
                }
                if let Some(since) = since_unix_ns {
                    if let Some(d) = r.date_unix_ns {
                        if d < since {
                            return true;
                        }
                    }
                }
                out.push(r);
                true
            },
        )?;
        Ok(out)
    }

    /// Read the tid side-table at `<data_dir>/tid/tid.parquet` into a
    /// `message_id -> tid` map. Returns empty if the side-table
    /// hasn't been built yet.
    pub fn tid_lookup(&self) -> Result<std::collections::HashMap<String, String>> {
        let path = self.data_dir.join("tid").join("tid.parquet");
        if !path.exists() {
            return Ok(std::collections::HashMap::new());
        }
        let file = File::open(&path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        let reader = builder.build()?;
        let mut out = std::collections::HashMap::new();
        for batch in reader {
            let batch = batch?;
            let mid = downcast_string(&batch, "message_id")?;
            let tid = downcast_string(&batch, "tid")?;
            for i in 0..batch.num_rows() {
                out.insert(mid.value(i).to_owned(), tid.value(i).to_owned());
            }
        }
        Ok(out)
    }

    /// Read the tid side-table propagated_files / propagated_functions
    /// columns into a `message_id -> (files, functions)` map.
    #[allow(clippy::type_complexity)]
    pub fn propagated_lookup(
        &self,
    ) -> Result<std::collections::HashMap<String, (Vec<String>, Vec<String>)>> {
        let path = self.data_dir.join("tid").join("tid.parquet");
        if !path.exists() {
            return Ok(std::collections::HashMap::new());
        }
        let file = File::open(&path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        let reader = builder.build()?;
        let mut out = std::collections::HashMap::new();
        for batch in reader {
            let batch = batch?;
            let mid = downcast_string(&batch, "message_id")?;
            let files = downcast_list(&batch, "propagated_files")?;
            let funcs = downcast_list(&batch, "propagated_functions")?;
            for i in 0..batch.num_rows() {
                out.insert(
                    mid.value(i).to_owned(),
                    (list_strings(&files, i), list_strings(&funcs, i)),
                );
            }
        }
        Ok(out)
    }

    /// Apply `visit` to every row matching `filter`. Short-circuits when
    /// `visit` returns false.
    /// Core scan: walks every Parquet file, deduplicates by message_id.
    ///
    /// Because `parquet_files()` returns files in descending filename
    /// order (newest run_id first), the first occurrence of each
    /// message_id is the freshest. Subsequent duplicates (from
    /// dangling-OID re-walks) are skipped. This makes the "freshest
    /// row wins" contract from the ingest docs enforceable end-to-end
    /// without a separate dedup pass.
    fn scan<F, V>(&self, mut filter: F, mut visit: V) -> Result<()>
    where
        F: FnMut(&MessageRow) -> bool,
        V: FnMut(MessageRow) -> bool,
    {
        let mut seen = std::collections::HashSet::<String>::new();
        for path in self.parquet_files()? {
            let file = File::open(&path)?;
            let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
            let reader = builder.build()?;
            for batch in reader {
                let batch = batch?;
                let rows = materialize_batch(&batch)?;
                for row in rows {
                    if !seen.insert(row.message_id.clone()) {
                        continue; // duplicate — skip
                    }
                    if !filter(&row) {
                        continue;
                    }
                    if !visit(row) {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    /// Point lookup by Message-ID (across all lists).
    pub fn fetch_message(&self, message_id: &str) -> Result<Option<MessageRow>> {
        let needle = strip_angles(message_id).to_owned();
        let mut found: Option<MessageRow> = None;
        self.scan(
            |r| r.message_id == needle,
            |r| {
                found = Some(r);
                false
            },
        )?;
        Ok(found)
    }

    /// Return every row whose `touched_files` or `touched_functions`
    /// matches, with optional date lower-bound. `file` and `function`
    /// are ANDed when both are Some.
    pub fn activity(
        &self,
        file: Option<&str>,
        function: Option<&str>,
        since_unix_ns: Option<i64>,
        list: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        let f_path = file.map(str::to_owned);
        let f_func = function.map(str::to_owned);
        let list_filter = list.map(str::to_owned);

        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref lst) = list_filter {
                    if &r.list != lst {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                if let Some(ref p) = f_path {
                    if !r.touched_files.iter().any(|x| x == p) {
                        return false;
                    }
                }
                if let Some(ref fn_) = f_func {
                    if !r.touched_functions.iter().any(|x| x == fn_) {
                        return false;
                    }
                }
                true
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        // Newest first.
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }

    /// Given any message-id in a series, return all sibling messages
    /// sharing the same normalized subject, ordered by
    /// `(series_version, series_index)`.
    pub fn series_timeline(&self, message_id: &str) -> Result<Vec<MessageRow>> {
        let Some(seed) = self.fetch_message(message_id)? else {
            return Ok(Vec::new());
        };
        let Some(subj) = seed.subject_normalized.clone() else {
            return Ok(vec![seed]);
        };
        let list = seed.list.clone();
        let from = seed.from_addr.clone();
        let mut out = Vec::new();
        self.scan(
            |r| {
                r.list == list
                    && r.subject_normalized.as_deref() == Some(subj.as_str())
                    && r.from_addr == from
            },
            |r| {
                out.push(r);
                true
            },
        )?;
        out.sort_by_key(|r| (r.series_version, r.series_index.unwrap_or(0)));
        Ok(out)
    }

    // ---- low-level retrieval primitives (Phase 7) -------------------
    //
    // Each method below is one well-defined query against one tier.
    // The MCP layer exposes them as composable tools; agents stack
    // them themselves rather than us inventing higher-order workflows
    // for every new question.

    /// Exact-equality scan over one structured metadata column.
    ///
    /// `field` selects the column; `value` is matched verbatim
    /// (case-sensitive). For list-shaped columns (`touched_files`,
    /// `signed_off_by`, ...), the row matches if `value` appears in
    /// the list. `since_unix_ns` and `list` are global filters.
    pub fn eq(
        &self,
        field: EqField,
        value: &str,
        since_unix_ns: Option<i64>,
        list_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        let value_owned = value.to_owned();
        let list_owned = list_filter.map(str::to_owned);
        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned {
                    if &r.list != l {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                eq_field_matches(field, r, &value_owned)
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }

    /// `WHERE field IN (values)` — set-membership over one column.
    pub fn in_list(
        &self,
        field: EqField,
        values: &[String],
        since_unix_ns: Option<i64>,
        list_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        let want: std::collections::HashSet<String> = values.iter().cloned().collect();
        let list_owned = list_filter.map(str::to_owned);
        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned {
                    if &r.list != l {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                want.iter().any(|v| eq_field_matches(field, r, v))
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }

    /// Aggregate counts over the same predicate language as `eq`.
    /// Single full scan; cheap relative to materializing rows.
    pub fn count(
        &self,
        field: EqField,
        value: &str,
        since_unix_ns: Option<i64>,
        list_filter: Option<&str>,
    ) -> Result<CountSummary> {
        let value_owned = value.to_owned();
        let list_owned = list_filter.map(str::to_owned);
        let mut summary = CountSummary::default();
        let mut authors: std::collections::HashSet<String> = std::collections::HashSet::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned {
                    if &r.list != l {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                eq_field_matches(field, r, &value_owned)
            },
            |r| {
                summary.count += 1;
                if let Some(addr) = &r.from_addr {
                    authors.insert(addr.clone());
                }
                if let Some(d) = r.date_unix_ns {
                    summary.earliest_unix_ns = match summary.earliest_unix_ns {
                        Some(e) => Some(e.min(d)),
                        None => Some(d),
                    };
                    summary.latest_unix_ns = match summary.latest_unix_ns {
                        Some(l) => Some(l.max(d)),
                        None => Some(d),
                    };
                }
                true
            },
        )?;
        summary.distinct_authors = authors.len() as u64;
        Ok(summary)
    }

    /// Case-insensitive byte substring scan over `subject_raw`.
    /// Cheap because subjects are short; one full metadata scan.
    pub fn substr_subject(
        &self,
        needle: &str,
        list_filter: Option<&str>,
        since_unix_ns: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        let needle_lc = needle.to_ascii_lowercase();
        let list_owned = list_filter.map(str::to_owned);
        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned {
                    if &r.list != l {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                r.subject_raw
                    .as_ref()
                    .map(|s| s.to_ascii_lowercase().contains(&needle_lc))
                    .unwrap_or(false)
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }

    /// Substring scan inside one named trailer column.
    ///
    /// `name` is the trailer kind (case-insensitive): "fixes", "link",
    /// "reviewed-by", "acked-by", "tested-by", "signed-off-by",
    /// "co-developed-by", "reported-by", "closes", "cc-stable".
    /// `value_substring` is matched case-insensitively against any
    /// value in the column.
    pub fn substr_trailers(
        &self,
        name: &str,
        value_substring: &str,
        list_filter: Option<&str>,
        since_unix_ns: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        let name_lc = name.to_ascii_lowercase();
        let needle_lc = value_substring.to_ascii_lowercase();
        let list_owned = list_filter.map(str::to_owned);
        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned {
                    if &r.list != l {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                trailer_matches(r, &name_lc, &needle_lc)
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }

    /// DFA-only regex scan over one of {subject_raw, from_addr,
    /// body_prose, patch}. `body_prose` and `patch` require fetching
    /// the body from the compressed store; subject + from are scanned
    /// straight from the metadata tier.
    ///
    /// `anchor_required=true` rejects patterns starting with `.*` —
    /// keeps the trigram filter (when we add it) honest. v0.5 fully
    /// scans, so anchoring is policy not performance.
    pub fn regex(
        &self,
        field: RegexField,
        pattern: &str,
        anchor_required: bool,
        list_filter: Option<&str>,
        since_unix_ns: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        if anchor_required && (pattern.starts_with(".*") || pattern.starts_with("^.*")) {
            return Err(crate::error::Error::RegexComplexity(
                "anchored-only mode rejected leading `.*` — narrow the pattern \
                 (prefix anchor, list:/since: filter) or pass anchor_required=false"
                    .to_owned(),
            ));
        }
        // DFA build via regex-automata. Reject non-DFA-able patterns
        // (backrefs, lookaround) by using the dense::DFA::new builder
        // which only supports a safe subset.
        use regex_automata::dfa::dense;
        use regex_automata::util::syntax;
        let dfa = dense::DFA::builder()
            .syntax(syntax::Config::new().unicode(false).utf8(false))
            .build(pattern)
            .map_err(|e| {
                crate::error::Error::RegexComplexity(format!(
                    "pattern not DFA-buildable (backrefs / lookaround / size limit): {e}"
                ))
            })?;

        let list_owned = list_filter.map(str::to_owned);
        let mut out = Vec::new();
        self.scan(
            |r| {
                if let Some(ref l) = list_owned {
                    if &r.list != l {
                        return false;
                    }
                }
                if let Some(t) = since_unix_ns {
                    match r.date_unix_ns {
                        Some(d) if d >= t => {}
                        _ => return false,
                    }
                }
                match field {
                    RegexField::Subject => r
                        .subject_raw
                        .as_deref()
                        .map(|s| dfa_search(&dfa, s.as_bytes()))
                        .unwrap_or(false),
                    RegexField::FromAddr => r
                        .from_addr
                        .as_deref()
                        .map(|s| dfa_search(&dfa, s.as_bytes()))
                        .unwrap_or(false),
                    RegexField::Prose | RegexField::Patch => true, // confirm via body fetch below
                }
            },
            |r| {
                out.push(r);
                out.len() < limit * 4 // gather extra for body confirm pass
            },
        )?;

        if matches!(field, RegexField::Prose | RegexField::Patch) {
            let mut confirmed = Vec::with_capacity(out.len());
            for row in out {
                if let Some(body) = self.fetch_body(&row.message_id)? {
                    let bytes = match field {
                        RegexField::Patch => extract_patch_bytes(&body),
                        RegexField::Prose => extract_prose_bytes(&body),
                        _ => unreachable!(),
                    };
                    if let Some(b) = bytes {
                        if dfa_search(&dfa, &b) {
                            confirmed.push(row);
                        }
                    }
                }
                if confirmed.len() >= limit {
                    break;
                }
            }
            confirmed.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
            return Ok(confirmed);
        }

        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        out.truncate(limit);
        Ok(out)
    }

    /// Generalized message-vs-message diff. `mode` selects the view:
    /// `Patch` (just the diff payload), `Prose` (body minus patch
    /// minus quoted reply / sig), or `Raw` (verbatim RFC822 bytes).
    pub fn diff(&self, a: &str, b: &str, mode: DiffMode) -> Result<DiffResult> {
        let row_a = self
            .fetch_message(a)?
            .ok_or_else(|| crate::error::Error::State(format!("message_id {a:?} not found")))?;
        let row_b = self
            .fetch_message(b)?
            .ok_or_else(|| crate::error::Error::State(format!("message_id {b:?} not found")))?;
        let body_a = self
            .fetch_body(a)?
            .ok_or_else(|| crate::error::Error::State(format!("body for {a:?} missing")))?;
        let body_b = self
            .fetch_body(b)?
            .ok_or_else(|| crate::error::Error::State(format!("body for {b:?} missing")))?;
        let text_a = match mode {
            DiffMode::Raw => decode_lossy(&body_a),
            DiffMode::Patch => decode_lossy(&extract_patch_bytes(&body_a).unwrap_or_default()),
            DiffMode::Prose => decode_lossy(&extract_prose_bytes(&body_a).unwrap_or_default()),
        };
        let text_b = match mode {
            DiffMode::Raw => decode_lossy(&body_b),
            DiffMode::Patch => decode_lossy(&extract_patch_bytes(&body_b).unwrap_or_default()),
            DiffMode::Prose => decode_lossy(&extract_prose_bytes(&body_b).unwrap_or_default()),
        };
        Ok(DiffResult {
            row_a,
            row_b,
            text_a,
            text_b,
        })
    }

    /// Walk the reply graph from any starting message_id and return
    /// every message in the same conversation, ordered by date.
    /// Bounded by `max_messages` so a runaway thread can't OOM the
    /// server.
    pub fn thread(&self, message_id: &str, max_messages: usize) -> Result<Vec<MessageRow>> {
        use std::collections::{HashSet, VecDeque};
        let needle = strip_angles(message_id).to_owned();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::from([needle]);
        let mut collected: Vec<MessageRow> = Vec::new();

        while let Some(mid) = queue.pop_front() {
            if visited.contains(&mid) || collected.len() >= max_messages {
                continue;
            }
            visited.insert(mid.clone());
            let mut new_relations: Vec<String> = Vec::new();
            self.scan(
                |r| {
                    r.message_id == mid
                        || r.in_reply_to.as_deref() == Some(mid.as_str())
                        || r.references.iter().any(|p| p == &mid)
                },
                |r| {
                    if r.message_id == mid {
                        if let Some(parent) = r.in_reply_to.as_deref() {
                            if !parent.is_empty() {
                                new_relations.push(parent.to_owned());
                            }
                        }
                        for p in &r.references {
                            if !p.is_empty() {
                                new_relations.push(p.clone());
                            }
                        }
                    } else {
                        new_relations.push(r.message_id.clone());
                    }
                    collected.push(r);
                    collected.len() < max_messages
                },
            )?;
            for relation in new_relations {
                if !visited.contains(&relation) {
                    queue.push_back(relation);
                }
            }
        }

        let mut seen = HashSet::new();
        collected.retain(|r| seen.insert(r.message_id.clone()));
        collected.sort_by_key(|r| r.date_unix_ns.unwrap_or(i64::MIN));
        Ok(collected)
    }

    /// Free-text BM25 search over prose (body minus patch) +
    /// subject_normalized. Returns ranked hits with their scores.
    ///
    /// Phrase queries (`"..."`) are rejected — positions are off by
    /// design. Use `patch_search` for literal substrings in code.
    pub fn prose_search(&self, query: &str, limit: usize) -> Result<Vec<(MessageRow, f32)>> {
        self.prose_search_filtered(query, None, limit)
    }

    /// Like `prose_search` but with an optional tantivy-side list
    /// filter. When `list_filter` is set, tantivy only scores
    /// documents from that list, eliminating false negatives from
    /// post-filter starvation under corpus skew.
    pub fn prose_search_filtered(
        &self,
        query: &str,
        list_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(MessageRow, f32)>> {
        use crate::bm25::BmReader;
        let bm = BmReader::open(&self.data_dir)?;
        let top = bm.search_filtered(query, list_filter, limit)?;
        if top.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: std::collections::HashMap<String, f32> =
            top.iter().map(|(m, s)| (m.clone(), *s)).collect();

        let mut rows = Vec::new();
        self.scan(
            |r| wanted.contains_key(&r.message_id),
            |r| {
                rows.push(r);
                true
            },
        )?;
        let mut scored: Vec<(MessageRow, f32)> = rows
            .into_iter()
            .filter_map(|r| wanted.get(&r.message_id).map(|s| (r, *s)))
            .collect();
        scored.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    /// Substring search over patch content via the trigram tier,
    /// confirmed against the decompressed body.
    ///
    /// `needle` is a literal byte string. Matches use byte-exact
    /// comparison (no case folding). Returns at most `limit` rows
    /// newest-first.
    ///
    /// `list` (optional) restricts both the trigram segments scanned
    /// and the metadata lookup to one list.
    pub fn patch_search(
        &self,
        needle: &str,
        list: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRow>> {
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let lists = match list {
            Some(l) => vec![l.to_owned()],
            None => list_trigram_lists(&self.data_dir)?,
        };

        // Gather candidates across all (list, segment) trigram indices.
        let mut candidates: std::collections::HashSet<String> = std::collections::HashSet::new();
        for lst in &lists {
            for seg_dir in crate::trigram::list_segments(&self.data_dir, lst)? {
                let seg = crate::trigram::SegmentReader::open(&seg_dir)?;
                for mid in seg.candidates_for_substring(needle.as_bytes()) {
                    candidates.insert(mid.to_owned());
                }
            }
        }
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let mut hits = Vec::new();
        let list_filter = list.map(str::to_owned);
        let needle_bytes = needle.as_bytes().to_owned();
        self.scan(
            |r| {
                if let Some(ref lst) = list_filter {
                    if &r.list != lst {
                        return false;
                    }
                }
                candidates.contains(&r.message_id)
            },
            |r| {
                hits.push(r);
                true
            },
        )?;

        // Confirm: decompress + byte-scan. Dropping ambiguous hits.
        let mut confirmed = Vec::new();
        for row in hits {
            let store = crate::store::Store::open(&self.data_dir, &row.list)?;
            let body = store.read_at(row.body_segment_id, row.body_offset)?;
            if memchr::memmem::find(&body, &needle_bytes).is_some() {
                confirmed.push(row);
            }
        }

        confirmed.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        confirmed.truncate(limit);
        Ok(confirmed)
    }

    /// Like `patch_search` but with optional edit-distance tolerance.
    /// When `fuzzy_edits == 0`, behaves identically to `patch_search`.
    /// When `fuzzy_edits > 0`, the confirmation step uses
    /// `triple_accel::levenshtein_search` to find approximate matches.
    pub fn patch_search_fuzzy(
        &self,
        needle: &str,
        list: Option<&str>,
        limit: usize,
        fuzzy_edits: u32,
    ) -> Result<Vec<MessageRow>> {
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let lists = match list {
            Some(l) => vec![l.to_owned()],
            None => list_trigram_lists(&self.data_dir)?,
        };

        let mut candidates: std::collections::HashSet<String> = std::collections::HashSet::new();
        for lst in &lists {
            for seg_dir in crate::trigram::list_segments(&self.data_dir, lst)? {
                let seg = crate::trigram::SegmentReader::open(&seg_dir)?;
                for mid in seg.candidates_for_substring_fuzzy(needle.as_bytes(), fuzzy_edits) {
                    candidates.insert(mid.to_owned());
                }
            }
        }
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let mut hits = Vec::new();
        let list_filter = list.map(str::to_owned);
        let needle_bytes = needle.as_bytes().to_owned();
        self.scan(
            |r| {
                if let Some(ref lst) = list_filter {
                    if &r.list != lst {
                        return false;
                    }
                }
                candidates.contains(&r.message_id)
            },
            |r| {
                hits.push(r);
                true
            },
        )?;

        let mut confirmed = Vec::new();
        for row in hits {
            let store = crate::store::Store::open(&self.data_dir, &row.list)?;
            let body = store.read_at(row.body_segment_id, row.body_offset)?;
            let is_match = if fuzzy_edits == 0 {
                memchr::memmem::find(&body, &needle_bytes).is_some()
            } else {
                triple_accel::levenshtein::levenshtein_search_simd_with_opts(
                    &needle_bytes,
                    &body,
                    fuzzy_edits,
                    triple_accel::SearchType::Best,
                    triple_accel::levenshtein::LEVENSHTEIN_COSTS,
                    false,
                )
                .next()
                .is_some()
            };
            if is_match {
                confirmed.push(row);
            }
        }

        confirmed.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        confirmed.truncate(limit);
        Ok(confirmed)
    }

    /// Fetch the raw uncompressed body bytes for a given message-id.
    ///
    /// Does a point lookup over the metadata tier to find the
    /// `(list, segment_id, offset)` crosswalk, then a streaming
    /// zstd-decode from the compressed store.
    pub fn fetch_body(&self, message_id: &str) -> Result<Option<Vec<u8>>> {
        let Some(row) = self.fetch_message(message_id)? else {
            return Ok(None);
        };
        // body_offset from the metadata tier is the byte offset into the
        // compressed segment file. Segment id is derived per-list from the
        // active-segment scan during ingest; the first N-GB of data lives in
        // segment 0, which is what we write at v0.5 (no rollover in tests).
        // TODO(phase-2): add `segment_id` column to metadata so we aren't
        // relying on this convention on the reader side.
        let store = crate::store::Store::open(&self.data_dir, &row.list)?;
        let body = store.read_at(row.body_segment_id, row.body_offset)?;
        Ok(Some(body))
    }

    /// Universal lookup: message-id exact, or SHA in `fixes[]`, or CVE in
    /// subject. Returns up to `limit` rows, newest first.
    pub fn expand_citation(&self, token: &str, limit: usize) -> Result<Vec<MessageRow>> {
        let needle = strip_angles(token).to_owned();
        let sha_like = is_sha_prefix(&needle);
        let cve_like = is_cve_id(&needle);

        let mut out: Vec<MessageRow> = Vec::new();
        self.scan(
            |r| {
                if r.message_id == needle {
                    return true;
                }
                if sha_like && r.fixes.iter().any(|f| f.contains(&needle)) {
                    return true;
                }
                if cve_like {
                    if let Some(subj) = &r.subject_raw {
                        if subj.contains(&needle) {
                            return true;
                        }
                    }
                }
                false
            },
            |r| {
                out.push(r);
                out.len() < limit
            },
        )?;
        out.sort_by_key(|r| std::cmp::Reverse(r.date_unix_ns.unwrap_or(i64::MIN)));
        Ok(out)
    }
}

/// Equality-targetable column. The PyO3 wrapper maps Python strings
/// to these variants; downstream code stays type-safe.
#[derive(Debug, Clone, Copy)]
pub enum EqField {
    MessageId,
    List,
    FromAddr,
    InReplyTo,
    Tid,
    CommitOid,
    BodySha256,
    SubjectNormalized,
    /// list-shaped columns: row matches if value appears in the list.
    TouchedFile,
    TouchedFunction,
    Reference,
    SubjectTag,
    SignedOffBy,
    ReviewedBy,
    AckedBy,
    TestedBy,
    CoDevelopedBy,
    ReportedBy,
    Fixes,
    Link,
    Closes,
    CcStable,
}

impl EqField {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "message_id" => EqField::MessageId,
            "list" => EqField::List,
            "from_addr" => EqField::FromAddr,
            "in_reply_to" => EqField::InReplyTo,
            "tid" => EqField::Tid,
            "commit_oid" => EqField::CommitOid,
            "body_sha256" => EqField::BodySha256,
            "subject_normalized" => EqField::SubjectNormalized,
            "touched_files" | "touched_file" => EqField::TouchedFile,
            "touched_functions" | "touched_function" => EqField::TouchedFunction,
            "references" | "reference" => EqField::Reference,
            "subject_tags" | "subject_tag" | "tag" => EqField::SubjectTag,
            "signed_off_by" => EqField::SignedOffBy,
            "reviewed_by" => EqField::ReviewedBy,
            "acked_by" => EqField::AckedBy,
            "tested_by" => EqField::TestedBy,
            "co_developed_by" => EqField::CoDevelopedBy,
            "reported_by" => EqField::ReportedBy,
            "fixes" => EqField::Fixes,
            "link" => EqField::Link,
            "closes" => EqField::Closes,
            "cc_stable" => EqField::CcStable,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RegexField {
    Subject,
    FromAddr,
    Prose,
    Patch,
}

impl RegexField {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "subject_raw" | "subject" => RegexField::Subject,
            "from_addr" | "from" => RegexField::FromAddr,
            "body_prose" | "prose" => RegexField::Prose,
            "patch" => RegexField::Patch,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum DiffMode {
    Patch,
    Prose,
    Raw,
}

impl DiffMode {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "patch" => DiffMode::Patch,
            "prose" => DiffMode::Prose,
            "raw" => DiffMode::Raw,
            _ => return None,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct CountSummary {
    pub count: u64,
    pub distinct_authors: u64,
    pub earliest_unix_ns: Option<i64>,
    pub latest_unix_ns: Option<i64>,
}

pub struct DiffResult {
    pub row_a: MessageRow,
    pub row_b: MessageRow,
    pub text_a: String,
    pub text_b: String,
}

fn eq_field_matches(field: EqField, r: &MessageRow, value: &str) -> bool {
    match field {
        EqField::MessageId => r.message_id == value,
        EqField::List => r.list == value,
        EqField::FromAddr => r.from_addr.as_deref() == Some(value),
        EqField::InReplyTo => r.in_reply_to.as_deref() == Some(value),
        EqField::Tid => r.tid.as_deref() == Some(value),
        EqField::CommitOid => r.commit_oid == value,
        EqField::BodySha256 => r.body_sha256 == value,
        EqField::SubjectNormalized => r.subject_normalized.as_deref() == Some(value),
        EqField::TouchedFile => r.touched_files.iter().any(|x| x == value),
        EqField::TouchedFunction => r.touched_functions.iter().any(|x| x == value),
        EqField::Reference => r.references.iter().any(|x| x == value),
        EqField::SubjectTag => r.subject_tags.iter().any(|x| x == value),
        EqField::SignedOffBy => r.signed_off_by.iter().any(|x| x.contains(value)),
        EqField::ReviewedBy => r.reviewed_by.iter().any(|x| x.contains(value)),
        EqField::AckedBy => r.acked_by.iter().any(|x| x.contains(value)),
        EqField::TestedBy => r.tested_by.iter().any(|x| x.contains(value)),
        EqField::CoDevelopedBy => r.co_developed_by.iter().any(|x| x.contains(value)),
        EqField::ReportedBy => r.reported_by.iter().any(|x| x.contains(value)),
        EqField::Fixes => r.fixes.iter().any(|x| x.contains(value)),
        EqField::Link => r.link.iter().any(|x| x.contains(value)),
        EqField::Closes => r.closes.iter().any(|x| x.contains(value)),
        EqField::CcStable => r.cc_stable.iter().any(|x| x.contains(value)),
    }
}

fn trailer_matches(r: &MessageRow, name_lc: &str, needle_lc: &str) -> bool {
    let bag: &[String] = match name_lc {
        "fixes" => &r.fixes,
        "link" => &r.link,
        "closes" => &r.closes,
        "cc-stable" | "cc_stable" => &r.cc_stable,
        "signed-off-by" | "signed_off_by" => &r.signed_off_by,
        "reviewed-by" | "reviewed_by" => &r.reviewed_by,
        "acked-by" | "acked_by" => &r.acked_by,
        "tested-by" | "tested_by" => &r.tested_by,
        "co-developed-by" | "co_developed_by" => &r.co_developed_by,
        "reported-by" | "reported_by" => &r.reported_by,
        _ => return false,
    };
    bag.iter()
        .any(|v| v.to_ascii_lowercase().contains(needle_lc))
}

fn dfa_search(dfa: &regex_automata::dfa::dense::DFA<Vec<u32>>, haystack: &[u8]) -> bool {
    use regex_automata::Input;
    use regex_automata::dfa::Automaton;
    dfa.try_search_fwd(&Input::new(haystack))
        .ok()
        .flatten()
        .is_some()
}

fn extract_patch_bytes(body: &[u8]) -> Option<Vec<u8>> {
    // Find first "\ndiff --git " or leading "diff --git ".
    let needle = b"\ndiff --git ";
    if body.starts_with(b"diff --git ") {
        return Some(body.to_vec());
    }
    let pos = memchr::memmem::find(body, needle)?;
    Some(body[pos + 1..].to_vec())
}

fn extract_prose_bytes(body: &[u8]) -> Option<Vec<u8>> {
    let needle = b"\ndiff --git ";
    let end = if body.starts_with(b"diff --git ") {
        0
    } else {
        memchr::memmem::find(body, needle).unwrap_or(body.len())
    };
    if end == 0 {
        return Some(Vec::new());
    }
    Some(body[..end].to_vec())
}

fn decode_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn list_trigram_lists(data_dir: &Path) -> Result<Vec<String>> {
    let root = data_dir.join("trigram");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                out.push(name.to_owned());
            }
        }
    }
    out.sort();
    Ok(out)
}

// ---- internals ----

fn strip_angles(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(s)
}

fn is_sha_prefix(s: &str) -> bool {
    s.len() >= 7 && s.len() <= 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_cve_id(s: &str) -> bool {
    // CVE-YYYY-NNNN(+)
    let Some(tail) = s.strip_prefix("CVE-") else {
        return false;
    };
    let mut parts = tail.split('-');
    let (Some(y), Some(n), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    y.len() == 4
        && y.chars().all(|c| c.is_ascii_digit())
        && n.len() >= 4
        && n.chars().all(|c| c.is_ascii_digit())
}

/// Convert an Arrow RecordBatch into owned `MessageRow`s.
///
/// This is the one place we map column names → indices. If you add a
/// column to the schema, add it here.
#[allow(clippy::needless_borrow)]
fn materialize_batch(batch: &RecordBatch) -> Result<Vec<MessageRow>> {
    let schema = batch.schema();
    let get = |name: &str| -> Result<&dyn Array> {
        let idx = schema
            .index_of(name)
            .map_err(|e| Error::State(format!("missing column {name}: {e}")))?;
        Ok(batch.column(idx).as_ref())
    };

    let message_id = get(sc::COL_MESSAGE_ID)?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| Error::State("message_id not utf8".to_owned()))?;
    let list = downcast_string(batch, sc::COL_LIST)?;
    let shard = downcast_string(batch, sc::COL_SHARD)?;
    let commit_oid = downcast_string(batch, sc::COL_COMMIT_OID)?;
    let from_addr = downcast_string(batch, sc::COL_FROM_ADDR)?;
    let from_name = downcast_string(batch, sc::COL_FROM_NAME)?;
    let subject_raw = downcast_string(batch, sc::COL_SUBJECT_RAW)?;
    let subject_normalized = downcast_string(batch, sc::COL_SUBJECT_NORMALIZED)?;
    let subject_tags = downcast_list(batch, sc::COL_SUBJECT_TAGS)?;
    let date = batch
        .column(schema.index_of(sc::COL_DATE).unwrap())
        .as_any()
        .downcast_ref::<TimestampNanosecondArray>()
        .cloned();
    let in_reply_to = downcast_string(batch, sc::COL_IN_REPLY_TO)?;
    let references = downcast_list(batch, sc::COL_REFERENCES)?;
    let tid = downcast_string_opt(batch, sc::COL_TID);
    let series_version = downcast_u32(batch, sc::COL_SERIES_VERSION)?;
    let series_index = downcast_u32(batch, sc::COL_SERIES_INDEX)?;
    let series_total = downcast_u32(batch, sc::COL_SERIES_TOTAL)?;
    let is_cover_letter = downcast_bool(batch, sc::COL_IS_COVER_LETTER)?;
    let has_patch = downcast_bool(batch, sc::COL_HAS_PATCH)?;
    let touched_files = downcast_list(batch, sc::COL_TOUCHED_FILES)?;
    let touched_functions = downcast_list(batch, sc::COL_TOUCHED_FUNCTIONS)?;
    let files_changed = downcast_u32(batch, sc::COL_FILES_CHANGED)?;
    let insertions = downcast_u32(batch, sc::COL_INSERTIONS)?;
    let deletions = downcast_u32(batch, sc::COL_DELETIONS)?;
    let signed_off_by = downcast_list(batch, sc::COL_SIGNED_OFF_BY)?;
    let reviewed_by = downcast_list(batch, sc::COL_REVIEWED_BY)?;
    let acked_by = downcast_list(batch, sc::COL_ACKED_BY)?;
    let tested_by = downcast_list(batch, sc::COL_TESTED_BY)?;
    let co_developed_by = downcast_list(batch, sc::COL_CO_DEVELOPED_BY)?;
    let reported_by = downcast_list(batch, sc::COL_REPORTED_BY)?;
    let fixes = downcast_list(batch, sc::COL_FIXES)?;
    let link_trailers = downcast_list(batch, sc::COL_LINK)?;
    let closes = downcast_list(batch, sc::COL_CLOSES)?;
    let cc_stable = downcast_list(batch, sc::COL_CC_STABLE)?;
    let suggested_by = downcast_list_opt(batch, sc::COL_SUGGESTED_BY);
    let helped_by = downcast_list_opt(batch, sc::COL_HELPED_BY);
    let assisted_by = downcast_list_opt(batch, sc::COL_ASSISTED_BY);
    let trailers_json = downcast_string_opt(batch, sc::COL_TRAILERS_JSON);
    // body_segment_id was added after v0.1.0; older Parquet files
    // lack it. Default to segment 0 for backward compat.
    let body_segment_id = downcast_u32_opt(batch, sc::COL_BODY_SEGMENT_ID);
    let body_offset = downcast_u64(batch, sc::COL_BODY_OFFSET)?;
    let body_length = downcast_u64(batch, sc::COL_BODY_LENGTH)?;
    let body_sha256 = downcast_string(batch, sc::COL_BODY_SHA256)?;
    let schema_version = downcast_u32(batch, sc::COL_SCHEMA_VERSION)?;

    let mut rows = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        rows.push(MessageRow {
            message_id: message_id.value(i).to_owned(),
            list: list.value(i).to_owned(),
            shard: shard.value(i).to_owned(),
            commit_oid: commit_oid.value(i).to_owned(),
            from_addr: opt_string(&from_addr, i),
            from_name: opt_string(&from_name, i),
            subject_raw: opt_string(&subject_raw, i),
            subject_normalized: opt_string(&subject_normalized, i),
            subject_tags: list_strings(&subject_tags, i),
            date_unix_ns: date.as_ref().filter(|a| !a.is_null(i)).map(|a| a.value(i)),
            in_reply_to: opt_string(&in_reply_to, i),
            references: list_strings(&references, i),
            tid: tid.as_ref().and_then(|a| {
                if a.is_null(i) {
                    None
                } else {
                    Some(a.value(i).to_owned())
                }
            }),
            series_version: series_version.value(i),
            series_index: if series_index.is_null(i) {
                None
            } else {
                Some(series_index.value(i))
            },
            series_total: if series_total.is_null(i) {
                None
            } else {
                Some(series_total.value(i))
            },
            is_cover_letter: is_cover_letter.value(i),
            has_patch: has_patch.value(i),
            touched_files: list_strings(&touched_files, i),
            touched_functions: list_strings(&touched_functions, i),
            files_changed: if files_changed.is_null(i) {
                None
            } else {
                Some(files_changed.value(i))
            },
            insertions: if insertions.is_null(i) {
                None
            } else {
                Some(insertions.value(i))
            },
            deletions: if deletions.is_null(i) {
                None
            } else {
                Some(deletions.value(i))
            },
            signed_off_by: list_strings(&signed_off_by, i),
            reviewed_by: list_strings(&reviewed_by, i),
            acked_by: list_strings(&acked_by, i),
            tested_by: list_strings(&tested_by, i),
            co_developed_by: list_strings(&co_developed_by, i),
            reported_by: list_strings(&reported_by, i),
            fixes: list_strings(&fixes, i),
            link: list_strings(&link_trailers, i),
            closes: list_strings(&closes, i),
            cc_stable: list_strings(&cc_stable, i),
            suggested_by: suggested_by
                .as_ref()
                .map(|a| list_strings(a, i))
                .unwrap_or_default(),
            helped_by: helped_by
                .as_ref()
                .map(|a| list_strings(a, i))
                .unwrap_or_default(),
            assisted_by: assisted_by
                .as_ref()
                .map(|a| list_strings(a, i))
                .unwrap_or_default(),
            trailers_json: trailers_json.as_ref().and_then(|a| {
                if a.is_null(i) {
                    None
                } else {
                    Some(a.value(i).to_owned())
                }
            }),
            body_segment_id: body_segment_id.as_ref().map(|a| a.value(i)).unwrap_or(0),
            body_offset: body_offset.value(i),
            body_length: body_length.value(i),
            body_sha256: body_sha256.value(i).to_owned(),
            schema_version: schema_version.value(i),
        });
    }
    Ok(rows)
}

fn downcast_string<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|e| Error::State(format!("missing column {name}: {e}")))?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| Error::State(format!("column {name} not utf8")))
}

/// Returns `None` when the column doesn't exist (backward compat for
/// columns added after v0.1.0).
fn downcast_string_opt(batch: &RecordBatch, name: &str) -> Option<StringArray> {
    let idx = batch.schema().index_of(name).ok()?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .cloned()
}

/// Returns `None` when the column doesn't exist (backward compat).
fn downcast_list_opt(batch: &RecordBatch, name: &str) -> Option<ListArray> {
    let idx = batch.schema().index_of(name).ok()?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<ListArray>()
        .cloned()
}

fn downcast_bool(batch: &RecordBatch, name: &str) -> Result<BooleanArray> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|e| Error::State(format!("missing column {name}: {e}")))?;
    Ok(batch
        .column(idx)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| Error::State(format!("column {name} not bool")))?
        .clone())
}

fn downcast_u32(batch: &RecordBatch, name: &str) -> Result<UInt32Array> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|e| Error::State(format!("missing column {name}: {e}")))?;
    Ok(batch
        .column(idx)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .ok_or_else(|| Error::State(format!("column {name} not u32")))?
        .clone())
}

/// Like `downcast_u32` but returns `None` when the column doesn't
/// exist (backward compat for columns added after v0.1.0).
fn downcast_u32_opt(batch: &RecordBatch, name: &str) -> Option<UInt32Array> {
    let idx = batch.schema().index_of(name).ok()?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .cloned()
}

fn downcast_u64(batch: &RecordBatch, name: &str) -> Result<UInt64Array> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|e| Error::State(format!("missing column {name}: {e}")))?;
    Ok(batch
        .column(idx)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or_else(|| Error::State(format!("column {name} not u64")))?
        .clone())
}

fn downcast_list(batch: &RecordBatch, name: &str) -> Result<ListArray> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|e| Error::State(format!("missing column {name}: {e}")))?;
    Ok(batch
        .column(idx)
        .as_any()
        .downcast_ref::<ListArray>()
        .ok_or_else(|| Error::State(format!("column {name} not list")))?
        .clone())
}

fn opt_string(arr: &StringArray, i: usize) -> Option<String> {
    if arr.is_null(i) {
        None
    } else {
        Some(arr.value(i).to_owned())
    }
}

fn list_strings(list: &ListArray, i: usize) -> Vec<String> {
    if list.is_null(i) {
        return Vec::new();
    }
    let values = list.value(i);
    let Some(s) = values.as_any().downcast_ref::<StringArray>() else {
        return Vec::new();
    };
    (0..s.len())
        .filter(|j| !s.is_null(*j))
        .map(|j| s.value(j).to_owned())
        .collect()
}

// Pedantic unused import silencer; kept for future needs.
#[allow(dead_code)]
fn _int64_unused(_: Int64Array) {}

#[allow(dead_code)]
fn _map_unused(_: HashMap<String, String>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::ingest_shard;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    fn make_synthetic_shard(shard_dir: &Path, messages: &[&[u8]]) {
        let run = |args: &[&str], cwd: &Path| {
            let out = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "tester")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "tester")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let work = tempdir().unwrap();
        run(&["init", "-q", "-b", "master", "."], work.path());
        for (i, msg) in messages.iter().enumerate() {
            fs::write(work.path().join("m"), msg).unwrap();
            run(&["add", "m"], work.path());
            run(&["commit", "-q", "-m", &format!("m{i}")], work.path());
        }
        if shard_dir.exists() {
            fs::remove_dir_all(shard_dir).unwrap();
        }
        run(
            &[
                "clone",
                "--bare",
                "-q",
                work.path().to_str().unwrap(),
                shard_dir.to_str().unwrap(),
            ],
            Path::new("/"),
        );
    }

    fn sample_corpus() -> Vec<Vec<u8>> {
        vec![
            b"From: Alice <alice@example.com>\r\n\
Subject: [PATCH v3 1/2] ksmbd: tighten ACL bounds\r\n\
Date: Mon, 14 Apr 2026 12:00:00 +0000\r\n\
Message-ID: <m1@x>\r\n\
\r\n\
Prose here.\r\n\
Fixes: deadbeef01234567 (\"ksmbd: initial ACL handling\")\r\n\
Reviewed-by: Carol <carol@example.com>\r\n\
Signed-off-by: Alice <alice@example.com>\r\n\
---\r\n\
diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c\r\n\
--- a/fs/smb/server/smbacl.c\r\n\
+++ b/fs/smb/server/smbacl.c\r\n\
@@ -1,1 +1,2 @@ int smb_check_perm_dacl(struct ksmbd_conn *c)\r\n\
 a\r\n\
+b\r\n"
                .to_vec(),
            b"From: Alice <alice@example.com>\r\n\
Subject: [PATCH v3 2/2] ksmbd: follow-up\r\n\
Date: Mon, 14 Apr 2026 12:05:00 +0000\r\n\
Message-ID: <m2@x>\r\n\
In-Reply-To: <m1@x>\r\n\
\r\n\
More prose.\r\n\
Signed-off-by: Alice <alice@example.com>\r\n\
---\r\n\
diff --git a/fs/smb/server/smb2pdu.c b/fs/smb/server/smb2pdu.c\r\n\
--- a/fs/smb/server/smb2pdu.c\r\n\
+++ b/fs/smb/server/smb2pdu.c\r\n\
@@ -1,1 +1,2 @@ int smb2_create(struct ksmbd_conn *c)\r\n\
 a\r\n\
+b\r\n"
                .to_vec(),
        ]
    }

    fn ingest_sample(data: &Path) {
        let shard = tempdir().unwrap();
        let shard_dir = shard.path().join("0.git");
        let msgs = sample_corpus();
        let msg_refs: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
        make_synthetic_shard(&shard_dir, &msg_refs);
        ingest_shard(data, &shard_dir, "linux-cifs", "0", "run-0001").unwrap();
    }

    #[test]
    fn fetch_message_roundtrip() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let m = r.fetch_message("m1@x").unwrap().unwrap();
        assert_eq!(m.message_id, "m1@x");
        assert_eq!(m.list, "linux-cifs");
        assert!(m.has_patch);
        assert_eq!(m.series_version, 3);
        assert_eq!(m.series_index, Some(1));
        assert!(
            m.reviewed_by
                .iter()
                .any(|s| s.contains("carol@example.com"))
        );
        assert!(
            m.touched_files
                .iter()
                .any(|s| s == "fs/smb/server/smbacl.c")
        );
        assert!(
            m.touched_functions
                .iter()
                .any(|s| s == "smb_check_perm_dacl")
        );
    }

    #[test]
    fn activity_by_file() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .activity(Some("fs/smb/server/smbacl.c"), None, None, None, 50)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m1@x");

        let none = r
            .activity(Some("no/such/file.c"), None, None, None, 50)
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn activity_by_function() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .activity(None, Some("smb2_create"), None, None, 50)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m2@x");
    }

    #[test]
    fn series_timeline_groups_versions() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        // Both messages are from the same series "ksmbd: tighten ACL bounds"
        // only if subject_normalized matches. They don't here (follow-up has
        // a different subject); so series_timeline("m1@x") should return
        // exactly m1@x.
        let rows = r.series_timeline("m1@x").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m1@x");
    }

    #[test]
    fn expand_citation_sha_hit() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r.expand_citation("deadbeef01234567", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m1@x");
    }

    #[test]
    fn expand_citation_mid_hit() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r.expand_citation("<m2@x>", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m2@x");
    }

    #[test]
    fn eq_by_from_addr_returns_only_matching_rows() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .eq(EqField::FromAddr, "alice@example.com", None, None, 50)
            .unwrap();
        let mids: std::collections::HashSet<&str> =
            rows.iter().map(|r| r.message_id.as_str()).collect();
        assert!(mids.contains("m1@x"));
        assert!(mids.contains("m2@x"));
    }

    #[test]
    fn eq_on_touched_files_set_membership() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .eq(
                EqField::TouchedFile,
                "fs/smb/server/smb2pdu.c",
                None,
                None,
                50,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m2@x");
    }

    #[test]
    fn in_list_unions_multiple_values() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .in_list(
                EqField::TouchedFile,
                &[
                    "fs/smb/server/smbacl.c".to_owned(),
                    "fs/smb/server/smb2pdu.c".to_owned(),
                ],
                None,
                None,
                50,
            )
            .unwrap();
        let mids: std::collections::HashSet<&str> =
            rows.iter().map(|r| r.message_id.as_str()).collect();
        assert_eq!(mids, ["m1@x", "m2@x"].into_iter().collect());
    }

    #[test]
    fn count_returns_summary_stats() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let s = r
            .count(EqField::FromAddr, "alice@example.com", None, None)
            .unwrap();
        assert_eq!(s.count, 2);
        assert_eq!(s.distinct_authors, 1);
        assert!(s.earliest_unix_ns.is_some());
        assert!(s.latest_unix_ns.is_some());
    }

    #[test]
    fn substr_subject_case_insensitive() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r.substr_subject("ksmbd", None, None, 50).unwrap();
        assert!(rows.iter().any(|r| r.message_id == "m1@x"));
        assert!(rows.iter().any(|r| r.message_id == "m2@x"));
        // Uppercase needle still hits.
        let rows = r.substr_subject("KSMBD", None, None, 50).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn substr_trailers_finds_via_fixes_substring() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .substr_trailers("fixes", "deadbeef", None, None, 50)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m1@x");

        // Unknown trailer name returns empty without erroring.
        assert!(
            r.substr_trailers("nonsense", "x", None, None, 5)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn regex_subject_anchored() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .regex(
                RegexField::Subject,
                r"\[PATCH v3 1/2\]",
                false,
                None,
                None,
                10,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m1@x");
    }

    #[test]
    fn regex_rejects_unanchored_dotstar_when_required() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let err = r
            .regex(RegexField::Subject, ".*ksmbd.*", true, None, None, 10)
            .unwrap_err();
        match err {
            crate::error::Error::RegexComplexity(_) => {}
            other => panic!("wrong err: {other:?}"),
        }
    }

    #[test]
    fn regex_patch_field_confirms_via_body() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let rows = r
            .regex(
                RegexField::Patch,
                r"smb_check_perm_dacl\(",
                false,
                None,
                None,
                10,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message_id, "m1@x");
    }

    #[test]
    fn diff_patch_mode_returns_both_views() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        let res = r.diff("m1@x", "m2@x", DiffMode::Patch).unwrap();
        assert_eq!(res.row_a.message_id, "m1@x");
        assert_eq!(res.row_b.message_id, "m2@x");
        assert!(res.text_a.starts_with("diff --git "));
        assert!(res.text_b.starts_with("diff --git "));
    }

    #[test]
    fn patch_search_finds_function_name() {
        let d = tempdir().unwrap();
        ingest_sample(d.path());
        let r = Reader::new(d.path());
        // m1 contains `smb_check_perm_dacl` in its hunk header; m2
        // contains `smb2_create`. Both are in patch bodies, so both
        // should be findable.
        let m1 = r.patch_search("smb_check_perm_dacl", None, 10).unwrap();
        assert_eq!(m1.len(), 1);
        assert_eq!(m1[0].message_id, "m1@x");

        let m2 = r.patch_search("smb2_create", None, 10).unwrap();
        assert_eq!(m2.len(), 1);
        assert_eq!(m2[0].message_id, "m2@x");

        let none = r.patch_search("never_appears_anywhere", None, 10).unwrap();
        assert!(none.is_empty());
    }
}
