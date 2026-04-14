# Checklist: Commit and Push

Adapted from `.../kaos-modules/docs/python/checklists/07-commit.md`.

Atomic commits. Conventional-ish messages. Nothing generated in
the index.

---

## Non-negotiables

- [ ] **Pipeline passed.** Every step of [05-quality.md](05-quality.md).
      No `--no-verify`.
- [ ] **No secrets, no generated artifacts.** `.gitignore` already
      catches `data/`, `target/`, `*.tantivy`, `*.zst`, `*.so`,
      `*.parquet`, `.env`. Verify the staged set anyway.
- [ ] **One logical change per commit.** If the subject contains
      "and", split.

---

## Items

- [ ] **Stage specific files.** `git add <path1> <path2>`. Avoid
      `git add -A` / `git add .` — they sweep up scratch files,
      editor detritus, and `.env` escapes.

- [ ] **Inspect the staged set.**
      ```bash
      git status
      git diff --staged --name-only
      git diff --staged
      ```
      Read every line of the diff. Last chance before history.

- [ ] **Verify no generated files are staged.** Nothing in
      `data/`, `target/`, no `*.tantivy`, `*.zst`, `*.so`,
      `*.parquet`. Also: no `uv.lock` churn unless you meant it,
      no `Cargo.lock` churn without a real dep change.

- [ ] **Write a conventional-ish message.** Format:
      `type(scope): description`. Imperative, under 70 chars.
      Body explains WHY.
      ```
      feat(router): reject phrase-on-prose with Error::QueryParse
      fix(ingest): full-rewalk fallback when last_oid is dangling
      perf(trigram): 3.2x speedup via roaring bitmap intersection
      docs(mcp): document HMAC cursor key rotation
      chore(deps): bump tantivy 0.26.0 -> 0.26.1 (security fix)
      ```
      Scopes: `router`, `ingest`, `trigram`, `bm25`, `metadata`,
      `store`, `mcp`, `tools`, `server`, `ops`, `docs`, `tests`,
      `deps`.

- [ ] **Include performance numbers for `perf(...)` commits.**
      Before/after, input size, speedup. No numbers -> no perf
      claim.
      ```
      perf(trigram): 3.2x speedup via roaring bitmap intersection

      Before: 41.2 ms/query (v6.8 shard, `dfa:skb_unlink`)
      After:  12.8 ms/query (same)
      Measured with cargo bench, criterion default settings.
      ```

- [ ] **Reference CLAUDE.md / TODO.md** when a commit unblocks or
      closes a listed item. Use `TODO: ...` lines for items not
      yet done; remove lines that are.

- [ ] **Update `uv.lock` when `pyproject.toml` deps changed.**
      Include the lockfile in the same commit. Same rule for
      `Cargo.lock`.

- [ ] **Live tests pass** when the change touches the router,
      ingest, or any query path. Unit-only green is not enough
      for those areas.

- [ ] **No `--no-verify`, no `--no-gpg-sign`.** If a pre-commit
      hook fails, fix the code. If a hook is wrong, fix the hook
      in a separate commit first.

- [ ] **Branching.** Feature work on `feat/<short-desc>` or
      `fix/<short-desc>`. Don't pile multi-commit work on `main`.

- [ ] **PR body follows the template.** Summary bullets + test
      plan checklist. Link to the `TODO.md` item the change
      closes.
