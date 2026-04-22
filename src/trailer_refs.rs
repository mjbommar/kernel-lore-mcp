//! Normalization helpers for trailer-derived lookup keys.
//!
//! The same extraction rules must drive both:
//!   * `over.db` side-table population at ingest/backfill time, and
//!   * reader-side fallback scans on deployments whose side tables
//!     haven't been backfilled yet.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TrailerRef {
    pub ref_kind: &'static str,
    pub ref_value: String,
}

pub const REF_KIND_RAW_LC: &str = "raw_lc";
pub const REF_KIND_EMAIL: &str = "email";
pub const REF_KIND_SHA_PREFIX: &str = "sha_prefix";
pub const REF_KIND_SYZBOT_HASH: &str = "syzbot_hash";
pub const REF_KIND_LORE_MID: &str = "lore_mid";
pub const REF_KIND_URL_LC: &str = "url_lc";

pub fn strip_angles(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(s)
}

pub fn extract_email(s: &str) -> String {
    let start = match s.rfind('<') {
        Some(i) => i + 1,
        None => {
            return s
                .split_whitespace()
                .find(|tok| tok.contains('@'))
                .map(|tok| {
                    tok.trim_matches(|c: char| !c.is_ascii_graphic())
                        .to_ascii_lowercase()
                })
                .unwrap_or_default();
        }
    };
    let end = match s[start..].find('>') {
        Some(i) => start + i,
        None => return String::new(),
    };
    s[start..end].trim().to_ascii_lowercase()
}

pub fn normalize_trailer_ref_value(ref_kind: &str, value: &str) -> String {
    match ref_kind {
        REF_KIND_RAW_LC | REF_KIND_EMAIL | REF_KIND_URL_LC | REF_KIND_SHA_PREFIX => {
            value.trim().to_ascii_lowercase()
        }
        REF_KIND_SYZBOT_HASH => value.trim().to_ascii_lowercase(),
        REF_KIND_LORE_MID => strip_angles(value).trim().to_owned(),
        _ => value.trim().to_owned(),
    }
}

pub fn extract_trailer_refs(kind: &str, raw: &str) -> Vec<TrailerRef> {
    let kind_lc = kind.to_ascii_lowercase();
    let raw_trimmed = raw.trim();
    if raw_trimmed.is_empty() {
        return Vec::new();
    }

    let mut out: BTreeSet<TrailerRef> = BTreeSet::new();
    out.insert(TrailerRef {
        ref_kind: REF_KIND_RAW_LC,
        ref_value: raw_trimmed.to_ascii_lowercase(),
    });

    if kind_lc == "reported_by" {
        let email = extract_email(raw_trimmed);
        if !email.is_empty() {
            out.insert(TrailerRef {
                ref_kind: REF_KIND_EMAIL,
                ref_value: email.clone(),
            });
            if let Some(hash) = extract_syzbot_hash_from_email(&email) {
                out.insert(TrailerRef {
                    ref_kind: REF_KIND_SYZBOT_HASH,
                    ref_value: hash,
                });
            }
        }
    }

    if kind_lc == "fixes" {
        for sha in extract_sha_prefixes(raw_trimmed) {
            out.insert(TrailerRef {
                ref_kind: REF_KIND_SHA_PREFIX,
                ref_value: sha,
            });
        }
    }

    if matches!(kind_lc.as_str(), "link" | "closes") {
        for url in extract_urls(raw_trimmed) {
            out.insert(TrailerRef {
                ref_kind: REF_KIND_URL_LC,
                ref_value: url.to_ascii_lowercase(),
            });
            if let Some(mid) = extract_lore_mid_from_url(&url) {
                out.insert(TrailerRef {
                    ref_kind: REF_KIND_LORE_MID,
                    ref_value: mid,
                });
            }
            if let Some(hash) = extract_syzbot_hash_from_url(&url) {
                out.insert(TrailerRef {
                    ref_kind: REF_KIND_SYZBOT_HASH,
                    ref_value: hash,
                });
            }
        }
    }

    out.into_iter().collect()
}

pub fn extract_syzbot_hash_from_email(email: &str) -> Option<String> {
    let email = email.trim().to_ascii_lowercase();
    let local = email.split('@').next()?;
    let hash = local.strip_prefix("syzbot+")?;
    if is_hex_token(hash) {
        return Some(hash.to_owned());
    }
    None
}

pub fn extract_syzbot_hash_from_url(url: &str) -> Option<String> {
    let url_lc = url.trim().to_ascii_lowercase();
    for key in ["extid=", "id="] {
        let Some(idx) = url_lc.find(key) else {
            continue;
        };
        let value = &url_lc[idx + key.len()..];
        let hex: String = value
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if is_hex_token(&hex) {
            return Some(hex);
        }
    }
    None
}

pub fn extract_lore_mid_from_url(url: &str) -> Option<String> {
    let url_lc = url.to_ascii_lowercase();
    let host_idx = url_lc.find("lore.kernel.org/")?;
    let path = &url[host_idx + "lore.kernel.org/".len()..];
    for segment in path.split(['/', '?', '#']) {
        if segment.is_empty() {
            continue;
        }
        let decoded = percent_decode_ascii(segment);
        if decoded.contains('@') {
            let mid = strip_angles(decoded.trim());
            if !mid.is_empty() {
                return Some(mid.to_owned());
            }
        }
    }
    None
}

fn extract_urls(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .filter_map(|tok| {
            let trimmed =
                tok.trim_matches(|c: char| matches!(c, '<' | '>' | '(' | ')' | ',' | ';'));
            if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                return Some(trimmed.to_owned());
            }
            None
        })
        .collect()
}

fn extract_sha_prefixes(raw: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    let mut current = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_hexdigit() {
            current.push(ch.to_ascii_lowercase());
            continue;
        }
        if is_hex_token(&current) {
            out.insert(current.clone());
        }
        current.clear();
    }
    if is_hex_token(&current) {
        out.insert(current);
    }
    out.into_iter().collect()
}

fn is_hex_token(s: &str) -> bool {
    s.len() >= 8 && s.len() <= 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn percent_decode_ascii(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0_usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = bytes[i + 1] as char;
            let lo = bytes[i + 2] as char;
            if hi.is_ascii_hexdigit() && lo.is_ascii_hexdigit() {
                let val = hex_value(hi) * 16 + hex_value(lo);
                out.push(val as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn hex_value(c: char) -> u8 {
    match c {
        '0'..='9' => (c as u8) - b'0',
        'a'..='f' => (c as u8) - b'a' + 10,
        'A'..='F' => (c as u8) - b'A' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_syzbot_hash_from_reported_by_email() {
        let refs = extract_trailer_refs(
            "reported_by",
            "syzbot+ac3c79181f6aecc5120c@syzkaller.appspotmail.com",
        );
        assert!(refs.iter().any(|r| {
            r.ref_kind == REF_KIND_EMAIL
                && r.ref_value == "syzbot+ac3c79181f6aecc5120c@syzkaller.appspotmail.com"
        }));
        assert!(refs.iter().any(|r| {
            r.ref_kind == REF_KIND_SYZBOT_HASH && r.ref_value == "ac3c79181f6aecc5120c"
        }));
    }

    #[test]
    fn extracts_refs_from_lore_and_syzbot_urls() {
        let refs = extract_trailer_refs(
            "link",
            "https://lore.kernel.org/all/%3Cm1%40x%3E/ https://syzkaller.appspot.com/bug?extid=ac3c79181f6aecc5120c",
        );
        assert!(
            refs.iter()
                .any(|r| r.ref_kind == REF_KIND_LORE_MID && r.ref_value == "m1@x")
        );
        assert!(refs.iter().any(|r| {
            r.ref_kind == REF_KIND_SYZBOT_HASH && r.ref_value == "ac3c79181f6aecc5120c"
        }));
    }

    #[test]
    fn extracts_sha_prefixes_from_fixes() {
        let refs = extract_trailer_refs(
            "fixes",
            "deadbeef01234567 (\"ksmbd: initial ACL handling\")",
        );
        assert!(
            refs.iter().any(|r| {
                r.ref_kind == REF_KIND_SHA_PREFIX && r.ref_value == "deadbeef01234567"
            })
        );
    }
}
