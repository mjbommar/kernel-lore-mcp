# PyO3 FFI — The Rust Side of the Boundary

Shared contract with [`../python/pyo3-maturin.md`](../python/pyo3-maturin.md).
Read that first: the three-layer architecture, cost model, and
Python-side rules are authoritative there. This document covers the
Rust-side implementation details.

PyO3 version is pinned at **0.28.3** in `Cargo.toml`. The 0.28 series
renamed `allow_threads` / `with_gil` to `Python::detach` /
`Python::attach` (pyo3 PRs #5209, #5221). **Never write
`allow_threads` or `with_gil` in new code.** Old PyO3 tutorials and
answers online will lead you wrong.

---

## The three-layer architecture

```
┌─────────────────────────────────────────────────┐
│  Layer 3 — Python wrappers                       │
│  src/kernel_lore_mcp/*.py                       │
│  pydantic models, typed returns,                │
│  FastMCP tool definitions                       │
├─────────────────────────────────────────────────┤
│  Layer 2 — PyO3 glue                             │
│  src/lib.rs + #[pyfunction]/#[pyclass] sections │
│  Type conversion, GIL release, error mapping    │
├─────────────────────────────────────────────────┤
│  Layer 1 — Pure Rust core                        │
│  src/{router,ingest,trigram,bm25,...}.rs       │
│  No PyO3 types, testable with cargo test        │
└─────────────────────────────────────────────────┘
```

Rules:

- **Layer 1 does not import `pyo3::*`.** Not even `PyErr`. The core
  returns `Result<T, Error>` where `Error` is our crate's error type.
- **Layer 2 is thin.** Convert, call, convert back, map errors. If
  Layer 2 grows business logic, extract it to Layer 1.
- **Layer 3 is where pydantic models live.** Pydantic never crosses
  into Rust — the Rust side sees plain Python types (str, int, list,
  dict, None).

See [`src/lib.rs`](../../../src/lib.rs) for our `#[pymodule]` root
and the module-declaration style we use:

```rust
#[pymodule]
mod _core {
    use super::*;

    #[pyfunction]
    fn version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}
```

The "multi-phase init" style (`#[pymodule] mod _core`) is pyo3 0.28's
recommended pattern. Do NOT use the legacy
`#[pymodule] fn _core(py: Python, m: &PyModule)` form.

---

## GIL discipline — detach / attach

### The rename (0.28)

| Old (pre-0.28) | New (0.28+) | Meaning |
|----------------|-------------|---------|
| `py.allow_threads(\|\| { ... })` | `py.detach(\|\| { ... })` | release the GIL for a Rust block |
| `Python::with_gil(\|py\| { ... })` | `Python::attach(\|py\| { ... })` | acquire the GIL from a Rust thread |

**Never write `allow_threads` or `with_gil` in this project.**
Reviewers will reject. The names are identical in meaning but the new
names correctly describe what's happening under free-threaded Python
(where there is no single GIL — you're detaching from / attaching to
the interpreter's "thread state," not a lock).

### When to release the GIL

Release (detach) whenever the Rust block does real work — parsing,
searching, decompressing, regex-matching.

```rust
#[pyfunction]
fn router_search(py: Python<'_>, query: &str) -> PyResult<Vec<PyHit>> {
    // Accept the Python string, then do the work with the GIL released.
    let parsed = router::parse_query(query)?;
    let hits = py.detach(|| -> Result<Vec<Hit>, Error> {
        router::search(&parsed)
    })?;
    Ok(hits.into_iter().map(PyHit::from).collect())
}
```

Rules:

- **`detach` the heavy work.** Parsing, searching, decompression,
  regex — all release the GIL.
- **Don't detach trivial work.** The rule of thumb is >1 us. Under
  that, you pay two state transitions to save nothing.
- **Never hold a `Bound<'py, T>` across `detach`.** The compiler
  enforces this (the closure's `'static` bound excludes `'py`),
  which is the whole point of the API. If you need data inside the
  closure, extract it to an owned Rust value first.

### Acquiring the GIL from a background thread

Ingestion does not re-enter Python (it's its own systemd unit), but
if a future feature needs to call back into Python from a rayon
thread:

```rust
rayon::spawn(|| {
    // ... heavy Rust work, no GIL ...
    Python::attach(|py| {
        // now we have the GIL, talk to Python
    });
});
```

Re-entering Python from a rayon pool is a legitimate perf footgun —
callbacks serialize on the GIL. Prefer finishing the Rust work and
returning bulk results instead.

---

## Handle types — `Py<T>` vs `Bound<'py, T>` vs `Borrowed`

PyO3 0.28 has three handle types. Pick by lifetime.

| Handle | Lifetime | When |
|--------|----------|------|
| `Bound<'py, T>` | tied to a `Python<'py>` token | the normal case — arguments, locals, returns within a single call |
| `Py<T>` | `'static` | store across calls (inside a `#[pyclass]`, in a long-lived registry) |
| `Borrowed<'a, 'py, T>` | tied to the borrow | rare — zero-cost borrow from a `Py<T>` without a clone |

### `Bound<'py, T>` — the default

```rust
#[pyfunction]
fn count_lines(text: Bound<'_, PyString>) -> PyResult<usize> {
    Ok(text.to_str()?.lines().count())
}
```

`Bound<'py, T>` replaces the old `&PyAny` / `&PyString` borrow style.
It's a smart handle that tracks the Python lifetime. The `'_` is
sugar for "inferred `'py`".

### `Py<T>` — store across calls

Use inside `#[pyclass]` fields, or when a registry outlives a single
Python call:

```rust
#[pyclass(module = "kernel_lore_mcp._core")]
pub struct PyRouter {
    inner: Arc<Router>,               // pure Rust — no GIL needed
    config_obj: Py<PyDict>,           // pure Python — needs GIL to touch
}

impl PyRouter {
    fn read_config<'py>(&self, py: Python<'py>) -> Bound<'py, PyDict> {
        self.config_obj.bind(py).clone()  // materialize Bound from Py
    }
}
```

Call `.bind(py)` to materialize a `Py<T>` into a `Bound<'py, T>`
when you need to work with it.

### `Borrowed<'_, 'py, T>` — advanced

Zero-cost view into a `Py<T>` without a clone, for functions that
accept either. Rare in our code — reach for it only when a profiler
points here.

### Ownership rule of thumb

- Rust-only state inside a `#[pyclass]` is an `Arc<_>` of the pure
  Rust type, NOT a `Py<PyClass>` of itself.
- Python objects stored across calls live in `Py<T>`.
- Locals live in `Bound<'py, T>`.

---

## Byte-to-char offset conversion

**Critical and easy to get wrong.** Rust string operations are in
bytes; Python string offsets are in *characters* (USVs). Returning a
byte offset to Python is a bug that only surfaces with non-ASCII
text.

Lore has plenty of non-ASCII — patches on CJK symbols, emoji in
commit messages, accented names in trailers.

Pattern (also covered in
[`../python/pyo3-maturin.md`](../python/pyo3-maturin.md)):

```rust
/// Build a byte-offset -> char-offset lookup.
fn build_byte_to_char_table(text: &str) -> Vec<usize> {
    let mut table = Vec::with_capacity(text.len() + 1);
    let mut char_idx = 0usize;
    for (byte_idx, _) in text.char_indices() {
        while table.len() <= byte_idx {
            table.push(char_idx);
        }
        char_idx += 1;
    }
    while table.len() <= text.len() {
        table.push(char_idx);
    }
    table
}

#[pyfunction]
fn find_match(text: &str, pat: &str) -> Option<(usize, usize)> {
    let byte_start = memchr::memmem::find(text.as_bytes(), pat.as_bytes())?;
    let byte_end = byte_start + pat.len();
    let table = build_byte_to_char_table(text);
    Some((table[byte_start], table[byte_end]))
}
```

ASCII fast path — skip the table when every byte is < 128:

```rust
if text.is_ascii() {
    return Some((byte_start, byte_end));
}
```

Testing this path (from Python): every `#[pyfunction]` that returns
a text offset has a pytest with a CJK or emoji input. See
[`testing.md`](testing.md).

---

## Error conversion — `impl From<Error> for PyErr`

One error enum per crate, one `From` impl at the boundary. The whole
mapping lives in [`src/error.rs`](../../../src/error.rs).

```rust
// src/error.rs
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

impl From<Error> for PyErr {
    fn from(e: Error) -> Self {
        match e {
            Error::QueryParse(_)
            | Error::RegexComplexity(_)
            | Error::InvalidCursor(_) => PyValueError::new_err(e.to_string()),
            _ => PyRuntimeError::new_err(e.to_string()),
        }
    }
}
```

Rules:

- **User-input errors -> `PyValueError`.** Query grammar, regex
  complexity, cursor validation.
- **System / runtime errors -> `PyRuntimeError`.** IO, state
  corruption, tantivy errors.
- **Never expose internals.** The `Display` impl on `Error`
  (via `thiserror`) is agent-facing text. No file paths, no raw SQL,
  no inner-library stack traces.

For per-request wall-clock limits, consider a dedicated exception
(`klmcp.errors.QueryTimeout` subclass of `TimeoutError`) so callers
can catch the right thing. Keep the mapping table in
`src/error.rs`.

### Enabling `?` across the boundary

The `?` operator works on pyfunctions because of this impl. Pattern:

```rust
#[pyfunction]
fn lore_search(query: &str) -> PyResult<Vec<PyHit>> {
    let parsed = router::parse_query(query)?;   // Error -> PyErr via From
    let hits   = router::search(&parsed)?;
    Ok(hits.into_iter().map(PyHit::from).collect())
}
```

---

## Pickle support

Every `#[pyclass]` must be pickleable. Pydantic models on the Python
side serialize cleanly; our `PyHit` / `PyRouter` / etc. need to do
the same so:

- `multiprocessing` works.
- Subinterpreters (3.14) can move objects between.
- Caches can persist.

Two patterns, pick by type:

### Pattern A — small, stable value types: `__getnewargs__`

For types constructible from a short, stable argument list, implement
`__getnewargs__`:

```rust
#[pyclass(module = "kernel_lore_mcp._core", name = "Hit", frozen)]
pub struct PyHit {
    #[pyo3(get)] message_id: String,
    #[pyo3(get)] list: String,
    #[pyo3(get)] score: f32,
}

#[pymethods]
impl PyHit {
    #[new]
    fn new(message_id: String, list: String, score: f32) -> Self {
        Self { message_id, list, score }
    }

    fn __getnewargs__(&self) -> (String, String, f32) {
        (self.message_id.clone(), self.list.clone(), self.score)
    }
}
```

### Pattern B — larger/opaque: `__getstate__` + `__setstate__` + bincode

For types carrying substantial internal state:

```rust
use serde::{Deserialize, Serialize};
use pyo3::types::PyBytes;

#[derive(Serialize, Deserialize)]
struct RouterState { /* ... */ }

#[pyclass(module = "kernel_lore_mcp._core", name = "Router")]
pub struct PyRouter {
    state: RouterState,
}

#[pymethods]
impl PyRouter {
    fn __getstate__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = bincode::serialize(&self.state)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(PyBytes::new(py, &bytes))
    }

    fn __setstate__(&mut self, state: Bound<'_, PyBytes>) -> PyResult<()> {
        self.state = bincode::deserialize(state.as_bytes())
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(())
    }

    #[new]
    fn new_empty() -> Self {
        Self { state: RouterState::default() }
    }
}
```

Schema versioning: embed a `version: u32` field in the serialized
struct and reject old versions on `__setstate__`. See
[`src/schema.rs`](../../../src/schema.rs) for the project-wide
`SCHEMA_VERSION`.

---

## Returning data to Python

Three options, pick by shape:

| Shape | Return | Example |
|-------|--------|---------|
| Single scalar / struct | `Py<PyT>` or `PyT` with `#[pyclass]` | `PyHit` |
| Collection of structs | `Vec<PyT>` | `Vec<PyHit>` from search |
| Heterogeneous / dict-ish | `Bound<'py, PyDict>` | rare — prefer typed structs |

**Prefer typed `#[pyclass]` over `dict` returns.** The Python wrapper
layer can still project a pydantic model from a `PyHit` if that's
what FastMCP needs for `outputSchema`. Dict returns lose type
information and are where "where did this field come from" bugs
live. See [`../python/pyo3-maturin.md`](../python/pyo3-maturin.md).

---

## Batching

FFI round-trip is ~25 ns on top of the work done. For tight inner
loops (scoring a list of hits, checking a batch of candidates), pass
the whole list in and get the whole list back:

```rust
// Good — one FFI call
#[pyfunction]
fn score_many(query: &str, texts: Vec<String>) -> PyResult<Vec<f32>> {
    let q = parse_query(query)?;
    Python::detach(move || -> Result<Vec<f32>, Error> {
        texts.iter().map(|t| q.score(t)).collect()
    })
    .map_err(PyErr::from)
}

// Bad — N FFI calls
#[pyfunction]
fn score_one(query: &str, text: &str) -> PyResult<f32> {
    parse_query(query)?.score(text).map_err(PyErr::from)
}
```

For the trigram tier, this means: return all candidate hits in one
call, not one-per-candidate.

---

## Free-threaded considerations (Python 3.14t)

`abi3-py312` default build is **not** compatible with free-threaded
3.14t — PEP 803 "abi3t" hasn't landed in pyo3 yet.

For the free-threaded build path:

```bash
cargo build --no-default-features
```

Rules once we support it:

- `#[pymodule(gil_used = false)]` on the module declaration.
- Every `#[pyclass]` must be `Sync`. If interior mutation is needed,
  use `std::sync::Mutex` or `parking_lot::Mutex` — not `RefCell`.
- All data stored inside `#[pyclass]` must be thread-safe. Our
  existing `Arc<_>` + immutable-inner pattern is well-positioned.
- `Python::detach` is still the right way to release the
  interpreter's attention during CPU-bound work.

Test under both builds. Add a CI matrix entry when free-threaded
support is formally targeted.

---

## Module declaration

Current `src/lib.rs` uses the 0.28 "module as `mod`" form:

```rust
#[pymodule]
mod _core {
    use super::*;

    #[pyfunction]
    fn version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}
```

Items (`#[pyfunction]`, `#[pyclass]`) declared inside the `mod` block
are auto-registered. This replaces the old `m.add_function(wrap_pyfunction!(...))`
boilerplate.

For pyclasses declared in other modules (normal organization), bring
them in with `use`:

```rust
#[pymodule]
mod _core {
    use super::*;
    use crate::router::PyHit;
    use crate::router::PyRouter;
    // ...
}
```

---

## Stub file (`.pyi`)

Required by `ty` on the Python side. Location:
`src/kernel_lore_mcp/_core.pyi`.

Keep the stub in lockstep with the Rust interface. When you add a
`#[pyfunction]`, add its signature to the stub in the same commit.

```python
# src/kernel_lore_mcp/_core.pyi
from typing import Final

def version() -> str: ...

class Hit:
    message_id: Final[str]
    list: Final[str]
    score: Final[float]

def lore_search(query: str) -> list[Hit]: ...
```

See [`../python/pyo3-maturin.md`](../python/pyo3-maturin.md) for the
`py.typed` marker and layout.

---

## Checklist — adding a new pyfunction

1. Write the pure-Rust implementation in the appropriate `src/*.rs`
   module. Return `Result<T, Error>`.
2. Add the `#[pyfunction]` wrapper in the same module or in
   `src/lib.rs`. Use `detach` for work over 1 us.
3. Map errors via the single `impl From<Error> for PyErr` — do NOT
   write ad-hoc `.map_err(|e| PyRuntimeError::new_err(e.to_string()))`.
4. Add a `.pyi` stub in `src/kernel_lore_mcp/_core.pyi`.
5. Add a pytest (`tests/python/test_<module>.py`). Cover:
   - Happy path.
   - Every error variant this function can produce.
   - Non-ASCII input if text is involved.
6. `cargo test` + `uv run pytest`. Both green.

---

## Anti-patterns

| Don't | Why |
|-------|-----|
| `py.allow_threads(\|\| ...)` | Renamed to `detach` in 0.28. |
| `Python::with_gil(\|py\| ...)` | Renamed to `attach` in 0.28. |
| `&PyString`, `&PyAny` borrow style | Legacy pre-0.21 API. Use `Bound<'py, T>`. |
| Business logic in `#[pyfunction]` | Layer 1 shouldn't care about Python. |
| Holding GIL across `rayon::spawn` | Deadlock risk + serializes parallelism. |
| Returning raw `PyDict` | Loses typing. Use `#[pyclass]`. |
| Per-item `#[pyfunction]` calls in a hot loop | FFI overhead. Batch. |
| Ad-hoc `map_err(PyErr)` | Breaks the single error-mapping invariant. |
| Byte offsets returned as char offsets | Bug on every non-ASCII input. |
| `#[pyclass]` without pickle support | Breaks multiprocessing + subinterpreters. |

---

## Cross-references

- [`index.md`](index.md) — standards index.
- [`../python/pyo3-maturin.md`](../python/pyo3-maturin.md) —
  authoritative contract; three-layer architecture; Python-side
  rules; stubs.
- [`../../../CLAUDE.md`](../../../CLAUDE.md) — pyo3 0.28.3 pin and
  the `detach`/`attach` rename note.
- [`language.md`](language.md) — when to use `impl Trait`, GATs,
  `async fn in traits` (spoiler: not in the FFI layer).
- [`testing.md`](testing.md) — how FFI tests are structured (pytest,
  not cargo test).
- [`design/errors.md`](design/errors.md) — thiserror variants, the
  `From<Error> for PyErr` mapping.
