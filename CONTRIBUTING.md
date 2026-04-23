# Contributing

## Dev environment

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain stable
git clone https://github.com/mjbommar/kernel-lore-mcp.git
cd kernel-lore-mcp
uv sync
uv run maturin develop --release
cargo build --release \
    --bin kernel-lore-sync \
    --bin kernel-lore-reindex \
    --bin kernel-lore-doctor
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

Releases currently ship to PyPI as:

- one manylinux2014 x86_64 abi3 wheel
- one source distribution

Users install with `uv tool install kernel-lore-mcp`. The git repo
still carries scripts/, systemd/, and docs/ that are not in the
wheel.

### 1. Pre-flight

- Land everything that's going into the release in `main`.
- Update `CHANGELOG.md`: move `[Unreleased]` → `[<version>] —
  <date>`. SemVer: bump major on breaking changes, minor on
  features, patch on fixes.
- Bump `version` in `pyproject.toml` AND `Cargo.toml`. Both must
  match (maturin enforces).
- Bump `src/kernel_lore_mcp/__init__.py` too.
- If the wheel-shipped helper binaries changed, rebuild the bundled
  `src/kernel_lore_mcp/bin/kernel-lore-{sync,reindex,doctor}` copies
  before packaging.
- Commit: `release: v<version>`.

### 2. Build + local validation

```sh
rm -rf dist/
cargo test --all-targets
uv run maturin develop --release
uv run pytest tests/python -q
uv build --sdist --out-dir dist
uv run --with "maturin[zig]>=1.13,<2" \
    maturin build --release --compatibility manylinux2014 --zig -o dist
uv run twine check dist/*
```

Then validate the built artifacts in a throwaway environment:

```sh
uv venv /tmp/klmcp-test --python 3.12
uv pip install --python /tmp/klmcp-test/bin/python dist/*.whl
/tmp/klmcp-test/bin/kernel-lore-mcp --help
/tmp/klmcp-test/bin/kernel-lore-sync --version
/tmp/klmcp-test/bin/kernel-lore-reindex --version
/tmp/klmcp-test/bin/kernel-lore-doctor --version
```

If the console scripts resolve and the helper CLIs run, the wheel is
shippable.

### 3. Tag + push

```sh
git tag -a v<version> -m "release: v<version>"
git push origin main
git push origin v<version>
```

### 4. Publish to PyPI + GitHub

This repo uses the local `~/.pypirc` credentials and manual release
commands:

```sh
uv publish --publish-url https://upload.pypi.org/legacy/ dist/*
gh release create v<version> dist/* --notes-file /tmp/klmcp-release-notes.txt
```

The GitHub release notes can be the matching `CHANGELOG.md` section.

### 5. Verify from public artifacts

```sh
uv venv /tmp/klmcp-real --python 3.12
uv pip install --python /tmp/klmcp-real/bin/python \
    kernel-lore-mcp==<version>
/tmp/klmcp-real/bin/kernel-lore-mcp --help
/tmp/klmcp-real/bin/kernel-lore-sync --version
/tmp/klmcp-real/bin/kernel-lore-reindex --version
/tmp/klmcp-real/bin/kernel-lore-doctor --version
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
