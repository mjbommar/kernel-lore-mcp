# Checklist: Commit and Push (Rust)

Rust counterpart to [`../../python/checklists/07-commit.md`](../../python/checklists/07-commit.md).

Atomic commits. Build on every commit. No mixing concerns.

---

## Non-negotiables

- [ ] Full QA pipeline green: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo doc --no-deps`.
- [ ] `Cargo.lock` bumps NOT mixed with feature code.
- [ ] Dep-pin bumps (CLAUDE.md stack pins) get their own commit with rationale in the body.
- [ ] Generated artifacts (wheels, `target/`, indexed data, compressed stores) never committed. Verify with `git diff --staged --name-only`.
- [ ] `perf(...)` commits include committed before/after criterion numbers.

> Source: [`../../../CLAUDE.md`](../../../CLAUDE.md), [`../../python/git.md`](../../python/git.md).

---

## Commit steps

### Pre-flight

- [ ] **Run the full Rust pipeline.**
  ```bash
  cargo fmt --check && \
    cargo clippy --all-targets -- -D warnings && \
    cargo test && \
    cargo doc --no-deps
  ```

- [ ] **If PyO3 glue changed:**
  ```bash
  uv run maturin develop --release && \
    uv run pytest tests/python -v
  ```

### Staging

- [ ] **Stage specific files.** Never `git add -A` / `git add .` — too easy to catch a `target/` leftover or a test artifact.
  ```bash
  git add src/router.rs src/error.rs tests/python/test_router.py
  ```

- [ ] **Review staged diff.**
  ```bash
  git diff --staged
  git diff --staged --name-only
  ```
  Do any of these appear? If yes, un-stage:
  - `target/**`
  - `*.zst`, `*.tantivy`, `*.parquet` (indexed data)
  - `*.whl` (built wheels)
  - `data/**`
  - `.env`, secrets, cursor keys

### Commit-type discipline

- [ ] **One logical change per commit.** If the message contains "and", split.

- [ ] **Dep-pin bumps are their own commit.** Bumping tantivy from 0.25 -> 0.26 is NOT buried inside `feat(router): ...`. It is:
  ```
  deps(tantivy): bump 0.25 -> 0.26 (required for ...)

  tantivy 0.26 gates stemmer behind a feature flag which we never enable.
  Confirmed stemmer stays off via cargo tree -e features.
  No API break in our use (schema.rs, bm25.rs unchanged).
  ```

- [ ] **`Cargo.lock` changes accompany the commit that caused them.** If you bumped a dep, the `Cargo.lock` diff is in the same commit as the `Cargo.toml` diff. If you didn't bump anything, `Cargo.lock` should not appear at all.

- [ ] **Never commit `uv.lock` + `Cargo.lock` together in a feature commit.** Lock bumps are their own commits.

### Message format

- [ ] **`type(scope): imperative description`** under 70 chars.
  ```
  feat(trigram): fst builder with zstd-compressed postings
  fix(router): reject regex queries containing backrefs
  perf(bm25): 3.2x speedup with pre-computed IDF table
  refactor(ingest): extract trailer parsing into trailers.rs
  deps(gix): 0.79 -> 0.81 for revision+parallel features
  docs(rust): add concurrency guide
  ```

- [ ] **Body explains WHY.** What problem did this solve? What alternative was considered? Why this approach?

- [ ] **`perf(...)` body includes criterion numbers.**
  ```
  perf(bm25): 3.2x speedup with pre-computed IDF table

  Criterion: router::bm25 on 100k-doc corpus
  before: 4.21 ms/query (p50), 8.1 ms (p99)
  after:  1.31 ms/query (p50), 2.5 ms (p99)
  speedup: 3.2x p50, 3.2x p99

  Trade-off: +48 MB RAM (IDF table). Budget OK on r7g.xlarge.
  ```

### Secrets

- [ ] **No `.env`, no API keys, no cursor HMAC key.** Re-check `git diff --staged`.

- [ ] **No credentials in test fixtures.** Synthetic only.

### Branch discipline

- [ ] **Feature branches named `feat/<scope>`.** Phase-scoped work uses `phase-N/<topic>`.

- [ ] **Do not push directly to `main` for multi-commit work.** Open a PR.

- [ ] **Never `--no-verify` / `--no-gpg-sign`.** If a pre-commit hook fails, fix the cause.

### Post-commit

- [ ] **`git status` clean.** No stray modified files.

- [ ] **CI runs:** `cargo fmt`, `clippy`, `test`, `doc`, MSRV (1.85), abi3-off build. Wait for green before opening the PR.

### PR body (when applicable)

- [ ] **Summary: 1-3 bullets.** What does this accomplish?
- [ ] **Test plan checklist.** How to verify. Include `cargo bench` commands for perf PRs.
- [ ] **Link to TODO.md phase/item.** Which line is this closing out?

---

## Cross-references

- [`../cargo.md`](../cargo.md)
- [`../../python/git.md`](../../python/git.md)
- [`../../../CLAUDE.md`](../../../CLAUDE.md)
- [`../../../TODO.md`](../../../TODO.md)
- [`../../python/checklists/07-commit.md`](../../python/checklists/07-commit.md)
