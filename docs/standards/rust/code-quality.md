# Code Quality — fmt, clippy, test, docs

Rust counterpart to [`../python/code-quality.md`](../python/code-quality.md).

Every change to the Rust side of kernel-lore-mcp goes through the
same four-step gate: **format -> lint -> test -> docs**. All four
pass before committing. No exceptions.

---

## The pipeline

```bash
# Step 1 — format
cargo fmt --all -- --check

# Step 2 — lint
cargo clippy --all-targets --all-features -- -D warnings

# Step 3 — test
cargo test --locked

# Step 4 — docs (catches broken links and missing examples)
cargo doc --no-deps --locked
```

Run in this order. Format before lint avoids false clippy positives
on layout. Clippy before tests catches issues faster than the test
run. `cargo doc` last because it's the slowest and the least likely
to fail.

For the Python side of the build, see
[`../python/code-quality.md`](../python/code-quality.md). The mixed
pre-commit script at the bottom of this file runs both.

---

## cargo fmt

`rustfmt` with project defaults. We do NOT keep a `rustfmt.toml`
because the default edition-2024 config matches what we want.

```bash
# Format in place
cargo fmt --all

# Check without modifying (CI)
cargo fmt --all -- --check
```

Rules:

- **Format before committing.** Always.
- **Do NOT disable rustfmt with `#[rustfmt::skip]` except on visibly
  tabular data** — the tokenizer fingerprint table is a legitimate
  case; most others are not.
- **Do not merge with `// rustfmt: ignore` sections.** If rustfmt
  produces something ugly, it's usually a sign the code needs
  restructuring.

---

## cargo clippy

All lints fail CI. We run with `-D warnings` so there are no
"warnings acceptable" shortcuts.

```bash
# Lint with fixes applied (interactive)
cargo clippy --fix --allow-dirty --allow-staged --all-targets

# Strict mode (CI)
cargo clippy --all-targets --all-features -- -D warnings
```

### Lint groups we enable

`cargo clippy` defaults (`correctness`, `suspicious`, `style`,
`complexity`, `perf`) plus:

```rust
// crate root — src/lib.rs
#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![allow(clippy::module_name_repetitions)]   // PyFoo in foo.rs is fine
#![allow(clippy::missing_errors_doc)]        // Err variants explained on Error enum
```

`pedantic` and `nursery` have noise. The allow-list above is the
accepted noise. If you hit a new `pedantic` lint that's wrong for our
code, add it to the allow-list *in a commit that explains why* —
never in an ad-hoc `#[allow(...)]` on a call site.

### Per-item allows

Occasionally a single call site genuinely needs an allow. Pattern:

```rust
// SAFETY / REASON: roaring's bitmap builder requires u32;
// docids are bounded by u32::MAX by schema contract.
#[allow(clippy::cast_possible_truncation)]
let docid = offset as u32;
```

Rule: every `#[allow]` carries a one-line comment saying why. No
bare allows.

### The lints we care about most

| Lint | Why it matters for this project |
|------|---------------------------------|
| `clippy::unwrap_used` | Ingestion runs unattended. An unwrap in library code crashes the ingest systemd unit. |
| `clippy::panic` | Same. Panics abort the process (we set `panic = "abort"` in release). |
| `clippy::cast_*` | We shuffle between `u32` docids and `usize` offsets. Size bugs here are silent data corruption. |
| `clippy::needless_collect` | Ingest hot loops. A bad `.collect::<Vec<_>>()` doubles memory. |
| `clippy::redundant_clone` | Same. |
| `clippy::match_same_arms` | Our Error -> PyErr mapping grows over time; fold duplicate arms. |

`clippy::unwrap_used` is allowed in `#[cfg(test)]` and
`tests/` — production code must not `.unwrap()`. See
[`language.md`](language.md).

---

## cargo test

See [`testing.md`](testing.md) for test organization, proptest, and
criterion. For the QA pipeline:

```bash
# Run all tests including ignored ones that are fast-enough to run
cargo test --locked

# Single file / module
cargo test --lib router::

# Run against the no-default-features path (free-threaded abi3t)
cargo test --no-default-features --locked
```

`--locked` ensures CI sees the same resolution as developers. See
[`cargo.md`](cargo.md).

FFI glue (PyO3 boundary) is **tested from Python**, not `cargo test`.
See [`../python/testing.md`](../python/testing.md) for
`tests/python/` conventions and `fastmcp.Client` integration tests.

---

## cargo doc

```bash
cargo doc --no-deps --locked
```

- `--no-deps` skips documenting transitive dependencies (they already
  have docs).
- `cargo doc` fails on broken intra-doc links (`[Foo]` pointing to
  nothing). That's good — it catches renames early.
- Every `pub` item in the Rust core (`src/*.rs`, excluding
  `src/bin/*.rs`) has a doc comment. Enforced informally by review;
  formally by `#![warn(missing_docs)]` once we are past scaffolding.

Doc examples (`/// ```rust ... ```  blocks`) are compiled by
`cargo test`. Keep them minimal and focused — they are documentation,
not a test suite.

---

## Optional, recommended checks

These are NOT yet in CI; run them locally before a release tag. Add
to CI when the project stabilizes past v0.

### cargo-deny

```bash
cargo install cargo-deny
cargo deny check
```

Covers:

- `advisories` — RUSTSEC advisories on current dep versions.
- `licenses` — rejects incompatible licenses (GPL-3, AGPL, SSPL).
- `bans` — flag duplicate versions of the same crate in the
  resolution graph.
- `sources` — enforce crates.io + vetted git registries only.

`deny.toml` lives at the repo root once this step lands. Template:

```toml
[licenses]
allow = ["MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Unicode-3.0", "Zlib"]
confidence-threshold = 0.93

[bans]
multiple-versions = "warn"   # "deny" once we audit duplicates
wildcards = "deny"

[advisories]
yanked = "deny"
```

### cargo-audit

Lighter alternative. Pick one of cargo-deny / cargo-audit for CI, not
both.

```bash
cargo install cargo-audit
cargo audit
```

### MSRV check

```bash
# With the exact pinned toolchain
cargo +1.85 check --locked --all-targets
```

CI runs this on every PR. MSRV is `1.85` — bumping it is a project
decision (see [`cargo.md`](cargo.md)).

### cargo-hack (feature matrix)

```bash
cargo install cargo-hack
cargo hack test --feature-powerset --no-dev-deps
```

Worth adding when we have a second non-trivial feature flag.
Currently our feature set is `default = ["abi3"]` only.

### miri (for unsafe code)

Required reading alongside [`unsafe.md`](unsafe.md). If you add
`unsafe`, you run miri:

```bash
rustup toolchain install nightly
cargo +nightly miri test
```

Our policy is "no unsafe"; if that changes, miri is a CI gate, not an
optional check.

---

## Pre-commit script

Drop this in `scripts/pre-commit.sh` (create if missing) and wire via
`git config core.hooksPath scripts/git-hooks` or run manually before
every commit:

```bash
#!/usr/bin/env bash
set -euo pipefail

echo "=== cargo fmt ==="
cargo fmt --all -- --check

echo "=== cargo clippy ==="
cargo clippy --all-targets --all-features -- -D warnings

echo "=== cargo test ==="
cargo test --locked

echo "=== cargo doc ==="
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked

echo "=== python: ruff format ==="
uv run ruff format --check src/kernel_lore_mcp tests/

echo "=== python: ruff lint ==="
uv run ruff check src/kernel_lore_mcp tests/

echo "=== python: ty ==="
uv run ty check src/kernel_lore_mcp tests/

echo "=== python: pytest ==="
uv run pytest tests/python -v --tb=short
```

Keep the Rust side first — it's faster to fail clippy than to wait
out a pytest run.

---

## CI workflow (`.github/workflows/ci.yml` sketch)

```yaml
jobs:
  rust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: "1.85"
          components: rustfmt, clippy
      - run: cargo fmt --all -- --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
      - run: cargo test --locked
      - run: cargo doc --no-deps --locked
        env:
          RUSTDOCFLAGS: "-D warnings"

  rust-advisories:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: "1.85"
      - run: cargo install cargo-deny --locked
      - run: cargo deny check
```

The Python job runs in parallel; see
[`../python/code-quality.md`](../python/code-quality.md).

---

## Handling clippy findings

Order of preference:

1. **Fix the code.** Most clippy lints point at real problems.
2. **Rewrite for clarity.** If clippy is wrong-ish but the code is
   also ugly, the code was the problem.
3. **Add a local `#[allow]` with a one-line reason.** Only if 1 and 2
   fail.
4. **Suppress the lint globally** in `src/lib.rs` — requires a commit
   message explaining why.

Never use `#[allow]` without a comment. Never silence a lint to
unblock a merge.

---

## Cross-references

- [`index.md`](index.md) — standards index.
- [`cargo.md`](cargo.md) — cargo commands, features, profiles.
- [`testing.md`](testing.md) — unit/integration/proptest/criterion.
- [`unsafe.md`](unsafe.md) — miri requirements.
- [`../python/code-quality.md`](../python/code-quality.md) — Python
  counterpart (ruff, ty).
