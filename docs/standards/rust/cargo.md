# Cargo — Dependencies, Features, Profiles

Rust counterpart to [`../python/uv.md`](../python/uv.md). Cargo is
to Rust what uv is to Python in this project: the single source of
truth for dependencies, resolution, and build profiles.

---

## The rule

Every dependency change goes through Cargo. No hand-edited
`Cargo.toml` shortcuts that skip resolution. No ambient `cargo
install` of crates that should be `dev-dependencies`.

| Instead of... | Use... |
|---|---|
| edit `Cargo.toml` by hand, cross fingers | `cargo add <crate>` |
| `cargo install criterion` | `[dev-dependencies]` + `cargo run --bench` |
| guessing a crate's features | `cargo add <crate> --features <f>` |
| global `cargo update` | `cargo update -p <pkg>` with a reason |
| "I'll just pull in this helper crate" | Evaluate against the dependency bar below |

---

## Single crate, not workspace

kernel-lore-mcp is a **single-crate** project. One `Cargo.toml`, one
lib target (cdylib + rlib), one binary target, one dev-dep set. We do
NOT use `[workspace]` because the Python extension and its native core
ship as a single artifact.

Adding a workspace would require a clear reason — for example,
factoring out a pure-Rust `klmcp-ingest` crate that the binary and
ingest systemd unit both depend on. That's a project decision.

See the actual manifest:
[`/home/mjbommar/projects/personal/kernel-lore-mcp/Cargo.toml`](../../../Cargo.toml).

---

## Manifest anatomy

The live `Cargo.toml` is the source of truth; this section describes
why each section exists so reviewers can spot unjustified additions.

### `[package]`

```toml
[package]
name = "kernel_lore_mcp"
version = "0.1.0"
edition = "2024"
rust-version = "1.88"     # MSRV — enforced by CI
```

- `name` uses `snake_case` — see [`naming.md`](naming.md). The
  library artifact is named `_core` (see `[lib]` below) because
  Python imports it as `kernel_lore_mcp._core`.
- `rust-version` is the MSRV. `cargo check --locked` fails on an
  older toolchain.

### `[lib]`

```toml
[lib]
name = "_core"
crate-type = ["cdylib", "rlib"]
```

- `cdylib` produces the `.so` that Python loads as
  `kernel_lore_mcp._core`. Required for PyO3 extension modules.
- `rlib` lets the `reindex` binary link against the same code without
  going through Python. Both artifacts share one build.

### `[features]`

```toml
default = ["abi3"]
abi3 = ["pyo3/abi3-py312"]
```

Our feature set is minimal by design:

- `abi3` is ON by default. We build against the stable Python ABI
  (3.12 floor) so one wheel covers 3.12/3.13/3.14.
- Free-threaded Python 3.14t requires `--no-default-features`. `abi3`
  is incompatible until PEP 803 "abi3t" lands in pyo3.
- Do NOT add feature flags for "maybe someday" functionality. Every
  feature is a combinatorial explosion for testing.

When to add a feature:

1. Two consumers genuinely need different subsets of the code
   (e.g., reindex binary vs. Python extension — handled already).
2. Disabling the feature eliminates a heavy dependency at compile
   time.

Not when:

- You want "experimental" code that might get deleted. Put it on a
  branch instead.
- You want per-environment tuning — use runtime config, not compile
  features.

### `[dependencies]`

Every entry in the current manifest has a reason. Examples:

```toml
# Python bindings. 0.28.3 stable has Python::detach / Python::attach
# (renamed from allow_threads/with_gil in pyo3 PRs #5209, #5221).
pyo3 = { version = "0.28", features = ["extension-module"] }

# Gix, NOT git2-rs. See docs/research/2026-04-14-gix-vs-git2.md.
gix = { version = "0.81", default-features = false, features = [
    "max-performance-safe",
    "revision",
    "parallel",
] }

# Tantivy. Stemmer feature INTENTIONALLY omitted — see CLAUDE.md.
tantivy = { version = "0.26", default-features = false, features = ["mmap"] }
```

Rules:

- **Set `default-features = false` when we only need a subset.** It
  has already paid for itself on `gix` (no
  `blocking-network-client`), `mail-parser`, `tantivy`, `arrow`, and
  `parquet`.
- **List features explicitly.** Comments on non-obvious ones. Future
  you will not remember why `full_encoding` is on `mail-parser`.
- **Pin to major.minor where the ecosystem is drifting.** `pyo3 =
  "0.28"`, `tantivy = "0.26"`, `gix = "0.81"` are fixed by the
  project stack. See [`../../../CLAUDE.md`](../../../CLAUDE.md).

### `[dev-dependencies]`

```toml
proptest = "1"
tempfile = "3"
criterion = { version = "0.5", features = ["html_reports"] }
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

`dev-dependencies` do not ship to consumers. Put QA tooling that's
actually used in `cargo test` or `cargo bench` here — never in
runtime `[dependencies]`. See [`testing.md`](testing.md).

### `[[bin]]`

```toml
[[bin]]
name = "reindex"
path = "src/bin/reindex.rs"
```

One binary: rebuild indices from the compressed store. It uses
`anyhow` for error handling (see [`language.md`](language.md)) and
shares the library's `_core` rlib for all the heavy lifting.

Add another `[[bin]]` only for another genuinely standalone tool. The
MCP server is Python (FastMCP), not a Rust binary — do NOT add a bin
target for "the server."

### `[[bench]]`

Benchmarks live in `benches/*.rs` and are declared:

```toml
[[bench]]
name = "router"
harness = false   # criterion provides its own main
```

See [`testing.md`](testing.md) for the criterion workflow. No
benches are committed today; add them when a perf claim needs
evidence.

### `[profile.*]`

```toml
[profile.release]
lto = "thin"
codegen-units = 1
opt-level = 3
debug = 1          # minimal line-table for perf tooling
panic = "abort"
strip = "symbols"
```

- `lto = "thin"` buys 5–15% on tight loops for ~2x link time. We
  accept that.
- `codegen-units = 1` for predictable autovectorization. Release is
  not incremental anyway.
- `debug = 1` keeps line-table data so `perf`, `flamegraph`, and
  `cargo flamegraph` produce useful output.
- `panic = "abort"` because a panic in the Rust core is unrecoverable
  — we do not want unwinding to race with the Python interpreter.
- `strip = "symbols"` trims the wheel size without losing line tables
  (which are in a separate section).

Dev profile stays defaults. Bench profile inherits release.

---

## Cargo commands we use

### `cargo add`

```bash
# Runtime dep
cargo add ahash

# Runtime dep with features, no defaults
cargo add arrow --no-default-features --features "ipc"

# Dev dep
cargo add --dev insta

# Optional dep behind a feature
cargo add foo --optional
# ...then wire it into [features]:  feature-name = ["dep:foo"]
```

Always review the resulting `Cargo.toml` diff — `cargo add` can pick
a more permissive version than you intended.

### `cargo tree`

```bash
# Show the full tree
cargo tree

# Who depends on `serde`?
cargo tree -i serde

# What features does `tantivy` resolve with?
cargo tree -e features -p tantivy

# Spot duplicates (two versions of the same crate)
cargo tree --duplicates
```

Duplicates are the biggest source of binary bloat and "why is my
wheel 40 MB." Use `[patch.crates-io]` (below) to unify when sensible.

### `cargo update`

```bash
# Never do this without a reason
cargo update

# Update one package, pin the reason in the commit message
cargo update -p regex-automata

# Update to the exact version you want
cargo update -p pyo3 --precise 0.28.3
```

Pin bumps to stack versions (pyo3, tantivy, gix) must cite a reason
in the commit message — "upstream fix", "required for free-threaded
abi3t", etc. Random `cargo update` on a quiet afternoon is how stack
drift happens.

### `cargo-hack`

Feature-matrix CI. Not yet in CI but worth knowing:

```bash
cargo install cargo-hack

# Build every feature combination
cargo hack build --feature-powerset --no-dev-deps

# Test with no default features (free-threaded abi3t path)
cargo hack test --no-default-features
```

When we gain a second non-trivial feature flag, add `cargo hack` to
CI. See [`code-quality.md`](code-quality.md).

### `cargo-deny`

License and advisory gating:

```bash
cargo install cargo-deny
cargo deny check advisories  # RUSTSEC
cargo deny check licenses    # GPL-3 etc. blocked
cargo deny check bans        # duplicate-version detection
```

`deny.toml` lives at the repo root (add when CI gains this step).
See [`code-quality.md`](code-quality.md) for the full pipeline.

### `cargo-audit`

Lighter-weight alternative to `cargo deny check advisories`:

```bash
cargo install cargo-audit
cargo audit
```

Either is acceptable; don't run both in CI — pick one.

---

## `[patch]` — version unification

When two dependencies pull in different versions of the same crate,
binary size and compile time balloon. Use `[patch.crates-io]` only
when:

1. `cargo tree --duplicates` shows a version duplicate in `release`
   builds, AND
2. The crates are API-compatible at the versions in play.

```toml
# Example only — do NOT commit this without a duplicate to solve.
[patch.crates-io]
parking_lot = "0.12"
```

Do NOT use `[patch]` to pin to a fork. If a fork is required, that's
a project-level decision and should be called out in
`CLAUDE.md`.

---

## The dependency bar

Before `cargo add`, answer yes to all:

1. Can we do this in <200 lines of our own code? If yes, do that.
2. Is the crate well-maintained (recent commits, recent release)?
3. Is its feature set narrow enough to enable just what we need via
   `default-features = false`?
4. Does it expose a `no_std` / sync / narrow-purpose core, or does it
   drag in async runtimes and HTTP clients?
5. Is the license MIT / Apache-2.0 / BSD-compatible? (cargo-deny
   enforces.)

Rejected examples (logged in `docs/research/`):

- **`git2`** — not `Sync`, drags libgit2 C dep. Use `gix`.
- **`reqwest` anywhere in the core** — Python owns HTTP.
- **`tokio` inside the core** — rayon for data parallelism,
  async stays on the Python side.

---

## Editing the manifest by hand

Sometimes `cargo add` can't express what you need (optional deps tied
to features, `[lib]` changes). When editing by hand:

1. Make the change.
2. Run `cargo check` — surfaces typos immediately.
3. Run `cargo tree` to confirm resolution.
4. Commit `Cargo.toml` AND `Cargo.lock` together.

Never edit `Cargo.lock` by hand. Ever.

---

## CI configuration

```bash
#!/usr/bin/env bash
set -euo pipefail

# Lockfile sanity — fails if Cargo.lock is stale.
cargo check --locked --all-targets

# MSRV check with the exact pinned toolchain.
cargo +1.88 check --locked

# Full QA (see code-quality.md)
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo doc --no-deps --locked
```

The `--locked` flag is non-negotiable in CI. Without it, a CI run can
silently resolve to a different dependency set than a developer saw.

---

## Cross-references

- [`index.md`](index.md) — standards index.
- [`../python/uv.md`](../python/uv.md) — Python counterpart.
- [`code-quality.md`](code-quality.md) — full QA pipeline including
  `cargo-deny` / `cargo-audit`.
- [`testing.md`](testing.md) — `[dev-dependencies]`,
  `[[bench]]` usage.
- [`../../../CLAUDE.md`](../../../CLAUDE.md) — authoritative stack
  pins and rationale.
