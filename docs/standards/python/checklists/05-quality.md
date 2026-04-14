# Checklist: Code Quality

Adapted from `.../kaos-modules/docs/python/checklists/05-quality.md`.

Every change must pass the full pipeline before committing. No
shortcuts. No `--no-verify`.

---

## Non-negotiables

- [ ] **`ty`, not `mypy`.** Astral's type checker is the project
      standard. Any reference to `mypy` in a new file is a bug.
- [ ] **`uv run` wraps every Python command.** Bare `python`,
      `pip`, `poetry`, `conda` are wrong answers.
- [ ] **The Rust side is a first-class citizen.** `cargo fmt`,
      `cargo clippy`, `cargo test` are part of the pipeline,
      not afterthoughts.

---

## The pipeline, in order

- [ ] **Build the extension.**
      ```bash
      uv run maturin develop
      ```
      Do this after any Rust change — otherwise tests run against
      a stale `_core.so`.

- [ ] **Format Python.**
      ```bash
      uv run ruff format src/kernel_lore_mcp tests/python
      ```
      > Ref: [../code-quality.md](../code-quality.md)

- [ ] **Lint Python.**
      ```bash
      uv run ruff check --fix src/kernel_lore_mcp tests/python
      ```
      Review remaining warnings — they are real issues.

- [ ] **Type-check Python.**
      ```bash
      uv run ty check src/kernel_lore_mcp tests/python
      ```
      Use `# ty: ignore[rule]` only with a justifying comment on
      the same line.

- [ ] **Format Rust.**
      ```bash
      cargo fmt --all
      ```

- [ ] **Lint Rust at the highest bar.**
      ```bash
      cargo clippy --all-targets -- -D warnings
      ```
      Warnings are errors. No `#[allow(...)]` without a comment.

- [ ] **Rust tests.**
      ```bash
      cargo test
      ```
      Includes unit tests and integration tests under `tests/`.

- [ ] **Python tests.**
      ```bash
      uv run pytest -v
      ```
      Unit + any integration tier relevant to the change. Live
      tests gated by marker.

---

## Beyond the pipeline

- [ ] **Type annotations on all public functions.** Everything
      exported from `kernel_lore_mcp.__init__` or crossed across
      modules.

- [ ] **Pydantic patterns.** `ConfigDict(frozen=True)` on
      response models, `SecretStr` for secrets,
      `model_validator(mode="before")` for settings normalization.
      > Ref: [../libraries/pydantic.md](../libraries/pydantic.md)

- [ ] **No bare `except:` or `except Exception: pass`.** Every
      handler re-raises, logs with context, or produces a
      three-part error message.
      > Ref: [../design/errors.md](../design/errors.md)

- [ ] **No `os.environ` in library code.** Settings are loaded at
      the edge (`__main__.py` / `server.py`) and passed in.
      > Ref: [../design/boundaries.md](../design/boundaries.md)

- [ ] **Import discipline.** stdlib -> third-party -> local. Lazy
      imports for `_core` and heavy deps. `TYPE_CHECKING` for
      type-only imports. No sibling-tool imports.
      > Ref: [../design/dependencies.md](../design/dependencies.md)

- [ ] **No new dep introduced casually.** `pyproject.toml` and
      `Cargo.toml` versions are pinned on purpose. Bumping
      requires an explicit justification in the commit message.

- [ ] **No debug artifacts.** `breakpoint()`, `print()`, `dbg!()`
      left behind. Grep before staging.

- [ ] **Stubs updated.** If Rust surface changed, `_core.pyi`
      matches. If it doesn't, `ty check` will miss real errors
      downstream.

- [ ] **Every `# noqa`, `# ty: ignore`, `#[allow]`, `clippy::allow`
      has a comment** explaining why.
