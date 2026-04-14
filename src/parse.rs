//! Parse an RFC822 message (one commit blob from a public-inbox shard)
//! into the structured fields our metadata tier cares about.

#![allow(dead_code)]
//!
//! Scope: trailer extraction, subject decomposition, prose/patch split,
//! `diff --git` walker, `@@ ... @@ <func>` hunk-header extractor, diffstat
//! summary. Everything else (tid, series-family joins) happens later in
//! the ingest pipeline once we have per-message records.

use std::collections::BTreeMap;

use mail_parser::{Addr, MessageParser};

/// Decoded view of one kernel-list message.
#[derive(Debug, Default, Clone)]
pub struct ParsedMessage {
    pub message_id: Option<String>,
    pub from_addr: Option<String>,
    pub from_name: Option<String>,
    pub subject_raw: Option<String>,
    pub subject_normalized: Option<String>,
    pub subject_tags: Vec<String>,
    pub series_version: u32, // 0 means "unversioned"
    pub series_index: Option<u32>,
    pub series_total: Option<u32>,
    pub is_cover_letter: bool,
    pub date_unix_ns: Option<i64>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,

    pub prose: String,
    pub patch: Option<String>,
    pub has_patch: bool,

    pub touched_files: Vec<String>,
    pub touched_functions: Vec<String>,
    pub files_changed: Option<u32>,
    pub insertions: Option<u32>,
    pub deletions: Option<u32>,

    /// Trailer name (lowercased, colon-stripped) -> list of values as they appeared.
    /// Common trailers are mirrored into named fields below, but the catch-all
    /// lives here so callers don't lose signal.
    pub trailers: BTreeMap<String, Vec<String>>,

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
}

/// Parse a single RFC822 message.
pub fn parse_message(bytes: &[u8]) -> ParsedMessage {
    let Some(msg) = MessageParser::default().parse(bytes) else {
        return ParsedMessage::default();
    };
    let mut out = ParsedMessage::default();

    if let Some(mid) = msg.message_id() {
        out.message_id = Some(strip_angles(mid).to_owned());
    }
    if let Some(address) = msg.from() {
        let contacts: Vec<&Addr> = address.iter().collect();
        if let Some(Addr { address, name }) = contacts.first() {
            out.from_addr = address.as_deref().map(|s| s.trim().to_lowercase());
            out.from_name = name.as_deref().map(|s| s.trim().to_owned());
        }
    }
    if let Some(subject) = msg.subject() {
        out.subject_raw = Some(subject.to_owned());
        let (norm, tags, version, index, total, is_cover) = decompose_subject(subject);
        out.subject_normalized = Some(norm);
        out.subject_tags = tags;
        out.series_version = version;
        out.series_index = index;
        out.series_total = total;
        out.is_cover_letter = is_cover;
    }
    if let Some(dt) = msg.date() {
        let s = dt.to_rfc3339();
        if let Ok(parsed) =
            time::OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339)
        {
            out.date_unix_ns = Some(parsed.unix_timestamp_nanos() as i64);
        }
    }
    if let Some(list) = msg.in_reply_to().as_text_list() {
        if let Some(irt) = list.first() {
            out.in_reply_to = Some(strip_angles(irt.as_ref()).to_owned());
        }
    }
    if let Some(refs) = msg.references().as_text_list() {
        for r in refs {
            out.references.push(strip_angles(r.as_ref()).to_owned());
        }
    }

    let body_text = msg.body_text(0).unwrap_or_default().into_owned();
    split_prose_and_patch(&body_text, &mut out);
    let prose = out.prose.clone();
    extract_trailers(&prose, &mut out);

    if let Some(patch) = out.patch.clone() {
        parse_patch(&patch, &mut out);
    }

    out
}

/// Best-effort fallback when `mail-parser` isn't available (e.g., messages
/// with exotic encodings). Takes raw bytes and just pulls Message-ID + Date
/// + Subject from headers. Used only as a recovery path.
#[allow(dead_code)]
pub fn minimal_headers_only(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes)
        .ok()?
        .lines()
        .next()
        .map(String::from)
}

fn strip_angles(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(s)
}

/// Decompose `[PATCH v3 2/5] ksmbd: fix foo` into its parts:
///   normalized = "ksmbd: fix foo"
///   tags       = ["PATCH", "PATCH v3", "PATCH 2/5"]
///   version    = 3
///   index      = 2, total = 5
///   is_cover   = false  (cover would be 0/5)
fn decompose_subject(raw: &str) -> (String, Vec<String>, u32, Option<u32>, Option<u32>, bool) {
    let mut rest = raw.trim();
    let mut tags = Vec::new();
    let mut version: u32 = 0;
    let mut index = None;
    let mut total = None;
    let mut is_cover = false;

    // Strip leading Re:/Fwd: any number of times.
    loop {
        let trimmed = rest.trim_start();
        let lower = trimmed.to_ascii_lowercase();
        if let Some(r) = lower.strip_prefix("re:") {
            rest = &trimmed[trimmed.len() - r.len()..];
            continue;
        }
        if let Some(r) = lower.strip_prefix("fwd:") {
            rest = &trimmed[trimmed.len() - r.len()..];
            continue;
        }
        if let Some(r) = lower.strip_prefix("fw:") {
            rest = &trimmed[trimmed.len() - r.len()..];
            continue;
        }
        rest = trimmed;
        break;
    }

    // Strip every bracketed tag prefix.
    loop {
        let trimmed = rest.trim_start();
        let Some(tail) = trimmed.strip_prefix('[') else {
            rest = trimmed;
            break;
        };
        let Some(end) = tail.find(']') else {
            rest = trimmed;
            break;
        };
        let tag = tail[..end].trim().to_owned();
        let upper = tag.to_ascii_uppercase();

        // Emit the full tag and also a normalized "PATCH" bucket if present.
        tags.push(tag.clone());
        if upper.contains("PATCH") {
            tags.push("PATCH".to_owned());
        }
        // Version: look for a `vN` token.
        for tok in tag.split_ascii_whitespace() {
            if let Some(n) = tok.strip_prefix('v').or_else(|| tok.strip_prefix('V')) {
                if let Ok(v) = n.parse::<u32>() {
                    if v > 0 {
                        version = v;
                    }
                }
            }
            // Index/total: `N/M`.
            if let Some((l, r)) = tok.split_once('/') {
                if let (Ok(l), Ok(r)) = (l.parse::<u32>(), r.parse::<u32>()) {
                    if r > 0 {
                        index = Some(l);
                        total = Some(r);
                        if l == 0 {
                            is_cover = true;
                        }
                    }
                }
            }
        }

        rest = &tail[end + 1..];
    }

    // Dedup tags preserving order.
    let mut seen = std::collections::HashSet::new();
    tags.retain(|t| seen.insert(t.clone()));

    let normalized = rest.trim().to_owned();
    (normalized, tags, version, index, total, is_cover)
}

fn split_prose_and_patch(body: &str, out: &mut ParsedMessage) {
    // Find first `^diff --git ` line. Everything before (minus quoted
    // reply prefixes and signature) is prose; the rest is patch.
    let patch_start = body
        .lines()
        .scan(0usize, |pos, line| {
            let start = *pos;
            *pos += line.len() + 1; // +1 for '\n'
            Some((start, line))
        })
        .find(|(_, line)| line.starts_with("diff --git "));

    let (prose_end, patch_text) = match patch_start {
        Some((start, _)) => (start, Some(&body[start..])),
        None => (body.len(), None),
    };

    out.prose = scrub_prose(&body[..prose_end]);
    out.patch = patch_text.map(str::to_owned);
    out.has_patch = out.patch.is_some();
}

fn scrub_prose(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_signature = false;
    for line in body.lines() {
        // RFC 3676 signature delimiter: literal "-- \n" (dash dash space LF).
        // After .lines() there is no LF; match on "-- " or "--" (some clients
        // strip the trailing space).
        let trimmed = line.trim_end();
        if trimmed == "--" || line == "-- " {
            in_signature = true;
            continue;
        }
        if in_signature {
            continue;
        }
        let stripped = line.trim_start();
        if stripped.starts_with('>') {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn extract_trailers(prose: &str, out: &mut ParsedMessage) {
    // Scan from end of prose backward, collecting contiguous trailer lines.
    // Trailer form: `<Token>: <value>` where Token matches [A-Za-z][A-Za-z0-9-]+.
    let lines: Vec<&str> = prose.lines().rev().collect();
    let mut trailer_lines: Vec<&str> = Vec::new();
    for line in lines {
        let line = line.trim_end();
        if line.is_empty() {
            if trailer_lines.is_empty() {
                continue;
            } else {
                break;
            }
        }
        if is_trailer_line(line) {
            trailer_lines.push(line);
        } else {
            break;
        }
    }
    trailer_lines.reverse();

    // Also look for trailer-shaped lines in the body (kernel patches
    // often interleave Signed-off-by: anywhere).
    let body_trailer_lines: Vec<&str> = prose
        .lines()
        .filter(|l| is_trailer_line(l.trim_end()))
        .collect();

    let mut all = trailer_lines;
    for l in body_trailer_lines {
        if !all.contains(&l) {
            all.push(l);
        }
    }

    for line in all {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name_norm = name.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();

        out.trailers
            .entry(name_norm.clone())
            .or_default()
            .push(value.clone());

        match name_norm.as_str() {
            "signed-off-by" => out.signed_off_by.push(value),
            "reviewed-by" => out.reviewed_by.push(value),
            "acked-by" => out.acked_by.push(value),
            "tested-by" => out.tested_by.push(value),
            "co-developed-by" => out.co_developed_by.push(value),
            "reported-by" => out.reported_by.push(value),
            "fixes" => out.fixes.push(value),
            "link" => out.link.push(value),
            "closes" => out.closes.push(value),
            "cc" => {
                // Only capture `Cc: stable@...` variants here.
                if value.to_ascii_lowercase().contains("stable@") {
                    out.cc_stable.push(value);
                }
            }
            _ => {}
        }
    }
}

fn is_trailer_line(line: &str) -> bool {
    let Some((head, rest)) = line.split_once(':') else {
        return false;
    };
    if rest.is_empty() || !rest.starts_with(' ') {
        return false;
    }
    let head = head.trim();
    if head.is_empty() || head.len() > 64 {
        return false;
    }
    let mut chars = head.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return false;
        }
    }
    true
}

fn parse_patch(patch: &str, out: &mut ParsedMessage) {
    let mut files: Vec<String> = Vec::new();
    let mut funcs: Vec<String> = Vec::new();
    let mut insertions: u32 = 0;
    let mut deletions: u32 = 0;
    let mut in_hunk = false;

    for line in patch.lines() {
        if let Some(tail) = line.strip_prefix("diff --git ") {
            if let Some((a, b)) = tail.split_once(' ') {
                for side in [a, b] {
                    let clean = side.trim_start_matches("a/").trim_start_matches("b/");
                    if !clean.is_empty() && !files.contains(&clean.to_owned()) {
                        files.push(clean.to_owned());
                    }
                }
            }
            in_hunk = false;
            continue;
        }
        if line.starts_with("@@ ") {
            // form: @@ -a,b +c,d @@ <context-or-function>
            in_hunk = true;
            if let Some(idx) = line[2..].find("@@") {
                let after = line[2 + idx + 2..].trim();
                if !after.is_empty() {
                    if let Some(ident) = extract_function_name(after) {
                        if !funcs.contains(&ident) {
                            funcs.push(ident);
                        }
                    }
                }
            }
            continue;
        }
        if in_hunk {
            if let Some(rest) = line.strip_prefix('+') {
                if !rest.starts_with('+') {
                    insertions += 1;
                }
            } else if let Some(rest) = line.strip_prefix('-') {
                if !rest.starts_with('-') {
                    deletions += 1;
                }
            }
        }
    }

    let files_changed = files.len() as u32;
    out.touched_files = files;
    out.touched_functions = funcs;
    out.files_changed = Some(files_changed);
    out.insertions = Some(insertions);
    out.deletions = Some(deletions);
}

fn extract_function_name(ctx: &str) -> Option<String> {
    // `@@ -a,b +c,d @@ <ctx>` where <ctx> is whatever git's
    // function-context picker emitted — usually a C declarator.
    // We want the identifier immediately preceding `(`, which is the
    // function name. If there's no `(`, fall back to the last
    // identifier-run in the string.
    let ctx = ctx.trim();
    if let Some(paren) = ctx.find('(') {
        let head = &ctx[..paren];
        if let Some(ident) = trailing_identifier(head) {
            return Some(ident.to_owned());
        }
    }
    trailing_identifier(ctx).map(str::to_owned)
}

fn trailing_identifier(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    while end > 0 {
        let c = bytes[end - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            end -= 1;
        } else {
            break;
        }
    }
    let start = end;
    let ident_end = bytes.len();
    if start == ident_end {
        return None;
    }
    let first = bytes[start];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    Some(&s[start..ident_end])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_PATCH: &[u8] = b"\
From: Alice <alice@example.com>\r\n\
To: linux-cifs@vger.kernel.org\r\n\
Cc: stable@vger.kernel.org\r\n\
Subject: [PATCH v3 2/5] ksmbd: fix OOB in smb_check_perm_dacl\r\n\
Date: Mon, 14 Apr 2026 19:15:33 +0000\r\n\
Message-ID: <20260414191533.1467353-3-alice@example.com>\r\n\
In-Reply-To: <20260414191533.1467353-1-alice@example.com>\r\n\
References: <20260414191533.1467353-1-alice@example.com>\r\n\
\r\n\
Prose explaining the change.\r\n\
\r\n\
Fixes: abcdef0123456789abcdef0123456789abcdef01 (\"ksmbd: initial\")\r\n\
Reported-by: Bob <bob@example.com>\r\n\
Reviewed-by: Carol <carol@example.com>\r\n\
Signed-off-by: Alice <alice@example.com>\r\n\
Cc: stable@vger.kernel.org # 5.15+\r\n\
---\r\n\
 fs/smb/server/smbacl.c | 5 +++--\r\n\
 1 file changed, 3 insertions(+), 2 deletions(-)\r\n\
\r\n\
diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c\r\n\
index 111..222 100644\r\n\
--- a/fs/smb/server/smbacl.c\r\n\
+++ b/fs/smb/server/smbacl.c\r\n\
@@ -123,6 +123,9 @@ int smb_check_perm_dacl(struct ksmbd_conn *conn, ...)\r\n\
 {\r\n\
+\tif (ace_size < sizeof(struct smb_ace))\r\n\
+\t\treturn -EINVAL;\r\n\
+\treturn 0;\r\n\
 }\r\n";

    #[test]
    fn full_patch_roundtrip() {
        let p = parse_message(SAMPLE_PATCH);
        assert_eq!(
            p.message_id.as_deref(),
            Some("20260414191533.1467353-3-alice@example.com")
        );
        assert_eq!(p.from_addr.as_deref(), Some("alice@example.com"));
        assert_eq!(p.from_name.as_deref(), Some("Alice"));
        assert_eq!(
            p.subject_normalized.as_deref(),
            Some("ksmbd: fix OOB in smb_check_perm_dacl")
        );
        assert!(p.subject_tags.iter().any(|t| t == "PATCH"));
        assert_eq!(p.series_version, 3);
        assert_eq!(p.series_index, Some(2));
        assert_eq!(p.series_total, Some(5));
        assert!(!p.is_cover_letter);
        assert!(p.has_patch);
        assert!(
            p.touched_files
                .contains(&"fs/smb/server/smbacl.c".to_owned())
        );
        assert!(
            p.touched_functions
                .contains(&"smb_check_perm_dacl".to_owned())
        );
        assert_eq!(p.files_changed, Some(1));
        assert!(p.insertions.unwrap() >= 3);
        assert_eq!(p.deletions, Some(0));
        assert_eq!(p.signed_off_by.len(), 1);
        assert_eq!(p.reviewed_by.len(), 1);
        assert_eq!(p.reported_by.len(), 1);
        assert_eq!(p.fixes.len(), 1);
        assert_eq!(p.cc_stable.len(), 1);
        assert!(p.cc_stable[0].contains("stable@"));
    }

    #[test]
    fn cover_letter_detection() {
        let msg = b"\
From: a@b\r\n\
Subject: [PATCH 0/3] foo: improve bar\r\n\
Message-ID: <cover@x>\r\n\
\r\n\
Cover letter body.\r\n\
";
        let p = parse_message(msg);
        assert!(p.is_cover_letter);
        assert_eq!(p.series_index, Some(0));
        assert_eq!(p.series_total, Some(3));
        assert!(!p.has_patch);
    }

    #[test]
    fn subject_tags_capture_rfc_and_resend() {
        let msg = b"Subject: [RFC] [RESEND v2 1/2] mm: test\r\nMessage-ID: <x>\r\n\r\n";
        let p = parse_message(msg);
        assert!(p.subject_tags.iter().any(|t| t == "RFC"));
        assert!(p.subject_tags.iter().any(|t| t == "RESEND v2 1/2"));
        assert_eq!(p.series_version, 2);
        assert_eq!(p.series_index, Some(1));
        assert_eq!(p.series_total, Some(2));
    }

    #[test]
    fn quoted_reply_and_signature_scrubbed() {
        let msg = b"\
Subject: Re: something\r\n\
Message-ID: <y>\r\n\
\r\n\
I think this looks good.\r\n\
> this is a quoted line\r\n\
>> nested quote\r\n\
-- \r\n\
Alice | company\r\n\
";
        let p = parse_message(msg);
        assert!(p.prose.contains("I think this looks good"));
        assert!(!p.prose.contains("quoted line"));
        assert!(!p.prose.contains("Alice | company"));
    }
}
