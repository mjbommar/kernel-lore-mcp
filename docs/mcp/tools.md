# MCP — tools

v1 surface. All tools read-only, `readOnlyHint: true`, no side
effects.

## `lore_search`

Discovery. Accepts a lei-compatible query, returns hits with
`message_id`, short snippet, and enough metadata to decide whether
to fetch more.

**Input:**
- `query` (string, required) — lei-compatible query. See
  [`query-routing.md`](./query-routing.md).
- `limit` (int, 1–200, default 25).
- `cursor` (string, optional) — opaque continuation token.

**Output (structured):**
```json
{
  "results": [
    {
      "message_id": "<20260414191533.1467353-1-michael.bommarito@gmail.com>",
      "list": "linux-cifs",
      "from": "Michael Bommarito <michael@bommaritollc.com>",
      "subject": "[PATCH 0/3] ksmbd: three response-side hardenings",
      "date": "2026-04-14T19:15:33Z",
      "snippet": "...cover letter excerpt...",
      "has_patch": false,
      "score": 7.21,
      "lore_url": "https://lore.kernel.org/linux-cifs/20260414191533.1467353-1-michael.bommarito@gmail.com/"
    }
  ],
  "nextCursor": "opaque-string-or-null",
  "query_tiers_hit": ["metadata", "bm25"],
  "freshness": {
    "oldest_list_last_updated": "2026-04-14T19:25:10Z",
    "blind_spots": ["security@kernel.org queue", "distro backports"]
  }
}
```

## `lore_thread`

Pull a full conversation by any message-id within it.

**Input:**
- `message_id` (string, required).
- `include_patches` (bool, default true).

**Output:**
- Thread root + chronological list of replies.
- Each message: full headers, body (prose + patch concatenated),
  `lore_url`.

## `lore_patch`

Fetch patch text by message-id. Useful when `lore_search` returns
a hit with `has_patch: true` and the caller wants the actual
diff.

**Input:**
- `message_id` (string, required).

**Output:**
- `patch` (string) — raw diff text.
- `touched_files`, `touched_functions`.
- Parent commit if derivable.

## `lore_activity`

Who has touched a file or function recently. The anti-fishing
signal from the parent project — surface it first-class.

**Input:**
- `file` (string, optional) — exact path match, e.g.
  `fs/smb/server/smbacl.c`.
- `function` (string, optional) — exact identifier, e.g.
  `smb_check_perm_dacl`.
- `since` (string, ISO8601 or relative like `30d`, default `90d`).
- `lists` (list of strings, optional) — restrict to lists.

One of `file` or `function` is required.

**Output:**
```json
{
  "touches": [
    {
      "message_id": "...",
      "list": "linux-cifs",
      "from": "Greg Kroah-Hartman <gregkh@linuxfoundation.org>",
      "date": "2026-04-06T10:22:00Z",
      "subject": "...",
      "lore_url": "..."
    }
  ],
  "distinct_authors": 4,
  "total_touches": 11,
  "interpretation": "Area saturated; 5+ distinct researchers in 90d."
}
```

## `lore_message`

Fetch a single message by message-id. Lowest-level primitive.

**Input:**
- `message_id` (string, required).
- `include_headers` (bool, default true).

**Output:**
- Full RFC822 as decoded UTF-8 text, plus parsed field dict.

## Conventions shared across tools

- **Snake_case tool names**, short, <64 chars.
- **`readOnlyHint: true`** on every tool.
- **Opaque cursor pagination**, never offset/limit for LLM callers.
- **Structured `content` blocks** in MCP responses, not stringified
  JSON. Clients render them.
- **Include `lore_url`** on every message reference — a link the
  human-in-the-loop can click.
- **Include `freshness` block** on discovery responses so LLM
  callers can qualify their claims.
- **Include `blind_spots` list** on every response so LLM callers
  don't overclaim coverage.
