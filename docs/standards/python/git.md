# Git Workflow and Commit Standards

Adapted from `../../../../../273v/kaos-modules/docs/python/git.md`.

Consistent, frequent commits with clear messages create a readable
project history. Every commit is atomic, buildable, and descriptive.

See also: [Rust counterpart](../rust/git.md) for scope names on
Rust-side changes.

---

## Commit Frequency

**Commit early and often.** A commit captures a coherent unit of work
— not a day's work, not a feature branch's worth of changes.

Good commit cadence:
- After each logical change passes the QA pipeline (`ruff format →
  ruff check → ty check → pytest`).
- After fixing a bug (with the test that proves the fix).
- After adding a new tool, model, or route (with its tests).
- After refactoring that changes structure.
- Before switching context to a different task.

Bad commit cadence:
- One massive commit at the end of the day with "various changes".
- Committing broken code that does not pass QA.
- Holding changes for days waiting for "everything to be done".

---

## Commit Messages

### Format

```
<type>(<scope>): <description>

<body — optional, explain WHY not WHAT>

Co-Authored-By: ...
```

Our commit summaries are **conventional-ish**: we use the `type(scope):
description` shape but do not enforce it with a hook. The discipline
is in the reviewer's eye, not the toolchain.

### Types

| Type | When to Use |
|------|-------------|
| `feat` | New functionality, new tool, new module |
| `fix` | Bug fix — something was broken, now it works |
| `refactor` | Code restructuring with no behavior change |
| `test` | Adding or improving tests |
| `docs` | Documentation changes only |
| `perf` | Performance improvement (include before/after numbers) |
| `build` | Build system, dependencies, CI configuration |
| `chore` | Routine maintenance (version bumps, config tweaks) |
| `pin` | Deliberate dep-pin change (requires rationale in body) |

### Scope

The scope identifies the subsystem. Use short names that map to our
package layout:

| Scope | Area |
|-------|------|
| `server` | `src/kernel_lore_mcp/server.py` — FastMCP assembly |
| `tools` | `src/kernel_lore_mcp/tools/*` — MCP tool handlers |
| `router` | `src/router.rs` + `src/kernel_lore_mcp/router.py` |
| `ingest` | `src/ingest.rs` + `src/bin/reindex.rs` |
| `bm25` | `src/bm25.rs` — tantivy tier |
| `trigram` | `src/trigram.rs` — fst+roaring tier |
| `metadata` | `src/metadata.rs` — Arrow/Parquet tier |
| `store` | `src/store.rs` — compressed raw store |
| `schema` | `src/schema.rs` — shared Arrow + tantivy schemas |
| `models` | `src/kernel_lore_mcp/models.py` — pydantic responses |
| `config` | `src/kernel_lore_mcp/config.py` |
| `cursor` | HMAC cursor payload |
| `routes` | `/status`, `/metrics` |
| `pyo3` | `src/lib.rs` / `src/error.rs` — FFI boundary |
| `docs` | `docs/**` |
| `ci` | `.github/workflows/*` |
| `deps` | `pyproject.toml`, `Cargo.toml` dep bumps |

Examples:

```
feat(tools): add lore_patch_diff with cursor-based chunked responses
fix(router): reload reader when generation advances mid-query
perf(trigram): cap candidate set before regex confirm (15x speedup)
refactor(models): extract TierProvenance into a discriminated union
test(ingest): add multi-byte subject round-trip fixture
docs(python): add pyo3-maturin standards for 0.28 detach/attach
build(pyo3): pin to 0.28.3 with abi3-py312
pin(tantivy): bump to 0.26.0 for WithFreqs IndexRecordOption
```

### Description

- Imperative mood: "add feature" not "added feature" or "adds feature".
- Lowercase first letter after the `type(scope):` prefix.
- No period at the end.
- Under 70 characters.
- Describe what the commit **does**, not what you did.

### Body

Use the body to explain **why** the change was made. The diff shows
what. Include:

- Performance numbers for `perf` commits (before / after / speedup).
- Context for non-obvious design decisions.
- References to related commits, issues, or docs.
- Breaking changes or migration notes.
- Rationale for any dep pin bump (we don't bump casually — the pin
  table in `CLAUDE.md` is authoritative).

```
perf(trigram): cap candidate set before regex confirm

Before: 180 ms/query (pathological /CVE-\d{4}/ over stable tree)
After:   12 ms/query
Speedup: 15x

The trigram tier was returning the full posting-list intersection
before handing off to regex-automata for confirmation. Capping the
candidate set at 4096 (via TRIGRAM_CANDIDATE_CAP) catches 99% of
real queries in 2 orders of magnitude less work, and queries that
exceed the cap fall back to BM25 with a note in tier_provenance.
```

```
pin(pyo3): bump 0.27.1 -> 0.28.3

0.28 renamed `allow_threads` -> `detach` and `with_gil` -> `attach`
(PRs pyo3#5209, pyo3#5221). This is the "free-threaded ergonomics"
release. All new Rust code uses the new names; existing callers
updated in this commit. See docs/standards/python/pyo3-maturin.md.

abi3-py312 floor preserved; no wheel-matrix change.
```

---

## Atomic Commits

Each commit represents **one logical change**. If you find yourself
writing "and" in the commit summary, split into two commits.

```bash
# Bad — two unrelated changes in one commit
git commit -m "fix(router): reload reader, and add lore_activity tool"

# Good — split into two commits
git commit -m "fix(router): reload reader when generation advances mid-query"
git commit -m "feat(tools): add lore_activity aggregation tool"
```

Atomic commits let `git bisect` do its job and let reviewers reason
about one thing at a time.

---

## What to Commit

### Always Commit

- Source code changes with their corresponding tests.
- `pyproject.toml` changes (dep additions, config changes).
- `Cargo.toml` / `Cargo.lock` changes.
- `uv.lock` updates (after `uv lock`).
- Documentation updates in `docs/`.
- Configuration files (`.ruff.toml` if we grow one, `rust-toolchain.toml`).
- `scripts/*.sh` changes.
- `.github/workflows/*` changes.

### Never Commit

- `.env` files or any file containing secrets.
- API keys, tokens, passwords, cursor keys (use `SecretStr` and
  environment variables).
- `.venv/` directories.
- `__pycache__/`, `.pyc` files.
- Build artifacts (`target/`, `dist/`, `build/`, `*.egg-info/`,
  `*.so` compiled by maturin inside the source tree).
- IDE settings (`.idea/`, `.vscode/` — unless shared team config).
- `node_modules/` (we don't have any, keep it that way).
- OS files (`.DS_Store`, `Thumbs.db`).
- **Compressed lore stores and indices.** `data/`, `*.tantivy`,
  `*.zst`, anything under an `indices/` directory. The deploy box
  builds these; they never live in git. `.gitignore` catches them.
- **Fetched lore shards.** `grok-pull` output stays on the deploy box.

### Staging Discipline

Stage specific files, not everything:

```bash
# Good — explicit files
git add src/router.rs src/kernel_lore_mcp/router.py tests/python/unit/test_router.py

# Acceptable — when you know the directory is clean
git add src/kernel_lore_mcp/tools/ tests/python/integration/

# Dangerous — can catch secrets, binaries, temp files
git add -A
git add .
```

If you must `git add -A`, run `git status` first and eyeball every
line.

---

## Branch Strategy

### Main Branch

`main` is the primary development branch. It is always in a buildable,
testable state. CI runs the full QA pipeline on every push.

### Feature Branches

For non-trivial work (multi-commit features, risky refactors,
experiments):

```bash
git checkout -b feat/lore-activity-tool
# ... work, commit, push ...
# Open a PR when ready
```

Branch naming: `<type>/<short-description>` using the same type
vocabulary as commit messages (`feat/`, `fix/`, `perf/`, `refactor/`,
`docs/`, `pin/`).

### Pushing

```bash
# Push current branch and set upstream
git push -u origin HEAD

# Never force-push to main.
# Force-push to feature branches only when rebasing — with-lease:
git push --force-with-lease origin feat/lore-activity-tool
```

**Never force-push to `main`** without explicit agreement. The project
is small but public; rewriting history breaks external forks and any
scheduled grokmirror operator who pinned an SHA.

---

## Pull Requests

### PR Title

Same format as commit messages: `<type>(<scope>): <description>`.

### PR Body

```markdown
## Summary
- Bullet points describing what changed and why

## Test plan
- [ ] Unit tests pass: `uv run pytest tests/python -m unit -v`
- [ ] Integration tests pass against tiny shards:
      `uv run pytest tests/python -m integration -v`
- [ ] Rust tests pass: `cargo test --no-default-features`
- [ ] QA pipeline clean:
      `uv run ruff format --check`, `uv run ruff check`,
      `uv run ty check`
```

### Merge Strategy

- **Squash merge** for feature branches that accumulated
  work-in-progress commits. One clean commit on `main`.
- **Rebase merge** for branches where every commit is individually
  meaningful and belongs in history as-is.

Do not create merge commits on `main`. Linear history is easier to
bisect and easier to explain.

---

## Pre-Commit Verification

Before every commit, run the QA pipeline:

```bash
# For a mixed (Rust + Python) change:
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --no-default-features

uv run maturin develop --release

uv run ruff format src/kernel_lore_mcp tests/python
uv run ruff check --fix src/kernel_lore_mcp tests/python
uv run ty check src/kernel_lore_mcp tests/python
uv run pytest tests/python -v

# Then commit
git add src/router.rs src/kernel_lore_mcp/router.py tests/python/unit/test_router.py
git commit -m "fix(router): reload reader when generation advances mid-query"
```

Do not use `--no-verify` to skip hooks. If a hook fails, fix the issue,
re-stage, and create a **new** commit — **do not** `git commit --amend`
after a hook failure. When a pre-commit hook fails, the commit did not
actually happen; `--amend` would silently modify the *previous*
commit, which can lose work.

---

## Commit Hygiene

### Do Not Commit Broken Code

Every commit should pass the QA pipeline. If you need to save
work-in-progress, use `git stash` rather than committing broken code.

### Do Not Amend Published Commits

Once a commit has been pushed to a shared branch, do not amend it.
Create a new commit. Amending published history causes problems for
anyone who has pulled the branch.

### Keep Rust + Python Changes Paired When They Are Logically One

When a single logical change touches both sides of the PyO3 boundary
(e.g. adding a new `#[pyfunction]` and its `.pyi` stub and the Python
wrapper and a test), commit them **together**. Splitting "add Rust
function" from "use Rust function in Python" leaves an intermediate
commit where the function is unused (clippy warns) or called but not
defined (build breaks). Atomicity beats granularity here.

### Document Pin Bumps

Every change to a pinned dep in the CLAUDE.md pin table requires:

1. A `pin(<scope>):` commit type.
2. A body explaining **why** the bump was made.
3. A note about whether downstream code changed in the same commit
   or in a follow-up.

Example:

```
pin(fastmcp): bump 3.1.0 -> 3.2.4

3.2 ships the Streamable HTTP transport stabilization and drops
the SSE code path we were never going to use (SSE deprecated
Apr 1 2026). No API changes on our surface.

Verified: test_tool_contracts.py, test_structured_content.py all
green. Server starts cleanly on both stdio and Streamable HTTP.
```

---

## Cross-references

- [code-quality.md](code-quality.md) — the QA pipeline every commit
  must pass
- [testing.md](testing.md) — unit vs integration vs live markers
- [uv.md](uv.md) — `uv lock` discipline before committing `uv.lock`
- [Rust counterpart](../rust/git.md) — scope names for Rust-side
  commits
