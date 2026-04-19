//! BM25 tier — tantivy index over prose (body minus patch) + subject.
//!
//! See `docs/indexing/bm25-tier.md` for the full schema and
//! `docs/indexing/tokenizer-spec.md` for the analyzer proscriptions.
//!
//! Non-negotiable proscriptions (enforced here):
//!   * NO stemming (tantivy's `stemmer` feature is off in Cargo.toml).
//!   * NO stopwords, asciifolding, typo tolerance.
//!   * Positions OFF (`IndexRecordOption::WithFreqs`). Phrase queries
//!     on body_prose are REJECTED by the router, not silently
//!     degraded.
//!   * Register `kernel_prose` + `raw_lc` analyzers after every
//!     Index::open or create_in_dir — tokenizer names are NOT
//!     persisted in `meta.json`.
//!
//! v0.5 scope:
//!   * single index under `<data_dir>/bm25/` shared across lists.
//!   * Writer single-instance via `state::acquire_writer_lock` held
//!     by the ingest process.
//!   * Readers open on demand; ReloadPolicy::Manual + generation-file
//!     stat is wired when the router lands (follow-up).
//!
//! Deliberately NOT in scope in this commit:
//!   * tokenizer-fingerprint sidecar (Phase 4b — needs a wire
//!     format; for now tokenizer names are constant and the on-disk
//!     index rebuilds from the compressed store if it ever drifts).
//!   * phrase-query rejection hook (lives in the router, not here).

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use tantivy::Index;
use tantivy::IndexWriter;
use tantivy::TantivyDocument;
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value};
use tantivy::tokenizer::{LowerCaser, RawTokenizer, TextAnalyzer, Token, TokenStream, Tokenizer};

use crate::error::{Error, Result};

pub const KERNEL_PROSE: &str = "kernel_prose";
pub const RAW_LC: &str = "raw_lc";

// --------------------------------------------------------------------
// Custom tokenizer: scan [A-Za-z0-9_]+ runs.
//
// Identifiers like `vector_mmsg_rx`, `SMB2_CREATE`, `__skb_unlink`
// arrive whole; subtokens (snake_case / camelCase splits) live on the
// TODO list for Phase 4b. v0.5 ships the whole-identifier token plus
// LowerCaser; it's enough to drive lore_search over prose without
// mangling kernel identifiers with a stemmer.

#[derive(Clone, Default)]
struct KernelIdentSplitter {
    token: Token,
}

struct KernelIdentStream<'a> {
    text: &'a str,
    cursor: usize,
    token: &'a mut Token,
}

impl Tokenizer for KernelIdentSplitter {
    type TokenStream<'a> = KernelIdentStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        self.token.reset();
        KernelIdentStream {
            text,
            cursor: 0,
            token: &mut self.token,
        }
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl<'a> TokenStream for KernelIdentStream<'a> {
    fn advance(&mut self) -> bool {
        let bytes = self.text.as_bytes();
        while self.cursor < bytes.len() && !is_ident_byte(bytes[self.cursor]) {
            self.cursor += 1;
        }
        if self.cursor >= bytes.len() {
            return false;
        }
        let start = self.cursor;
        while self.cursor < bytes.len() && is_ident_byte(bytes[self.cursor]) {
            self.cursor += 1;
        }
        self.token.text.clear();
        self.token.text.push_str(&self.text[start..self.cursor]);
        self.token.offset_from = start;
        self.token.offset_to = self.cursor;
        self.token.position = self.token.position.wrapping_add(1);
        self.token.position_length = 1;
        true
    }

    fn token(&self) -> &Token {
        self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        self.token
    }
}

fn make_kernel_prose() -> TextAnalyzer {
    TextAnalyzer::builder(KernelIdentSplitter::default())
        .filter(LowerCaser)
        .build()
}

fn make_raw_lc() -> TextAnalyzer {
    TextAnalyzer::builder(RawTokenizer::default())
        .filter(LowerCaser)
        .build()
}

fn register_analyzers(index: &Index) {
    let mgr = index.tokenizers();
    mgr.register(KERNEL_PROSE, make_kernel_prose());
    mgr.register(RAW_LC, make_raw_lc());
}

// --------------------------------------------------------------------
// Schema

#[derive(Debug, Clone, Copy)]
pub struct BmSchema {
    pub message_id: Field,
    pub list: Field,
    pub subject_normalized: Field,
    pub body_prose: Field,
}

fn build_schema() -> (Schema, BmSchema) {
    let mut b = Schema::builder();

    // message_id: raw (case-preserving), indexed + stored.
    let mid_opts = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("raw")
                .set_index_option(IndexRecordOption::Basic),
        )
        .set_stored();

    let raw_lc_opts = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(RAW_LC)
            .set_index_option(IndexRecordOption::Basic),
    );

    let prose_opts = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(KERNEL_PROSE)
            .set_index_option(IndexRecordOption::WithFreqs), // NO positions
    );

    let message_id = b.add_text_field("message_id", mid_opts);
    let list = b.add_text_field("list", raw_lc_opts);
    let subject_normalized = b.add_text_field("subject_normalized", prose_opts.clone());
    let body_prose = b.add_text_field("body_prose", prose_opts);

    let schema = b.build();
    let bm = BmSchema {
        message_id,
        list,
        subject_normalized,
        body_prose,
    };
    (schema, bm)
}

// --------------------------------------------------------------------
// Writer: one-shot builder used by the ingest pipeline.

pub struct BmWriter {
    _index: Index,
    writer: IndexWriter,
    schema: BmSchema,
}

impl BmWriter {
    /// Open (or create) the single BM25 index under
    /// `<data_dir>/bm25/`. Requires the writer flock held externally.
    pub fn open(data_dir: &Path) -> Result<Self> {
        let dir = data_dir.join("bm25");
        std::fs::create_dir_all(&dir)?;
        let mmap = MmapDirectory::open(&dir)
            .map_err(|e| Error::State(format!("mmap {}: {e}", dir.display())))?;

        let (schema, bm) = build_schema();
        let index = Index::open_or_create(mmap, schema)?;
        register_analyzers(&index);

        // 128 MiB is plenty for the per-shard batch sizes we see in
        // tests; the CLI binary can override via env later.
        let writer = index.writer(128 * 1024 * 1024)?;
        Ok(Self {
            _index: index,
            writer,
            schema: bm,
        })
    }

    pub fn schema(&self) -> BmSchema {
        self.schema
    }

    pub fn add(
        &mut self,
        message_id: &str,
        list: &str,
        subject_normalized: Option<&str>,
        body_prose: &str,
    ) -> Result<()> {
        let mut doc = TantivyDocument::new();
        doc.add_text(self.schema.message_id, message_id);
        doc.add_text(self.schema.list, list);
        if let Some(s) = subject_normalized {
            doc.add_text(self.schema.subject_normalized, s);
        }
        doc.add_text(self.schema.body_prose, body_prose);
        self.writer.add_document(doc)?;
        Ok(())
    }

    pub fn commit(&mut self) -> Result<u64> {
        let opstamp = self.writer.commit()?;
        Ok(opstamp)
    }
}

// --------------------------------------------------------------------
// Reader / query

pub struct BmReader {
    index: Index,
    schema: BmSchema,
    /// Cached `IndexReader`. Construction of this field is expensive
    /// (opens all segment readers, mmap-registers files); `reload()`
    /// on an existing reader is cheap (reads only new segment meta).
    /// Before this field existed, `search_filtered` called
    /// `reader_builder().try_into()` on every query — paying the
    /// full open cost on the hot path.
    reader: tantivy::IndexReader,
    /// Last generation we reloaded at. `maybe_reload` compares against
    /// the current `state/generation` counter and only calls
    /// `reader.reload()` when it advanced. Wrapped in `AtomicU64` so
    /// a `&self` search method can bump it after a successful reload
    /// without needing interior mutability on the whole struct.
    last_reloaded_generation: std::sync::atomic::AtomicU64,
}

impl BmReader {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let dir = data_dir.join("bm25");
        std::fs::create_dir_all(&dir)?;
        let mmap = MmapDirectory::open(&dir)
            .map_err(|e| Error::State(format!("mmap {}: {e}", dir.display())))?;
        let (schema, bm) = build_schema();
        let index = Index::open_or_create(mmap, schema)?;
        register_analyzers(&index);
        let reader = index
            .reader_builder()
            .reload_policy(tantivy::ReloadPolicy::Manual)
            .try_into()?;
        Ok(Self {
            index,
            schema: bm,
            reader,
            last_reloaded_generation: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn schema(&self) -> BmSchema {
        self.schema
    }

    /// Reload the IndexReader iff `current_generation` is strictly
    /// greater than the one we last reloaded at. `generation` is
    /// `State::generation()` — a stat on `state/generation`, cheap.
    /// Callers (the Reader's prose_search path) check the generation
    /// file once per query and pass it here.
    pub fn maybe_reload(&self, current_generation: u64) -> Result<()> {
        use std::sync::atomic::Ordering;
        let seen = self.last_reloaded_generation.load(Ordering::Acquire);
        if current_generation > seen {
            self.reader.reload()?;
            // Use compare-exchange to avoid a reader racing to an older
            // generation; last-writer-wins is fine if both are up-to-date.
            let _ = self.last_reloaded_generation.compare_exchange(
                seen,
                current_generation,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        Ok(())
    }

    /// Run a free-text query against `body_prose` + `subject_normalized`
    /// and return the top-`limit` message_ids with their BM25 scores.
    ///
    /// Phrase queries are rejected here with a clear error — positions
    /// are off, so a phrase query would be a lie.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<(String, f32)>> {
        self.search_filtered(query, None, limit)
    }

    /// Like `search` but optionally filter to a specific list at the
    /// tantivy query level. This eliminates false negatives from
    /// post-filtering: when `list_filter` is set, tantivy only
    /// scores documents from that list, so the top-N are guaranteed
    /// to be from the requested list.
    pub fn search_filtered(
        &self,
        query: &str,
        list_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        if query.contains('"') {
            return Err(Error::QueryParse(
                "phrase queries on body_prose are not supported in v0.5: this \
                 field is indexed WithFreqs (no positions). Use lore_patch_search \
                 for literal substrings in patch content, or split the phrase \
                 into AND-ed terms."
                    .to_owned(),
            ));
        }

        // Use the cached reader. Callers responsible for `maybe_reload`
        // before hitting us — that's a Reader-layer concern (it owns
        // the generation file stat). If a caller skips it, worst case
        // is serving the last-reloaded snapshot of the index, which is
        // still a valid query answer.
        let searcher = self.reader.searcher();

        let parser = QueryParser::for_index(
            &self.index,
            vec![self.schema.body_prose, self.schema.subject_normalized],
        );
        let text_query = parser
            .parse_query(query)
            .map_err(|e| Error::QueryParse(format!("parse {query:?}: {e}")))?;

        // When a list filter is present, combine the text query with
        // a TermQuery on the `list` field. This filters at the
        // tantivy level so the top-N results are all from the right
        // list — no post-filter starvation.
        let q: Box<dyn tantivy::query::Query> = if let Some(list_name) = list_filter {
            use tantivy::query::{BooleanQuery, Occur, TermQuery};
            use tantivy::schema::IndexRecordOption as IRO;
            let list_term =
                tantivy::Term::from_field_text(self.schema.list, &list_name.to_lowercase());
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, text_query),
                (Occur::Must, Box::new(TermQuery::new(list_term, IRO::Basic))),
            ]))
        } else {
            text_query
        };

        // tantivy 0.26: TopDocs::with_limit(N) is a builder; chain
        // `.order_by_score()` to get an `impl Collector`. The result
        // is `Vec<(Score, DocAddress)>` just like earlier versions.
        let top: Vec<(tantivy::Score, tantivy::DocAddress)> =
            searcher.search(&*q, &TopDocs::with_limit(limit).order_by_score())?;

        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let Some(mid_val) = doc.get_first(self.schema.message_id) else {
                continue;
            };
            let mid = mid_val.as_str().unwrap_or_default().to_owned();
            if !mid.is_empty() {
                out.push((mid, score));
            }
        }
        Ok(out)
    }
}

pub fn bm25_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("bm25")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn ingest_three(dir: &Path) {
        let mut w = BmWriter::open(dir).unwrap();
        w.add(
            "<m1@x>",
            "linux-cifs",
            Some("ksmbd: tighten ACL bounds"),
            "We tighten the ACL check in smb_check_perm_dacl to reject oversized ACEs.",
        )
        .unwrap();
        w.add(
            "<m2@x>",
            "linux-cifs",
            Some("ksmbd: follow-up in smb2_create"),
            "Also fix a smb2_create edge case near ksmbd_conn handling.",
        )
        .unwrap();
        w.add(
            "<m3@x>",
            "linux-nfs",
            Some("nfs: adjust layout_types decode"),
            "The decode_pnfs_layout_types path miscomputes u32 lengths.",
        )
        .unwrap();
        w.commit().unwrap();
    }

    #[test]
    fn index_and_search_roundtrip() {
        let d = tempdir().unwrap();
        ingest_three(d.path());

        let r = BmReader::open(d.path()).unwrap();
        let hits = r.search("smb_check_perm_dacl", 10).unwrap();
        assert_eq!(hits.len(), 1, "got {hits:?}");
        assert_eq!(hits[0].0, "<m1@x>");

        let hits = r.search("smb2_create", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "<m2@x>");

        let hits = r.search("ksmbd", 10).unwrap();
        let mids: Vec<&str> = hits.iter().map(|(m, _)| m.as_str()).collect();
        assert!(mids.contains(&"<m1@x>"));
        assert!(mids.contains(&"<m2@x>"));
        assert!(!mids.contains(&"<m3@x>"));
    }

    #[test]
    fn case_insensitive_match() {
        let d = tempdir().unwrap();
        ingest_three(d.path());
        let r = BmReader::open(d.path()).unwrap();

        // Uppercase query matches lowercased stored tokens.
        let hits = r.search("LAYOUT_TYPES", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "<m3@x>");
    }

    #[test]
    fn phrase_query_rejected() {
        let d = tempdir().unwrap();
        ingest_three(d.path());
        let r = BmReader::open(d.path()).unwrap();
        let err = r.search("\"ACL bounds\"", 10).unwrap_err();
        match err {
            Error::QueryParse(m) => assert!(m.contains("phrase queries")),
            other => panic!("expected QueryParse, got {other:?}"),
        }
    }

    #[test]
    fn empty_query_is_empty() {
        let d = tempdir().unwrap();
        ingest_three(d.path());
        let r = BmReader::open(d.path()).unwrap();
        assert!(r.search("", 10).unwrap().is_empty());
        assert!(r.search("   ", 10).unwrap().is_empty());
    }

    #[test]
    fn kernel_ident_splitter_runs_whole_identifiers() {
        let mut t = KernelIdentSplitter::default();
        let mut s = t.token_stream("vector_mmsg_rx + SMB2_CREATE bar.baz");
        let mut got = Vec::new();
        while s.advance() {
            got.push(s.token().text.clone());
        }
        assert_eq!(
            got,
            vec![
                "vector_mmsg_rx".to_owned(),
                "SMB2_CREATE".to_owned(),
                "bar".to_owned(),
                "baz".to_owned(),
            ]
        );
    }
}
