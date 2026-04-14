# Ingestion — patch parsing

A kernel patch is a unified diff embedded in an email body,
starting at `^diff --git a/<path> b/<path>`. We don't need to
*apply* patches — we need to extract:

- Touched files (left + right, usually identical).
- Touched functions (via `@@ ... @@ <func>` hunk headers when
  git emits them, which it does when `diff.orderFile` or the
  function context heuristic triggers).
- Hunk bodies (fed to the trigram tier).

## Format (what we care about)

```
diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c
index 1234abc..5678def 100644
--- a/fs/smb/server/smbacl.c
+++ b/fs/smb/server/smbacl.c
@@ -123,6 +123,9 @@ int smb_check_perm_dacl(struct ksmbd_conn *conn, ...)
 {
        ...
+       if (ace_size < sizeof(struct smb_ace))
+               return -EINVAL;
        ...
 }
```

Extract:
- `touched_files` += `fs/smb/server/smbacl.c`
- `touched_functions` += `smb_check_perm_dacl` (from `@@ ... @@ <func>`)
- Whole patch text (including hunk bodies) → trigram tier

## Parser approach

Simple line scanner, not a full patch library:

```rust
const DIFF_GIT: &[u8] = b"diff --git ";
const HUNK_HDR: &[u8] = b"@@ ";

for line in body.lines() {
    if line.starts_with(DIFF_GIT) {
        // parse "a/<path> b/<path>"; push both (dedup)
    } else if line.starts_with(HUNK_HDR) {
        // parse "@@ -123,6 +123,9 @@ <func>"; extract <func> if present
    }
}
```

Edge cases:

- **Rename diffs** (`rename from ... rename to ...`) — record both.
- **Binary diffs** (`Binary files ... differ`) — record file, skip
  hunk body.
- **Multi-file patches** — common, handled by continuing the scan.
- **No function context** — `@@ ... @@` without trailing identifier.
  Leave `touched_functions` empty for that hunk.
- **Series with inline reply quoting** (`> diff --git`) — quoted
  reply prefix was already stripped in mbox-parsing phase.

## Function name heuristic

When git emits `@@ -a,b +c,d @@ <context>`, `<context>` is
whatever line the C pretty-printer picked. It's usually a function
signature but can be a struct body, a `#define`, etc. We extract
the first identifier-like token and let the trigram index carry
the truth. False positives here are cheap — the metadata tier is
for narrowing, not for proof.

## What NOT to extract at ingest

- Parsed AST per file. We don't have the file context; the patch
  only shows hunks.
- Semantic change classification (bug fix vs refactor). v2+.
- Full file paths post-rename (touched_files captures both sides;
  good enough).
