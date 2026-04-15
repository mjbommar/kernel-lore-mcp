//! Trigram tier — Zoekt-style byte-trigram index over patch content.
//!
//! Indexes only patch bodies. Answers substring + (simple) regex
//! queries by intersecting posting lists, then confirms each
//! candidate by re-running the real regex/literal against the
//! uncompressed body.
//!
//! v0.5 scope:
//!   * per-(list, run_id) segments built from an in-memory
//!     `BTreeMap<u32, RoaringBitmap>` keyed by byte trigram.
//!   * On-disk layout under `<data_dir>/trigram/<list>/<run_id>/`:
//!     - `trigrams.fst` — fst::Map: u32 trigram (big-endian) -> posting offset
//!     - `trigrams.postings` — concatenated portable-roaring bitmaps
//!     - `docs.tsv` — u32 local docid -> message_id, line-delimited
//!   * Query path: `search_substring` takes a literal byte string,
//!     enumerates required trigrams (every overlapping 3-byte
//!     window), intersects postings, returns candidate message_ids.
//!   * Candidate cap at `TRIGRAM_CONFIRM_LIMIT`; confirmation
//!     happens in a caller that has a `Store` handle.
//!
//! Deferred to follow-ups:
//!   * regex → required-trigram extraction (codesearch-style `Query`
//!     tree). v0.5 accepts literal substring only; the router will
//!     build on this.
//!   * cross-segment docid unification (`RoaringTreemap`).
//!   * compaction + segment merging.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use fst::{Map as FstMap, MapBuilder, Streamer};
use roaring::RoaringBitmap;

use crate::error::{Error, Result};

/// Maximum candidates kept after posting-list intersection before
/// confirmation. See `docs/indexing/trigram-tier.md`.
pub const TRIGRAM_CONFIRM_LIMIT: usize = 4096;

/// In-memory segment builder. Feed patch bodies in, finalize writes
/// the three files to disk.
pub struct SegmentBuilder {
    postings: BTreeMap<u32, RoaringBitmap>,
    docs: Vec<String>, // local docid -> message_id
}

impl Default for SegmentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SegmentBuilder {
    pub fn new() -> Self {
        Self {
            postings: BTreeMap::new(),
            docs: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Add one patch body.  `message_id` is the Message-ID string (as
    /// stored in the metadata tier); we use it verbatim in `docs.tsv`
    /// for the cross-walk.
    pub fn add(&mut self, message_id: &str, patch: &[u8]) -> Result<()> {
        if message_id.contains('\n') {
            return Err(Error::State(format!(
                "message_id contains newline: {message_id:?}"
            )));
        }
        let docid = u32::try_from(self.docs.len())
            .map_err(|_| Error::State("segment exceeds u32 docids".to_owned()))?;
        self.docs.push(message_id.to_owned());
        for tri in patch_trigrams(patch) {
            self.postings.entry(tri).or_default().insert(docid);
        }
        Ok(())
    }

    /// Finalize the three files under `dir`. `dir` must not exist yet
    /// (we use atomic-rename from a sibling `.tmp` dir).
    pub fn finalize(self, dir: &Path) -> Result<()> {
        if self.is_empty() {
            return Ok(()); // nothing to write
        }

        let parent = dir
            .parent()
            .ok_or_else(|| Error::State(format!("no parent for {}", dir.display())))?;
        fs::create_dir_all(parent)?;
        let tmp = parent.join(format!(
            ".{}.tmp",
            dir.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("segment")
        ));
        if tmp.exists() {
            fs::remove_dir_all(&tmp)?;
        }
        fs::create_dir_all(&tmp)?;

        // docs.tsv
        let docs_path = tmp.join("docs.tsv");
        {
            let mut f = BufWriter::new(File::create(&docs_path)?);
            for mid in &self.docs {
                f.write_all(mid.as_bytes())?;
                f.write_all(b"\n")?;
            }
            f.flush()?;
        }

        // postings + fst
        let postings_path = tmp.join("trigrams.postings");
        let fst_path = tmp.join("trigrams.fst");
        {
            let mut postings_w = BufWriter::new(File::create(&postings_path)?);
            let fst_w = BufWriter::new(File::create(&fst_path)?);
            let mut builder =
                MapBuilder::new(fst_w).map_err(|e| Error::State(format!("fst builder: {e}")))?;
            let mut offset: u64 = 0;
            for (tri, bitmap) in &self.postings {
                // Write bitmap at current offset; key lex order matches
                // numeric u32 order since we encode big-endian.
                let mut buf = Vec::with_capacity(bitmap.serialized_size());
                bitmap
                    .serialize_into(&mut buf)
                    .map_err(|e| Error::State(format!("roaring serialize: {e}")))?;
                postings_w.write_all(&buf)?;
                let key = u32_be(*tri);
                builder
                    .insert(key, offset)
                    .map_err(|e| Error::State(format!("fst insert: {e}")))?;
                offset = offset
                    .checked_add(buf.len() as u64)
                    .ok_or_else(|| Error::State("posting offset overflow".to_owned()))?;
            }
            builder
                .finish()
                .map_err(|e| Error::State(format!("fst finish: {e}")))?;
            postings_w.flush()?;
        }

        // Atomic swap.
        fs::rename(&tmp, dir)?;
        Ok(())
    }
}

/// Read-only handle on one finalized segment. Mmaps the fst + postings.
pub struct SegmentReader {
    fst: FstMap<Vec<u8>>,
    postings: Vec<u8>,
    docs: Vec<String>,
}

impl SegmentReader {
    pub fn open(dir: &Path) -> Result<Self> {
        let fst_bytes = fs::read(dir.join("trigrams.fst"))?;
        let fst = FstMap::new(fst_bytes).map_err(|e| Error::State(format!("fst open: {e}")))?;
        let postings = fs::read(dir.join("trigrams.postings"))?;
        let mut docs_s = String::new();
        File::open(dir.join("docs.tsv"))?.read_to_string(&mut docs_s)?;
        let docs = docs_s.lines().map(str::to_owned).collect::<Vec<_>>();
        Ok(Self {
            fst,
            postings,
            docs,
        })
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    fn posting_for(&self, trigram: u32) -> Option<RoaringBitmap> {
        let key = u32_be(trigram);
        let offset = self.fst.get(key)?;
        let tail = &self.postings[offset as usize..];
        RoaringBitmap::deserialize_from(tail).ok()
    }

    /// Intersect postings for every required trigram in `needle`.
    /// Returns local docids (into `self.docs`).
    fn candidate_docids(&self, needle: &[u8]) -> RoaringBitmap {
        let mut trigrams = Vec::new();
        for w in needle.windows(3) {
            if w.iter().all(|b| *b < 0x80) {
                trigrams.push(pack_trigram(w));
            }
        }
        if trigrams.is_empty() {
            return RoaringBitmap::new();
        }
        trigrams.sort_unstable();
        trigrams.dedup();

        let mut iter = trigrams.into_iter();
        let first = iter.next().unwrap();
        let Some(mut acc) = self.posting_for(first) else {
            return RoaringBitmap::new();
        };
        for tri in iter {
            let Some(b) = self.posting_for(tri) else {
                return RoaringBitmap::new();
            };
            acc &= b;
            if acc.is_empty() {
                return acc;
            }
        }
        acc
    }

    /// Substring search: returns candidate `message_id`s that MIGHT
    /// contain `needle`. Caller must confirm by decompressing the body
    /// and running the real needle.
    pub fn candidates_for_substring(&self, needle: &[u8]) -> Vec<&str> {
        self.candidates_for_substring_fuzzy(needle, 0)
    }

    /// Fuzzy-aware substring candidate search. At `fuzzy_edits == 0`
    /// this is exact (AND of all trigrams). At `fuzzy_edits > 0`,
    /// uses threshold intersection: require at least
    /// `max(1, num_trigrams - 3*k)` trigrams to match. Each edit can
    /// destroy at most 3 trigrams (pigeonhole principle).
    pub fn candidates_for_substring_fuzzy(&self, needle: &[u8], fuzzy_edits: u32) -> Vec<&str> {
        if needle.len() < 3 {
            return self.docs.iter().map(String::as_str).collect();
        }
        if fuzzy_edits == 0 {
            let bitmap = self.candidate_docids(needle);
            return bitmap
                .iter()
                .take(TRIGRAM_CONFIRM_LIMIT)
                .filter_map(|docid| self.docs.get(docid as usize).map(String::as_str))
                .collect();
        }

        // Threshold intersection for fuzzy: a document must contain
        // at least `threshold` of the needle's trigrams.
        let mut trigrams = Vec::new();
        for w in needle.windows(3) {
            if w.iter().all(|b| *b < 0x80) {
                trigrams.push(pack_trigram(w));
            }
        }
        if trigrams.is_empty() {
            return self.docs.iter().map(String::as_str).collect();
        }
        trigrams.sort_unstable();
        trigrams.dedup();

        let threshold = trigrams
            .len()
            .saturating_sub(3 * fuzzy_edits as usize)
            .max(1);

        // Count how many of the needle's trigrams each doc matches.
        let mut counts: HashMap<u32, usize> = HashMap::new();
        for tri in &trigrams {
            if let Some(bitmap) = self.posting_for(*tri) {
                for docid in bitmap.iter() {
                    *counts.entry(docid).or_default() += 1;
                }
            }
        }

        // Collect ALL candidates meeting the threshold, sort by
        // match quality (count DESC, docid ASC) so the cap is
        // deterministic + quality-biased rather than hash-order-
        // dependent. Without this, identical queries can return
        // different result sets across runs.
        let mut candidates: Vec<(u32, usize)> = counts
            .into_iter()
            .filter(|(_, c)| *c >= threshold)
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        let mut out: Vec<&str> = candidates
            .into_iter()
            .take(TRIGRAM_CONFIRM_LIMIT)
            .filter_map(|(docid, _)| self.docs.get(docid as usize).map(String::as_str))
            .collect();
        out.sort_unstable();
        out
    }

    /// Yield every distinct trigram key in the segment. Useful for
    /// diagnostics + property tests.
    pub fn iter_trigrams(&self) -> impl Iterator<Item = u32> + '_ {
        TrigramKeyIter::new(&self.fst)
    }
}

struct TrigramKeyIter<'a> {
    stream: fst::map::Stream<'a>,
}

impl<'a> TrigramKeyIter<'a> {
    fn new(fst: &'a FstMap<Vec<u8>>) -> Self {
        Self {
            stream: fst.stream(),
        }
    }
}

impl<'a> Iterator for TrigramKeyIter<'a> {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        let (k, _v) = self.stream.next()?;
        Some(u32::from_be_bytes([k[0], k[1], k[2], 0]))
    }
}

fn u32_be(v: u32) -> [u8; 3] {
    // fst keys need lex order == numeric order. Big-endian 3-byte
    // encoding gives both.
    let be = v.to_be_bytes();
    [be[1], be[2], be[3]]
}

fn pack_trigram(bytes: &[u8]) -> u32 {
    debug_assert!(bytes.len() == 3);
    ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | (bytes[2] as u32)
}

/// Iterate byte trigrams in a patch body. Skips windows containing a
/// non-ASCII byte (≥ 0x80) — see `docs/indexing/trigram-tier.md` for
/// the symmetric query-side behaviour.
pub fn patch_trigrams(patch: &[u8]) -> impl Iterator<Item = u32> + '_ {
    patch
        .windows(3)
        .filter(|w| w.iter().all(|b| *b < 0x80))
        .map(pack_trigram)
}

// ---------------------------------------------------------------------
// Path helpers — the ingest + query sides both need the canonical
// `<data_dir>/trigram/<list>/<run_id>/` layout.

pub fn segment_dir(data_dir: &Path, list: &str, run_id: &str) -> PathBuf {
    data_dir.join("trigram").join(list).join(run_id)
}

/// Enumerate every segment under `<data_dir>/trigram/<list>/`.
pub fn list_segments(data_dir: &Path, list: &str) -> Result<Vec<PathBuf>> {
    let root = data_dir.join("trigram").join(list);
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            // Guard against the `.tmp` rename buffer (atomic finalize).
            let name = entry.file_name();
            if let Some(s) = name.to_str() {
                if !s.starts_with('.') {
                    out.push(entry.path());
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

// Avoid the unused-import lint on the `io` we pull in for future use.
#[allow(dead_code)]
fn _io_anchor(_: io::Error) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_patch() -> &'static [u8] {
        b"diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c\n\
--- a/fs/smb/server/smbacl.c\n\
+++ b/fs/smb/server/smbacl.c\n\
@@ -1,3 +1,5 @@ int smb_check_perm_dacl(struct ksmbd_conn *c)\n\
 a\n\
+if (ace_size < sizeof(struct smb_ace))\n\
+    return -EINVAL;\n"
    }

    #[test]
    fn build_and_query_roundtrip() {
        let tmp = tempdir().unwrap();
        let data = tmp.path();
        let mut b = SegmentBuilder::new();
        b.add("<m1@x>", sample_patch()).unwrap();
        b.add(
            "<m2@x>",
            b"diff --git a/other.c b/other.c\n@@ @@ int foo()\n-x\n+y\n",
        )
        .unwrap();
        b.finalize(&segment_dir(data, "linux-cifs", "run-0001"))
            .unwrap();

        let segs = list_segments(data, "linux-cifs").unwrap();
        assert_eq!(segs.len(), 1);
        let reader = SegmentReader::open(&segs[0]).unwrap();
        assert_eq!(reader.len(), 2);

        // Substring that appears only in m1's patch
        let cands = reader.candidates_for_substring(b"smb_check_perm_dacl");
        assert!(cands.contains(&"<m1@x>"));
        assert!(!cands.contains(&"<m2@x>"));

        // Substring that appears only in m2
        let cands = reader.candidates_for_substring(b"other.c");
        assert!(cands.contains(&"<m2@x>"));
        assert!(!cands.contains(&"<m1@x>"));

        // Substring shared by both (`diff --git`)
        let cands = reader.candidates_for_substring(b"diff --git");
        assert!(cands.contains(&"<m1@x>"));
        assert!(cands.contains(&"<m2@x>"));

        // Substring absent from the corpus
        let cands = reader.candidates_for_substring(b"DOES_NOT_EXIST_ANYWHERE");
        assert!(cands.is_empty());
    }

    #[test]
    fn short_needle_returns_all_docs() {
        let tmp = tempdir().unwrap();
        let mut b = SegmentBuilder::new();
        b.add("<m1@x>", b"hello world").unwrap();
        b.finalize(&segment_dir(tmp.path(), "l", "r1")).unwrap();
        let segs = list_segments(tmp.path(), "l").unwrap();
        let r = SegmentReader::open(&segs[0]).unwrap();
        let cands = r.candidates_for_substring(b"hi");
        assert_eq!(cands, vec!["<m1@x>"]);
    }

    #[test]
    fn rejects_newline_in_message_id() {
        let mut b = SegmentBuilder::new();
        assert!(b.add("mid\nwith-newline", b"xxx").is_err());
    }

    #[test]
    fn empty_builder_finalize_is_noop() {
        let tmp = tempdir().unwrap();
        let dir = segment_dir(tmp.path(), "l", "empty");
        SegmentBuilder::new().finalize(&dir).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn ascii_only_trigrams_skip_non_ascii() {
        let input = b"\xc3\xa9foo\xc3\xa9bar";
        let got: Vec<u32> = patch_trigrams(input).collect();
        // Only "foo", "oob", "oba", "bar" (wait — "bar" isn't windowed:
        // "foo", "oo\xc3" (skip), "o\xc3\xa9" (skip), "\xc3\xa9b" (skip),
        // "\xa9ba" (skip), "bar"). So we expect {foo, bar}.
        let expected = vec![pack_trigram(b"foo"), pack_trigram(b"bar")];
        let mut got_sorted = got.clone();
        got_sorted.sort_unstable();
        let mut exp_sorted = expected.clone();
        exp_sorted.sort_unstable();
        assert_eq!(got_sorted, exp_sorted);
    }
}
