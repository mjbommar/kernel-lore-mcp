# Ingestion — message parsing

## Parser

`mail-parser` 0.11 (Stalwart Labs). Handles RFC822 + MIME + weird
kernel-list encodings (mixed 8bit, koi8-r, windows-1252 from
older subsystems).

```rust
use mail_parser::MessageParser;
let msg = MessageParser::default().parse(bytes)
    .ok_or_else(|| anyhow!("unparseable"))?;
let from = msg.from();                  // Option<Addr>
let subj = msg.subject();               // Option<&str>
let date = msg.date();                  // Option<DateTime>
let mid  = msg.message_id();            // Option<&str>
let body = msg.body_text(0).unwrap_or_default();
```

## Field extraction

| Output column | Source |
|---|---|
| `message_id` | `Message-ID` header, angle-bracket-stripped |
| `list` | derived from shard path (`<list>/git/<N>.git`) |
| `from_addr` / `from_name` | `From:` parsed address |
| `subject_raw` | verbatim `Subject:` (stored) |
| `subject_normalized` | strip `[PATCH...]`, `Re:`, `Fwd:`, zero+ levels; collapse whitespace |
| `date` | `Date:` header; fall back to commit author date if unparseable |
| `in_reply_to` | `In-Reply-To:` header |
| `references` | `References:` split on whitespace |
| `touched_files`, `touched_functions` | extracted from patch (see [patch-parsing.md](./patch-parsing.md)) |
| `has_patch` | true if any `^diff --git` found |

## Prose / patch split

The prose/patch split is the single most important pre-tokenization
step. Our contract:

1. Find the first line matching `^diff --git ` (anchored, literal).
   Everything before it (minus quoted replies and signature) is
   prose.
2. If no `diff --git` appears, whole body is prose.
3. Prose goes to the BM25 tier. Patch goes to the trigram tier.
   **They never mix** — mixing is what makes kernel BM25 search
   terrible.
4. `Subject:` goes to both: metadata (normalized) + BM25 as a
   short field.

### Pre-BM25 prose scrubbing

- Strip lines matching `^>+ ` (quoted replies).
- Strip from first `^-- $` (signature delimiter, RFC 3676) to EOF.
- Collapse whitespace.
- Do NOT strip `Reviewed-by:` / `Signed-off-by:` trailers from the
  body — they're signal when someone searches for a reviewer. But
  also extract them to structured fields (v2).

## Encodings

- `mail-parser` decodes transfer encodings (quoted-printable,
  base64) automatically.
- Charset conversion: prefer UTF-8 output; fall back to
  `encoding_rs` for legacy 8-bit charsets if `mail-parser`
  returns bytes instead of str.
- Normalize to NFC before indexing.

## Thread reassembly

Not done at ingest — purely a query-time concern. We store
`in_reply_to` + `references`; the `lore_thread` tool walks those
at query time. Keeping it at query time means a new message
joining an old thread doesn't require reindexing the old messages.
