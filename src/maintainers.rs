//! Parser + matcher for the Linux kernel `MAINTAINERS` file.
//!
//! The MAINTAINERS file lists subsystems, maintainers, reviewers,
//! lists, and file patterns. This module parses it into a structured
//! index and answers "who owns path P?" with the same algorithm as
//! `scripts/get_maintainer.pl`.
//!
//! Tag reference (per `get_maintainer.pl`):
//!   M:  Maintainer(s) — patch recipient(s)
//!   R:  Designated reviewer(s) — CC target
//!   L:  Mailing list
//!   S:  Status (Supported / Maintained / Odd Fixes / Orphan / Obsolete)
//!   F:  File pattern — glob, expanded to regex
//!   X:  Exclude pattern — tested BEFORE F:
//!   N:  Filename regex (Perl regex, not glob)
//!   K:  Body-content regex (matches against patch content, NOT filename)
//!   T:  SCM tree URL
//!   W:  Web page
//!   Q:  Patchwork URL
//!   B:  Bug tracker URI
//!   C:  Chat URI
//!   P:  Profile doc path
//!
//! Matching:
//!   - Blank line = section break.
//!   - `#` lines are comments.
//!   - F:/X: use glob → regex:
//!       `*`  -> `[^/]*`
//!       `**` -> `.*`    (our choice: full path across segments)
//!       `?`  -> `.`
//!       trailing `/` matches the subtree under that prefix.
//!       leading `!` is NOT a kernel convention (that's `.gitignore`
//!       syntax); X: is the exclusion mechanism.
//!   - N: is raw regex applied to the full relative path.
//!   - Depth priority = count of `/` in the pattern. More specific
//!     (deeper) patterns win over shallower ones. N: counts as
//!     depth 0 (lowest priority), matching get_maintainer.pl.
//!
//! Sources:
//!   - scripts/get_maintainer.pl in torvalds/linux
//!   - Documentation/process/maintainers.rst

#![allow(dead_code)]

use std::path::Path;

use regex_automata::dfa::dense::DFA;
use regex_automata::util::syntax::Config as SyntaxConfig;

use crate::error::{Error, Result};

/// One MAINTAINERS section.
#[derive(Debug, Clone, Default)]
pub struct MaintainerEntry {
    /// Free-text section title (first line of the section, if any).
    pub name: String,
    /// M: lines — maintainer addresses ("Name <email>" lines).
    pub maintainers: Vec<String>,
    /// R: lines — designated reviewers.
    pub reviewers: Vec<String>,
    /// L: lines — mailing lists.
    pub lists: Vec<String>,
    /// S: line value — one of Supported/Maintained/Odd Fixes/Orphan/Obsolete.
    pub status: Option<String>,
    /// F: patterns (compiled). Ordered as declared.
    pub f_patterns: Vec<Pattern>,
    /// X: patterns (compiled).
    pub x_patterns: Vec<Pattern>,
    /// N: raw regex patterns (compiled to DFA via regex-automata).
    pub n_regex_sources: Vec<String>,
    /// K: raw regex patterns — matched against patch/diff content,
    /// not file paths. Stored but not consulted by `matches_path`.
    pub k_regex_sources: Vec<String>,
    /// T: SCM tree URLs.
    pub scm_trees: Vec<String>,
    /// W: web URLs.
    pub web_pages: Vec<String>,
    /// Q: patchwork URLs.
    pub patchwork_urls: Vec<String>,
}

impl MaintainerEntry {
    /// Depth score for ranking. Highest F: depth wins; N: counts as 0.
    /// Use this when sorting candidates for a path so the most
    /// specific subsystem claim surfaces first.
    pub fn depth(&self) -> usize {
        self.f_patterns.iter().map(|p| p.depth).max().unwrap_or(0)
    }
}

/// A compiled F: or X: glob: regex + depth metadata.
#[derive(Debug, Clone)]
pub struct Pattern {
    pub glob: String,
    pub regex_source: String,
    pub depth: usize,
}

/// A parsed MAINTAINERS file. Hold one at Reader startup; rebuild
/// on kernel-tag change.
#[derive(Debug, Default)]
pub struct MaintainersIndex {
    pub entries: Vec<MaintainerEntry>,
}

impl MaintainersIndex {
    /// Parse a MAINTAINERS file from its raw text.
    pub fn parse(text: &str) -> Result<Self> {
        // Skip the preamble: the real sections begin at the first
        // line that looks like `<Section Title>` followed by tagged
        // lines, typically after a `Maintainers List` header. We
        // don't try to locate it precisely — we just treat any run
        // of non-blank non-# lines as a section candidate; the parser
        // discards sections that have no M:/R:/L:/S:/F:/N: tags.

        let mut out = MaintainersIndex::default();
        let mut cur = MaintainerEntry::default();
        let mut cur_has_tags = false;
        let mut cur_title: Option<String> = None;

        for raw in text.lines() {
            let line = raw.trim_end();
            if line.is_empty() {
                // Section break.
                if cur_has_tags {
                    if let Some(t) = cur_title.take() {
                        cur.name = t;
                    }
                    out.entries.push(std::mem::take(&mut cur));
                }
                cur_has_tags = false;
                cur_title = None;
                continue;
            }
            if line.starts_with('#') {
                continue;
            }
            // Tag lines: `^([A-Z]):\s*(.*)$`.
            let bytes = line.as_bytes();
            if bytes.len() >= 3 && bytes[0].is_ascii_uppercase() && bytes[1] == b':' {
                let tag = bytes[0];
                let value = line[2..].trim_start();
                match tag {
                    b'M' => cur.maintainers.push(value.to_owned()),
                    b'R' => cur.reviewers.push(value.to_owned()),
                    b'L' => cur.lists.push(value.to_owned()),
                    b'S' => cur.status = Some(value.to_owned()),
                    b'F' => {
                        cur.f_patterns.push(compile_glob(value));
                    }
                    b'X' => {
                        cur.x_patterns.push(compile_glob(value));
                    }
                    b'N' => cur.n_regex_sources.push(value.to_owned()),
                    b'K' => cur.k_regex_sources.push(value.to_owned()),
                    b'T' => cur.scm_trees.push(value.to_owned()),
                    b'W' => cur.web_pages.push(value.to_owned()),
                    b'Q' => cur.patchwork_urls.push(value.to_owned()),
                    _ => {
                        // B:/C:/P:/D:/H: — captured silently for
                        // forward compatibility; we don't currently
                        // surface them.
                    }
                }
                cur_has_tags = true;
            } else if !cur_has_tags {
                // First non-tag line of a new section — treat as title.
                cur_title = Some(line.to_owned());
            }
        }
        if cur_has_tags {
            if let Some(t) = cur_title.take() {
                cur.name = t;
            }
            out.entries.push(cur);
        }

        Ok(out)
    }

    /// Return every entry whose F:/N: matches `path` AND no X: matches.
    /// Sorted by depth descending (most-specific first).
    pub fn lookup(&self, path: &str) -> Vec<&MaintainerEntry> {
        let mut hits: Vec<(usize, &MaintainerEntry)> = Vec::new();
        for entry in &self.entries {
            if entry.x_patterns.iter().any(|p| pattern_matches(p, path)) {
                continue;
            }
            // Highest depth among F: that matched.
            let mut best_depth: Option<usize> = None;
            for p in &entry.f_patterns {
                if pattern_matches(p, path) {
                    best_depth = Some(best_depth.map_or(p.depth, |d| d.max(p.depth)));
                }
            }
            // N: contributes depth 0 if it matches and nothing else did.
            if best_depth.is_none() && !entry.n_regex_sources.is_empty() {
                let n_match = entry
                    .n_regex_sources
                    .iter()
                    .any(|src| regex_dfa_match(src, path).unwrap_or(false));
                if n_match {
                    best_depth = Some(0);
                }
            }
            if let Some(d) = best_depth {
                hits.push((d, entry));
            }
        }
        hits.sort_by(|a, b| b.0.cmp(&a.0));
        hits.into_iter().map(|(_, e)| e).collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Parse from a file on disk.
    pub fn parse_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text)
    }
}

/// Convert a MAINTAINERS glob (F: / X: value) into a regex. Depth is
/// the count of `/` separators in the original glob — used to rank
/// specificity, matching `get_maintainer.pl`.
fn compile_glob(glob: &str) -> Pattern {
    let depth = glob.bytes().filter(|b| *b == b'/').count();
    let mut regex = String::with_capacity(glob.len() + 16);
    regex.push('^');
    let bytes = glob.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'*' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    regex.push_str(".*");
                    i += 2;
                    continue;
                }
                regex.push_str("[^/]*");
            }
            b'?' => regex.push('.'),
            b'.' | b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'+' | b'^' | b'$' | b'|' | b'\\' => {
                regex.push('\\');
                regex.push(c as char);
            }
            _ => regex.push(c as char),
        }
        i += 1;
    }
    // Trailing `/` in F: pattern ≡ "match everything beneath this dir".
    if glob.ends_with('/') {
        regex.push_str(".*");
    }
    regex.push('$');
    Pattern {
        glob: glob.to_owned(),
        regex_source: regex,
        depth,
    }
}

/// Test `path` against a compiled Pattern via regex-automata DFA.
/// Errors in regex construction are treated as non-matches — an
/// unparseable F: line shouldn't make the whole index return empty.
fn pattern_matches(p: &Pattern, path: &str) -> bool {
    regex_dfa_match(&p.regex_source, path).unwrap_or(false)
}

fn regex_dfa_match(regex: &str, path: &str) -> Result<bool> {
    // regex-automata DFA with `unicode(false)`: MAINTAINERS paths are
    // ASCII-only; unicode mode slows DFA construction with zero gain.
    let dfa = DFA::builder()
        .syntax(SyntaxConfig::new().unicode(false).utf8(false))
        .build(regex)
        .map_err(|e| Error::State(format!("maintainers regex {regex:?}: {e}")))?;
    use regex_automata::Input;
    use regex_automata::dfa::Automaton;
    let mut state = dfa
        .start_state_forward(&Input::new(path))
        .map_err(|e| Error::State(format!("dfa start: {e}")))?;
    for &b in path.as_bytes() {
        state = dfa.next_state(state, b);
    }
    state = dfa.next_eoi_state(state);
    Ok(dfa.is_match_state(state))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r"
# Preamble - ignored
Some free text

KSMBD (SERVER-SIDE CIFS)
M:	Namjae Jeon <linkinjeon@kernel.org>
M:	Sergey Senozhatsky <senozhatsky@chromium.org>
R:	Steve French <sfrench@samba.org>
L:	linux-cifs@vger.kernel.org
S:	Maintained
T:	git git://git.samba.org/ksmbd.git
F:	fs/smb/server/
F:	Documentation/filesystems/smb/ksmbd.rst

NETDEV
M:	Jakub Kicinski <kuba@kernel.org>
L:	netdev@vger.kernel.org
S:	Maintained
F:	net/
X:	net/bpf/
K:	(?:skb_|netdev_)

CATCH-ALL
N:	^drivers/.*\.c$
S:	Orphan
";

    #[test]
    fn parse_extracts_sections_and_tags() {
        let idx = MaintainersIndex::parse(SAMPLE).unwrap();
        assert_eq!(idx.len(), 3);
        let ksmbd = &idx.entries[0];
        assert!(ksmbd.maintainers.iter().any(|m| m.contains("Namjae Jeon")));
        assert!(ksmbd.reviewers.iter().any(|r| r.contains("Steve French")));
        assert_eq!(ksmbd.lists, vec!["linux-cifs@vger.kernel.org"]);
        assert_eq!(ksmbd.status.as_deref(), Some("Maintained"));
        assert_eq!(ksmbd.f_patterns.len(), 2);
        assert!(ksmbd.scm_trees[0].contains("samba.org"));
    }

    #[test]
    fn lookup_ksmbd_file_hits_ksmbd_entry() {
        let idx = MaintainersIndex::parse(SAMPLE).unwrap();
        let hits = idx.lookup("fs/smb/server/smbacl.c");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].name.starts_with("KSMBD"));
    }

    #[test]
    fn lookup_netdev_respects_exclude() {
        let idx = MaintainersIndex::parse(SAMPLE).unwrap();
        // net/core/sock.c matches NETDEV's F:net/, not excluded.
        let hits = idx.lookup("net/core/sock.c");
        assert!(hits.iter().any(|e| e.name == "NETDEV"));
        // net/bpf/verifier.c is excluded by X:net/bpf/ — no NETDEV.
        let hits = idx.lookup("net/bpf/verifier.c");
        assert!(
            !hits.iter().any(|e| e.name == "NETDEV"),
            "X: should have excluded NETDEV"
        );
    }

    #[test]
    fn lookup_catchall_via_n_regex() {
        let idx = MaintainersIndex::parse(SAMPLE).unwrap();
        let hits = idx.lookup("drivers/net/foobar.c");
        // CATCH-ALL matches via N:, and NETDEV matches F:net/ — wait no,
        // drivers/... does not match net/... Only CATCH-ALL should hit.
        assert!(hits.iter().any(|e| e.name == "CATCH-ALL"));
    }

    #[test]
    fn lookup_depth_ranking_deepest_first() {
        let text = r"
TOP
F:	drivers/
S:	Maintained

SPECIFIC
F:	drivers/net/ethernet/intel/e1000/
S:	Maintained
";
        let idx = MaintainersIndex::parse(text).unwrap();
        let hits = idx.lookup("drivers/net/ethernet/intel/e1000/e1000_main.c");
        assert_eq!(hits.len(), 2);
        // Deepest first.
        assert_eq!(hits[0].name, "SPECIFIC");
        assert_eq!(hits[1].name, "TOP");
    }

    #[test]
    fn glob_expansion_star_and_double_star() {
        let p1 = compile_glob("fs/*.c");
        assert!(pattern_matches(&p1, "fs/file.c"));
        assert!(!pattern_matches(&p1, "fs/smb/server/smbacl.c"));
        let p2 = compile_glob("fs/**/*.c");
        assert!(pattern_matches(&p2, "fs/smb/server/smbacl.c"));
    }

    #[test]
    fn comments_and_blank_sections_skipped() {
        let idx = MaintainersIndex::parse("\n# just a comment\n\nNAME\nM:	x@y\nS:	Orphan\nF:	/\n")
            .unwrap();
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.entries[0].name, "NAME");
    }
}
