# Research — PyO3 + maturin (April 14 2026)

## Decision

**PyO3 0.28.3** + **maturin 1.13.1**, abi3-py312 floor, mixed
layout (Rust in `src/`, Python in `python/`).

## Key facts (from PyO3 0.28 CHANGELOG, Feb–April 2026)

- MSRV bumped to Rust 1.83.
- **Free-threaded Python support is now opt-out by default**
  (PR #5564). Assume your module is thread-safe unless you
  explicitly opt out.
- **abi3 subclassing** for native types (PyDict, exceptions, PyList)
  landed on Python 3.12+ (PRs #5733, #5734). Nice to have.
- PEP-489 multi-phase initialization is the default for `#[pymodule]`
  — enables subinterpreters later.
- **abi3 does NOT yet work with free-threaded builds.** PEP 803
  ("abi3t" stable ABI for free-threaded) is open but not shipped.
  If abi3 is requested under a free-threaded interpreter, PyO3
  warns and falls back to non-abi3. This means:
  - v1: ship abi3 wheels that cover 3.12/3.13/3.14 standard builds.
  - Free-threaded support is a separate non-abi3 wheel job.

## Layout

Mixed (maturin's documented default):
```
pyproject.toml
Cargo.toml
src/lib.rs
python/kernel_lore_mcp/__init__.py
```

`[tool.maturin]` declares `python-source = "python"` and
`module-name = "kernel_lore_mcp._native"`.

## Releasing the GIL — `Python::detach` / `Python::attach`

PyO3 0.28 **renamed** the two foundational GIL APIs:
  * `Python::with_gil` → `Python::attach` (PR #5209).
  * `Python::allow_threads` → `Python::detach` (PR #5221).

Both names are present in 0.28.3 stable (verified on docs.rs). The
old names exist for a deprecation window; new code must use the
new names. Do NOT touch any `Py<T>` or `Bound` inside a `detach`
block. rayon works fine inside a detached section — workers run as
free OS threads; if they need to call back into Python they
re-`attach`.

An earlier reviewer claimed `detach` was only in 0.29 master — that
was wrong. Verified against `v0.28.3/CHANGELOG.md` and the
`pyo3::marker::Python` docs page on docs.rs.

## abi3 floor

We pick **py312** because:
- 3.10/3.11 are EOL soon; kernel contributors' distros are moving.
- abi3-py312 unlocks the subclassing wins landed in 0.28.
- Covers 3.12/3.13/3.14 with one wheel.

## Testing

- `cargo test` for pure Rust logic.
- `pytest` + `maturin develop` for PyO3 glue, via `uv run`.
- Separate CI job for free-threaded 3.14t with a non-abi3 build.

## CI

`PyO3/maturin-action@v1` with sccache enabled, matrix over
{linux, macos} × {x86_64, aarch64}. Single abi3 wheel per OS/arch.

## Sources

- [pyo3 0.28.3 CHANGELOG](https://github.com/PyO3/pyo3/blob/main/CHANGELOG.md)
- [pyo3 free-threading guide](https://pyo3.rs/main/free-threading.html)
- [PEP 803 — abi3t stable ABI for free-threaded](https://peps.python.org/pep-0803/)
- [maturin docs — project layout](https://www.maturin.rs/project_layout)
- [Python 3.14 what's new](https://docs.python.org/3/whatsnew/3.14.html)
- [Nandann — PyO3 v0.28 + maturin](https://www.nandann.com/blog/rust-pyo3-python-extensions-guide)
