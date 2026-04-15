"""Keyword-in-context (KWIC) snippet extraction.

A populated `Snippet` gives agents a verifiable excerpt around the
matched needle — byte offset + sha256 over the source text + up to
`window` chars of context. Without this, search responses look like
free-text (unverifiable) to the LLM and to the human reviewing the
transcript.

Provenance model: `sha256` is computed over the *full source string*
the snippet was extracted from (patch body, subject line, trailer
value). Offset + length are byte-accurate against that same string,
so `source[offset : offset + length]` reproduces the snippet text
byte-for-byte.

Behaviour on no-match: returns `None`. We prefer the Snippet field
stay empty over fabricating an offset of 0 (which would suggest the
needle appears at the head).
"""

from __future__ import annotations

import hashlib

from kernel_lore_mcp.models import Snippet

_DEFAULT_WINDOW = 200


def extract_kwic(
    source: str,
    needle: str,
    *,
    window: int = _DEFAULT_WINDOW,
    case_insensitive: bool = False,
) -> tuple[int, int, str] | None:
    """Return `(offset, length, excerpt)` for the first `needle` in `source`.

    `offset` + `length` are byte offsets into the source's UTF-8 encoding,
    suitable for reconstruction via `source.encode('utf-8')[offset:offset + length]`.
    The excerpt is centered on the match, expanded to `window` total bytes,
    and clipped to word boundaries where possible.
    """
    if not needle or not source:
        return None

    hay = source.lower() if case_insensitive else source
    nee = needle.lower() if case_insensitive else needle
    char_idx = hay.find(nee)
    if char_idx < 0:
        return None

    source_bytes = source.encode("utf-8")
    prefix_bytes = source[:char_idx].encode("utf-8")
    needle_bytes = source[char_idx : char_idx + len(needle)].encode("utf-8")
    match_offset = len(prefix_bytes)
    match_len = len(needle_bytes)

    half = max(0, (window - match_len) // 2)
    start = max(0, match_offset - half)
    end = min(len(source_bytes), match_offset + match_len + half)
    while start > 0 and (source_bytes[start] & 0xC0) == 0x80:
        start -= 1
    while end < len(source_bytes) and (source_bytes[end] & 0xC0) == 0x80:
        end += 1
    excerpt = source_bytes[start:end].decode("utf-8", errors="replace")
    return start, end - start, excerpt


def build_snippet(
    source: str,
    needle: str,
    *,
    window: int = _DEFAULT_WINDOW,
    case_insensitive: bool = False,
) -> Snippet | None:
    """Convenience: run `extract_kwic` and package as a `Snippet`.

    `sha256` is over the source's UTF-8 bytes, letting a caller verify
    the snippet against the original string end-to-end.
    """
    kwic = extract_kwic(
        source,
        needle,
        window=window,
        case_insensitive=case_insensitive,
    )
    if kwic is None:
        return None
    offset, length, text = kwic
    digest = hashlib.sha256(source.encode("utf-8")).hexdigest()
    return Snippet(offset=offset, length=length, sha256=digest, text=text)


def build_snippet_from_body(
    body: bytes | None,
    needle: str,
    body_sha256: str | None,
    *,
    window: int = _DEFAULT_WINDOW,
    case_insensitive: bool = False,
) -> Snippet | None:
    """Populate a Snippet from a decompressed message body.

    Prefers the ingest-time `body_sha256` when provided (round-trips to
    the store), falls back to a fresh digest over the decoded text so
    the caller still gets verifiable provenance on bodies fetched by
    other paths.
    """
    if body is None:
        return None
    try:
        text = body.decode("utf-8")
    except UnicodeDecodeError:
        text = body.decode("latin-1", errors="replace")

    kwic = extract_kwic(
        text,
        needle,
        window=window,
        case_insensitive=case_insensitive,
    )
    if kwic is None:
        return None
    offset, length, excerpt = kwic
    digest = body_sha256 or hashlib.sha256(body).hexdigest()
    return Snippet(offset=offset, length=length, sha256=digest, text=excerpt)


__all__ = ["build_snippet", "build_snippet_from_body", "extract_kwic"]
