//! `kernel-lore-build-over` — build (or rebuild) the SQLite
//! `over.db` metadata tier from existing Parquet metadata in a
//! single streaming pass.
//!
//! See `docs/plans/2026-04-17-overdb-metadata-tier.md` (Phase 2).
//!
//! Usage:
//!     KLMCP_DATA_DIR=/var/klmcp/data \
//!     kernel-lore-build-over
//!
//! Or with explicit args:
//!     kernel-lore-build-over --data-dir /var/klmcp/data \
//!                            --output /var/klmcp/data/over.db \
//!                            --from-list linux-cifs \
//!                            --batch-size 10000
//!
//! Build strategy:
//!   1. Open output as `<output>.tmp.<run_id>` via
//!      `OverDb::open_for_bulk_load` (table only — no indexes).
//!   2. Stream every row through `Reader::scan_streaming` (NOT
//!      `scan_all`, which materializes 29M rows in a Vec). Each row
//!      becomes one `OverRow` whose `ddd` payload carries the display
//!      fields not promoted to indexed columns.
//!   3. Flush in batches of `--batch-size` (default 10k) inside one
//!      transaction per batch.
//!   4. After all inserts: `create_indexes()`, `PRAGMA optimize`,
//!      `VACUUM`, set `meta.built_at`.
//!   5. On success, atomic rename `.tmp.<run_id>` -> `<output>`.
//!      On error, leave the tempfile in place so operators can
//!      inspect / resume.
//!
//! Logs (JSON via tracing) report row count, elapsed wall-clock,
//! throughput (MB/s of final db size), and final db file size.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};

use _core::{DddPayload, MessageRow, OverDb, OverRow, Reader};

#[derive(Default)]
struct Args {
    data_dir: Option<PathBuf>,
    output: Option<PathBuf>,
    from_list: Option<String>,
    batch_size: usize,
}

const DEFAULT_BATCH_SIZE: usize = 10_000;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .init();

    let args = parse_args()?;
    let data_dir = args
        .data_dir
        .or_else(|| std::env::var_os("KLMCP_DATA_DIR").map(PathBuf::from))
        .context("--data-dir or KLMCP_DATA_DIR required")?;

    if !data_dir.is_dir() {
        return Err(anyhow!(
            "--data-dir {} does not exist or is not a directory",
            data_dir.display()
        ));
    }

    let output = args
        .output
        .unwrap_or_else(|| data_dir.join("over.db"));
    let batch_size = if args.batch_size == 0 {
        DEFAULT_BATCH_SIZE
    } else {
        args.batch_size
    };

    let run_id = format!(
        "run-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );

    // Tempfile sits in the same directory as the final output so the
    // atomic rename stays on one filesystem.
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("create_dir_all {}", parent.display()))?;
    let tmp_filename = match output.file_name() {
        Some(n) => format!("{}.tmp.{}", n.to_string_lossy(), run_id),
        None => format!("over.db.tmp.{run_id}"),
    };
    let tmp_path = parent.join(&tmp_filename);

    if tmp_path.exists() {
        return Err(anyhow!(
            "temp file {} already exists; refusing to overwrite",
            tmp_path.display()
        ));
    }

    tracing::info!(
        data_dir = %data_dir.display(),
        output = %output.display(),
        tmp_path = %tmp_path.display(),
        from_list = args.from_list.as_deref().unwrap_or("<all>"),
        batch_size,
        run_id,
        "build_over starting"
    );

    let start = Instant::now();

    // Run the build in a scoped block so we can act on errors before
    // returning (we deliberately leave the tempfile on failure for
    // post-mortem; success path moves it).
    let result = run_build(
        &data_dir,
        &tmp_path,
        args.from_list.as_deref(),
        batch_size,
    );

    let row_count = match result {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(
                error = %e,
                tmp_path = %tmp_path.display(),
                "build_over failed; leaving tempfile in place for inspection"
            );
            return Err(e);
        }
    };

    // Atomic publish.
    if let Err(e) = std::fs::rename(&tmp_path, &output) {
        return Err(anyhow!(
            "atomic rename {} -> {} failed: {e}",
            tmp_path.display(),
            output.display()
        ));
    }

    let elapsed = start.elapsed();
    let db_bytes = std::fs::metadata(&output)
        .map(|m| m.len())
        .unwrap_or(0);
    let secs = elapsed.as_secs_f64().max(1e-6);
    let mb_per_sec = (db_bytes as f64 / 1_048_576.0) / secs;

    tracing::info!(
        rows = row_count,
        elapsed_secs = secs,
        db_bytes,
        db_mb = db_bytes as f64 / 1_048_576.0,
        mb_per_sec,
        output = %output.display(),
        "build_over complete"
    );

    Ok(())
}

/// Streaming build inside the tempfile. Returns the row count on
/// success. On error, leaves the tempfile alone — the caller logs +
/// surfaces the failure.
fn run_build(
    data_dir: &std::path::Path,
    tmp_path: &std::path::Path,
    from_list: Option<&str>,
    batch_size: usize,
) -> Result<u64> {
    let reader = Reader::new(data_dir);

    let mut over =
        OverDb::open_for_bulk_load(tmp_path).context("open_for_bulk_load on tempfile")?;

    let mut buffer: Vec<OverRow> = Vec::with_capacity(batch_size);
    let mut total: u64 = 0;
    let mut last_log_at = Instant::now();
    // We need to flush from inside the visit closure. The closure
    // can't return a Result, so we capture errors here and propagate
    // after the scan finishes. Once an error is captured, the closure
    // bails out by returning false (which terminates the scan).
    let mut flush_err: Option<anyhow::Error> = None;

    reader
        .scan_streaming(from_list, |mr| {
            buffer.push(message_row_to_over_row(mr));
            if buffer.len() >= batch_size {
                if let Err(e) = over.insert_batch(&buffer) {
                    flush_err = Some(anyhow::Error::new(e).context("insert_batch flush"));
                    return false;
                }
                total += buffer.len() as u64;
                buffer.clear();
                if last_log_at.elapsed().as_secs() >= 10 {
                    tracing::info!(rows_so_far = total, "build_over progress");
                    last_log_at = Instant::now();
                }
            }
            true
        })
        .context("scan_streaming")?;

    if let Some(e) = flush_err {
        return Err(e);
    }

    // Tail flush.
    if !buffer.is_empty() {
        over.insert_batch(&buffer).context("insert_batch tail")?;
        total += buffer.len() as u64;
        buffer.clear();
    }

    tracing::info!(rows = total, "all inserts done; building indexes");
    let idx_start = Instant::now();
    over.create_indexes().context("create_indexes")?;
    tracing::info!(
        elapsed_secs = idx_start.elapsed().as_secs_f64(),
        "indexes built"
    );

    let fin_start = Instant::now();
    over.finalize().context("finalize (optimize + vacuum)")?;
    tracing::info!(
        elapsed_secs = fin_start.elapsed().as_secs_f64(),
        "finalize complete"
    );

    let built_at = current_iso8601_utc();
    over.set_built_at(&built_at).context("set_built_at")?;
    let counted = over.row_count().context("row_count")?;
    if counted != total {
        // Not fatal but worth shouting about — likely indicates an
        // INSERT OR REPLACE collision (which shouldn't happen given
        // upstream dedup, but is technically possible if the input
        // Parquet has two rows with the same (message_id, list)).
        tracing::warn!(
            inserted = total,
            stored = counted,
            "row count drift — INSERT OR REPLACE collapsed duplicates"
        );
    }

    // Drop closes the connection so the rename below can succeed on
    // platforms that hold an exclusive lock (Windows). Linux is more
    // permissive but explicit drop documents intent.
    drop(over);

    Ok(counted)
}

/// Convert a `MessageRow` from the Parquet reader into the split
/// (indexed columns + ddd payload) shape `OverRow` expects.
///
/// Indexed columns mirror the most common query predicates (mid, list,
/// from, since, in_reply_to, tid). Everything else lives in `ddd`.
/// The `from_addr` indexed column is lowercased here defensively;
/// `OverDb::insert_batch` lowercases again, but doing it at the
/// callsite makes the intent obvious in code review.
fn message_row_to_over_row(mr: MessageRow) -> OverRow {
    let from_addr_lc = mr.from_addr.as_deref().map(str::to_ascii_lowercase);
    let from_addr_original = mr.from_addr.clone();
    let MessageRow {
        message_id,
        list,
        shard,
        commit_oid,
        from_addr: _,
        from_name,
        subject_raw,
        subject_normalized,
        subject_tags,
        date_unix_ns,
        in_reply_to,
        references,
        tid,
        series_version,
        series_index,
        series_total,
        is_cover_letter,
        has_patch,
        touched_files,
        touched_functions,
        files_changed,
        insertions,
        deletions,
        signed_off_by,
        reviewed_by,
        acked_by,
        tested_by,
        co_developed_by,
        reported_by,
        fixes,
        link,
        closes,
        cc_stable,
        suggested_by,
        helped_by,
        assisted_by,
        trailers_json,
        body_segment_id,
        body_offset,
        body_length,
        body_sha256,
        schema_version: _,
    } = mr;

    let commit_oid_opt = if commit_oid.is_empty() {
        None
    } else {
        Some(commit_oid)
    };

    OverRow {
        message_id,
        list,
        from_addr: from_addr_lc,
        date_unix_ns,
        in_reply_to,
        tid,
        body_segment_id: body_segment_id as i64,
        body_offset: body_offset as i64,
        body_length: body_length as i64,
        body_sha256,
        has_patch,
        is_cover_letter,
        series_version: Some(series_version as i64),
        series_index: series_index.map(|v| v as i64),
        series_total: series_total.map(|v| v as i64),
        files_changed: files_changed.map(|v| v as i64),
        insertions: insertions.map(|v| v as i64),
        deletions: deletions.map(|v| v as i64),
        commit_oid: commit_oid_opt,
        ddd: DddPayload {
            subject_raw,
            subject_normalized,
            subject_tags,
            references,
            touched_files,
            touched_functions,
            signed_off_by,
            reviewed_by,
            acked_by,
            tested_by,
            co_developed_by,
            reported_by,
            suggested_by,
            helped_by,
            assisted_by,
            fixes,
            link,
            closes,
            cc_stable,
            trailers_json,
            from_name,
            from_addr_original_case: from_addr_original,
            shard: if shard.is_empty() { None } else { Some(shard) },
        },
    }
}

/// Hand-rolled ISO 8601 UTC timestamp ("YYYY-MM-DDTHH:MM:SSZ"). We
/// already depend on `time` for parsing in the schema layer; using it
/// here keeps the binary's dep surface unchanged.
fn current_iso8601_utc() -> String {
    let now = time::OffsetDateTime::now_utc();
    // Format manually: avoids pulling in the `formatting` feature.
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
    )
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        batch_size: DEFAULT_BATCH_SIZE,
        ..Default::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data-dir" => {
                args.data_dir = Some(
                    it.next()
                        .map(PathBuf::from)
                        .ok_or_else(|| anyhow!("--data-dir requires a value"))?,
                )
            }
            "--output" => {
                args.output = Some(
                    it.next()
                        .map(PathBuf::from)
                        .ok_or_else(|| anyhow!("--output requires a value"))?,
                )
            }
            "--from-list" => {
                args.from_list = Some(
                    it.next()
                        .ok_or_else(|| anyhow!("--from-list requires a value"))?,
                )
            }
            "--batch-size" => {
                let raw = it
                    .next()
                    .ok_or_else(|| anyhow!("--batch-size requires a value"))?;
                args.batch_size = raw
                    .parse()
                    .with_context(|| format!("--batch-size: {raw} not a positive integer"))?;
                if args.batch_size == 0 {
                    return Err(anyhow!("--batch-size must be > 0"));
                }
            }
            "--help" | "-h" => {
                println!(
                    "kernel-lore-build-over\n\
                     \n\
                     Build <data_dir>/over.db from existing Parquet metadata in a\n\
                     single streaming pass. Atomic build via tempfile + rename.\n\
                     \n\
                     --data-dir PATH        (or $KLMCP_DATA_DIR)\n\
                     --output PATH          default: <data_dir>/over.db\n\
                     --from-list NAME       optional: restrict to one list (testing)\n\
                     --batch-size N         default: {DEFAULT_BATCH_SIZE}\n"
                );
                std::process::exit(0);
            }
            other => {
                return Err(anyhow!("unknown arg: {other}"));
            }
        }
    }
    Ok(args)
}
