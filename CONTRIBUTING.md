# Contributing

## Dev environment

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain stable
git clone https://github.com/mjbommar/kernel-lore-mcp.git
cd kernel-lore-mcp
uv sync
uv run maturin develop --release
cargo build --release --bin kernel-lore-ingest
```

Quality gate every change must pass:

```sh
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo test --lib
ruff format src tests && ruff check src tests
uv run pytest tests/python -q
./scripts/klmcp-doctor.sh             # 9/9 green
./scripts/agentic_smoke.sh local      # zero API cost
```

Live-fire tests (hit real Anthropic / OpenAI APIs, cost a few cents
per run):

```sh
./scripts/agentic_smoke.sh            # both agents
```

## Release process

Releases ship to PyPI as abi3 wheels (linux-x86_64, linux-aarch64,
macos-arm64) + sdist. Users `uv tool install kernel-lore-mcp`. The
git repo carries scripts/, systemd/, and docs/ that are not in the
wheel.

### 1. Pre-flight

- Land everything that's going into the release in `main`.
- Update `CHANGELOG.md`: move `[Unreleased]` → `[<version>] —
  <date>`. SemVer: bump major on breaking changes, minor on
  features, patch on fixes.
- Bump `version` in `pyproject.toml` AND `Cargo.toml`. Both must
  match (maturin enforces).
- Commit: `release: v<version>`.

### 2. TestPyPI dry-run

```sh
rm -rf dist/
uv run maturin build --release --out dist
uv run maturin sdist --out dist
uv run twine upload --repository testpypi dist/*
```

Uses the existing `~/.pypirc` (TestPyPI credential scope). Then
in a throwaway venv:

```sh
uv venv /tmp/klmcp-test --python 3.12
uv pip install --python /tmp/klmcp-test/bin/python \
    --index-url https://test.pypi.org/simple/ \
    --extra-index-url https://pypi.org/simple/ \
    kernel-lore-mcp==<version>
/tmp/klmcp-test/bin/kernel-lore-mcp --help
/tmp/klmcp-test/bin/kernel-lore-ingest --help
```

If the help output renders and both binaries exist, the wheel is
shippable.

### 3. Tag + push

```sh
git tag -a v<version> -m "release: v<version>"
git push origin main
git push origin v<version>
```

The `.github/workflows/release.yml` workflow (fires on tag push):

- builds the three abi3 wheels + sdist,
- publishes to PyPI via OIDC trusted-publisher (no API token),
- creates a GitHub Release with the CHANGELOG section as the
  body and wheels attached.

### 4. Verify

```sh
uv venv /tmp/klmcp-real --python 3.12
uv pip install --python /tmp/klmcp-real/bin/python \
    kernel-lore-mcp==<version>
/tmp/klmcp-real/bin/kernel-lore-mcp --help
```

If this works from a clean venv with no cache, the release landed.

### Hotfix path

If a published release is broken, yank it from PyPI and publish
`<version>+1` immediately. Do not edit history. Do not force-push.

## House style

- Rust: `cargo fmt`, clippy `-D warnings`, tests in `#[cfg(test)]`
  modules co-located with the code. Doctests disabled globally.
- Python: ruff format + ruff lint (rules: E F W I UP B C4 PTH SIM
  TC RUF S, `ignore = S101 B008 E501 TC001-003`). ty for
  type-checking (not mypy).
- Commit messages describe the *why*, not the *what*. The diff
  shows what.
- No authentication of any kind in the server surface — see
  [`CLAUDE.md`](./CLAUDE.md) § "Non-negotiable product constraints."
