# pyo3 0.28.3

Rust counterpart to `../../python/pyo3-maturin.md`. The Python
doc is authoritative on the shared build/boundary contract; this
doc covers the Rust-side idioms we commit to.

Pinned: `pyo3 = "0.28"` with `extension-module` feature. abi3
floor is `py312` by default; free-threaded `python3.14t` builds
require `--no-default-features`.

---

## Headline 0.28 changes (vs 0.24-ish muscle memory)

1. **`allow_threads` → `detach`, `with_gil` → `attach`** (PRs
   #5209, #5221, shipped in 0.28). These are the current names.
   **Do not write `allow_threads` / `with_gil` in new code.**
   Old tutorials will steer you wrong.
2. **Inline-mod `#[pymodule]`** is the preferred form. The old
   `#[pymodule] fn _core(m: &Bound<PyModule>) -> PyResult<()>`
   still works but is not what we write today.
3. **`Bound<'py, T>`** is the lifetime-parametric GIL-bound
   reference; `Py<T>` is the owning, 'static handle. 0.28
   completed the migration away from `PyRef` / direct `&PyAny`
   in public APIs.
4. **abi3 floor is py312 in this project.** abi3 doesn't
   support the free-threaded build; PEP 803's "abi3t" is
   pending. Our `default = ["abi3"]` feature in `Cargo.toml`
   gates this.

---

## The module root — inline-mod form

Our `lib.rs`:

```rust
use pyo3::prelude::*;

mod bm25;
mod error;
mod ingest;
mod metadata;
mod router;
mod schema;
mod state;
mod store;
mod trigram;

#[pymodule]
mod _core {
    use super::*;

    #[pyfunction]
    fn version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    // Future: #[pyfunction]s for lore_search, lore_thread, etc.
    // #[pyclass]es for Hit, Cursor, etc.
}
```

The Python side imports as `kernel_lore_mcp._core`. That path is
baked by `Cargo.toml`:

```toml
[lib]
name = "_core"
crate-type = ["cdylib", "rlib"]
```

### Why inline-mod

- Pyclasses and pyfunctions register in declaration order by
  the macro, with no manual `m.add_class::<...>()` list.
- Works with `#[pymodule_init]` when we need a one-shot setup
  (we don't yet).
- Matches the shape that pyo3 0.28's `#[pymodule]` test suite
  exercises — fewer foot-guns.

### Module-path declaration

If we ever make `_core` a submodule of another module for
bundling, the 0.28 form uses `#[pymodule(submodule)]`. We don't
today.

---

## `#[pyclass]` vs `#[pyfunction]`

**`#[pyfunction]`** — free function callable from Python. Our
default. Every MCP tool implementation is a pyfunction.

**`#[pyclass]`** — Rust struct with Python-visible
attributes/methods. Use when:

- You return a compound type that Python code needs to inspect
  (our `Hit` will be a pyclass).
- You need the type to be pickleable (`__reduce__` via
  `#[pymethods]` `__getstate__` / `__setstate__`).
- Python code calls methods on it across multiple
  pyfunction calls.

Don't use pyclass for:

- Values that are only passed in, never out.
- Collections that Python would rather see as native `list` /
  `dict`.
- Transient error types. Use `PyErr` via `From<Error> for
  PyErr`.

Naming: **`Py` prefix on pyclass types** (`PyHit`, `PyCursor`)
so the Rust reader sees "this is Python-facing" at the
declaration. The Python side re-exports without the prefix via
`__init__.py` if we want a clean name.

```rust
#[pyclass(name = "Hit", module = "kernel_lore_mcp._core")]
pub struct PyHit {
    #[pyo3(get)] message_id: String,
    #[pyo3(get)] cite_key: String,
    // ...
}
```

Explicit `name = "Hit"` keeps the Python-visible type name
clean; `module = "..."` is set so pickle can find it.

---

## `Bound<'py, T>` vs `Py<T>`

- **`Bound<'py, T>`** — GIL-bound reference. Use for arguments
  coming in, for building results before returning. Can't
  outlive the GIL token `py: Python<'py>`. This is what you
  write 90% of the time.
- **`Py<T>`** — owning, 'static. Use when you need to store a
  Python object past a GIL drop (e.g., in a struct field that
  will be used in a later pyfunction call).

Rule of thumb in our code:

```rust
#[pyfunction]
fn foo<'py>(py: Python<'py>, input: &Bound<'py, PyDict>) -> PyResult<...> {
    // `input` is Bound — fine, we have `py`.
    let s: String = input.get_item("query")?.extract()?;
    // Drop GIL, do work
    let out = py.detach(|| pure_rust_work(&s))?;
    Ok(out)
}
```

Don't return `Bound<'py, T>` unless the caller cares about the
lifetime. Return owned `String` / `Vec<T>` / `#[pyclass]`
instances; pyo3 converts.

---

## `Python::detach` — the GIL discipline

Every `#[pyfunction]` that does non-trivial work (>100 µs,
as a rule of thumb) detaches the GIL before doing it:

```rust
#[pyfunction]
fn search(py: Python<'_>, data_dir: &str, query: &str) -> PyResult<Vec<PyHit>> {
    let dir = std::path::PathBuf::from(data_dir);
    let q   = query.to_owned();
    let hits = py.detach(|| -> crate::Result<Vec<Hit>> {
        router::search(&dir, &q)
    })?;
    Ok(hits.into_iter().map(PyHit::from).collect())
}
```

Rules:

1. **Convert inputs to owned Rust types first.** Can't hold a
   `&Bound<'_, PyAny>` across `detach`.
2. **The closure returns pure Rust types.** No `Py<T>`, no
   `Bound<'_, T>`.
3. **Reattach (implicitly, after the closure) to build the
   return value.** Conversions of the owned Rust results into
   PyO3 types require the GIL; they happen after `detach`.
4. **Don't nest detach.** If you're already detached and need
   Python, use `Python::attach` — but in practice we return
   and let the caller reattach.

The old names still compile via deprecation shims in 0.28; new
code uses `detach` / `attach`.

### GIL-safety static assert

For a `#[pyfunction]` that spawns rayon work, we can prove we
didn't leak a Python handle into the pool:

```rust
fn _prove_send<T: Send>(_: &T) {}
py.detach(|| {
    _prove_send(&some_value);   // compile-checks we're not holding PyAny
    rayon_work(&some_value)
});
```

---

## `PyResult<T>` and error conversion

`PyResult<T>` is `Result<T, PyErr>`. Our library functions
return `crate::Result<T>`; `?` converts via the
`impl From<Error> for PyErr` in `src/error.rs`. See
`../design/errors.md`.

Don't return `anyhow::Result` from a pyfunction. `From<anyhow::Error>
for PyErr` would flatten to `RuntimeError` on everything,
losing the `ValueError` vs `RuntimeError` distinction.

---

## Pickle support (when we need it)

Pyclasses are not pickleable by default. If a `Hit` needs to
round-trip through `pickle` (MCP rarely needs this but some
clients do via `mcp.server` queueing):

```rust
#[pymethods]
impl PyHit {
    fn __getstate__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = serde_json::to_vec(&self.inner)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        Ok(PyBytes::new_bound(py, &bytes))
    }

    fn __setstate__(&mut self, state: &Bound<'_, PyBytes>) -> PyResult<()> {
        self.inner = serde_json::from_slice(state.as_bytes())
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        Ok(())
    }

    // pyo3 0.28 convention: classes that support pickle need
    // __reduce_ex__ support, which pyo3 derives from the above.
}
```

Store the picklable representation as `serde_json::Vec<u8>` —
forward-compatible with schema evolution. Don't pickle opaque
binary representations of internal state.

---

## Stub files — `.pyi`

Every `#[pyfunction]` and `#[pyclass]` has a matching stub in
`src/kernel_lore_mcp/_core.pyi`. Current file is 234 bytes; it
will grow as tools are added.

Shape:

```python
# src/kernel_lore_mcp/_core.pyi
from __future__ import annotations

def version() -> str: ...

class Hit:
    message_id: str
    cite_key: str
    # ... #[pyo3(get)] fields
    def __init__(self, *args: object, **kwargs: object) -> None: ...
```

Rules:

- Stub every public surface. Type checkers (mypy, pyright,
  basedpyright) consume these.
- Keep the file in lockstep with the Rust declarations. CI
  should diff (TODO).
- Don't duplicate docstrings — the Rust `///` comments become
  `__doc__` at import time, which type checkers already see.

---

## Threading and free-threaded Python

- abi3 build (our default) does **not** run on free-threaded
  3.14t. The wheel name marks it (`cp312-abi3`).
- To build for 3.14t: `maturin build --no-default-features`.
  PEP 803's "abi3t" isn't landed; when it is, we'll add an
  `abi3t` feature gate.
- Our Rust side has no GIL-yielding deadlocks; `Python::detach`
  + rayon is safe under either build.

See `../design/concurrency.md` for the rayon + GIL
interaction.

---

## Common 0.28 footguns

| Symptom | Cause | Fix |
|---|---|---|
| `cannot find macro allow_threads in scope` | Wrong version or old tutorial. | Use `detach`. |
| `#[pymodule] fn` compiles but Python import fails. | Old function-style form, works but we prefer inline-mod. | Convert to `#[pymodule] mod _core { ... }`. |
| `Bound<'_, PyAny>` held across `detach`. | Compile error — and correctly so. | Extract to owned first. |
| `#[pyclass]` inherits `!Send`. | pyclass has `Py<T>` fields, making it !Send. | Keep pyclass state as pure Rust; convert at boundary. |
| `PyErr: Send` fails when you try to send a `PyErr` into a rayon task. | PyErr captures GIL state. | Translate errors to `crate::Error` before detach; only map back after. |
| pickle round-trip loses data. | `#[pyo3(get)]` without a setter; default `__init__` doesn't populate. | Implement `__getstate__` / `__setstate__` explicitly. |

---

## Checklist for adding a new `#[pyfunction]`

1. Pure-Rust function in the right sibling module
   (`router`/`ingest`/...).
2. Thin `#[pyfunction]` in `lib.rs` that:
   - Converts inputs.
   - Calls `py.detach(|| ...)`.
   - Returns `PyResult<T>`.
3. Stub entry in `src/kernel_lore_mcp/_core.pyi`.
4. Error paths exercised: a test that provokes
   `Error::QueryParse` and asserts `ValueError` on the Python
   side (in `tests/python/`).
5. Benchmark the Rust core via `criterion` (see
   `../testing.md`).

See also:
- `../design/boundaries.md` — thin-wrapper rule.
- `../design/errors.md` — `From<Error> for PyErr`.
- `../design/concurrency.md` — detach + rayon.
- `../../python/pyo3-maturin.md` — shared build/boundary spec.
