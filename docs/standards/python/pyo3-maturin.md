# PyO3 and Maturin — Where Python Meets Rust

Adapted from `../../../../../273v/kaos-modules/docs/python/pyo3-maturin.md`.

**CRUCIAL VERSION DELTA:** KAOS targets PyO3 `0.23+`. kernel-lore-mcp is
on **PyO3 `0.28.3`**. Two consequences dominate everything in this
guide:

1. **`Python::detach()` and `Python::attach()` are the current names.**
   They replaced `allow_threads()` and `with_gil()` in PRs #5209 and
   #5221, shipped in 0.28. New code **must** use the current names.
   `allow_threads` / `with_gil` still work as aliases, but they are
   deprecated and will be removed. Anywhere you see them in a diff —
   rewrite.
2. **0.28 is free-threaded-aware by default.** Modules annotated with
   `#[pymodule(gil_used = false)]` run on `python3.14t` without
   re-enabling the GIL. We defer a free-threaded wheel until PEP 803
   (abi3t) lands, but the Rust code is already compatible.

See also: [Rust counterpart](../rust/pyo3-maturin.md) for the Rust-side
patterns (`Bound<'py, T>`, error conversions, module wiring).

---

## Decision Framework

**Reach for Rust+PyO3 when ALL of these are true:**

1. **CPU-bound inner loop** — tokenization, trigram extraction,
   posting-list intersection, mbox parsing, zstd block reads.
2. **Called frequently** — thousands to millions of calls per query,
   or large inputs where per-item work compounds.
3. **Python is the measured bottleneck** — profiling shows > 50% of
   time in pure Python compute (not I/O, not FastMCP framing).
4. **The algorithm is stable** — the interface is settled.

**Stay in pure Python when ANY of these is true:**

- I/O-bound work (HTTP, file reads on cold paths) — the bottleneck
  is waiting, not computing.
- Simple glue logic — combining results from the Router with
  pydantic models for the MCP response.
- Function called < 1000 times on small inputs.
- Per-call computation < 100 ns — FFI overhead (~25 ns) dominates.
- Rapid prototyping — get the algorithm right in Python first, then
  port the hot path.

### Cost Model

| Component | Cost |
|-----------|------|
| Pure Python function call | ~43 ns |
| PyO3 function call (0.28) | ~68 ns |
| **FFI overhead per call** | **~25 ns** |

The ~25 ns overhead is negligible when the Rust function body does
meaningful work:

- **< 100 ns of work per call**: Rust is likely slower than Python.
- **1–10 μs of work per call**: Rust starts winning.
- **> 100 μs of work per call**: FFI overhead is < 0.025% of total
  time — pure win.
- **Batched calls** (pass list in, get results back): overhead
  amortized across N items.

### Decision Table

| Criterion | Use Rust+PyO3 | Stay Pure Python |
|-----------|:---:|:---:|
| CPU-bound inner loop (tokenize, hash, parse) | Yes | — |
| Called > 1000x on non-trivial input | Yes | — |
| Profiling shows > 50% time in Python compute | Yes | — |
| Need true parallelism (rayon) | Yes | — |
| Memory-constrained (streaming ~350 GB lore) | Yes | — |
| Thread safety for free-threaded Python | Yes | — |
| I/O bound (HTTP, grokmirror) | — | Yes |
| Rapid prototyping / unstable algorithm | — | Yes |
| Simple glue logic (MCP response assembly) | — | Yes |
| Function called < 100 times | — | Yes |
| Per-call work < 100 ns | — | Yes |

### Where We Actually Draw the Line

| Subsystem | Language | Why |
|-----------|----------|-----|
| Ingestion walk (gix + mbox) | **Rust** | CPU-bound, high volume, rayon fan-out |
| Tokenizer (`kernel_prose`) | **Rust** | Inner loop of BM25 indexing |
| Trigram extract + postings | **Rust** | Bit-twiddling over GB of diffs |
| Tantivy queries | **Rust** | The library is Rust; PyO3 only wraps the reader |
| Router dispatch + merge | **Rust** | Hot path, called per query |
| FastMCP tools | **Python** | Glue, pydantic serialization, async I/O |
| Cursor signing (HMAC) | **Python** | Tiny payload, stdlib `hmac`, ~μs/call |
| Settings / config | **Python** | pydantic-settings, cold path |
| Structured logging | **Python** | structlog, async-friendly |
| Prometheus metrics | **Python** | `prometheus_client`, cold path |

---

## Current Versions

| Tool | Version | Key Feature |
|------|---------|-------------|
| **PyO3** | 0.28.3 | `Python::detach`/`attach`, free-threaded support, `Bound<'py, T>` API |
| **Maturin** | 1.13.1 | abi3, PGO, PEP 770 SBOM, PEP 735 dependency groups |
| **Python** | 3.12 floor (abi3-py312), 3.14 preferred | See [language.md](language.md) |

`Cargo.toml` pins:

```toml
[dependencies]
pyo3 = { version = "0.28", features = ["extension-module", "abi3-py312"], optional = true }
```

---

## Three-Layer Architecture

Every Rust+Python responsibility in kernel-lore-mcp follows the same
three-layer structure:

```
┌────────────────────────────────────────────────────────┐
│  Layer 3: Python re-exports (public API)               │
│  Typed pydantic / dataclass wrappers, async glue       │
│  src/kernel_lore_mcp/router.py                          │
│  src/kernel_lore_mcp/tools/search.py                    │
├────────────────────────────────────────────────────────┤
│  Layer 2: PyO3 bindings (FFI boundary)                 │
│  #[pyfunction], #[pyclass], PyErr conversions          │
│  src/lib.rs  (+ thin per-module files)                  │
├────────────────────────────────────────────────────────┤
│  Layer 1: Pure Rust core (no PyO3 dependency)          │
│  Algorithms, data structures, testable with            │
│  `cargo test --no-default-features`                    │
│  src/router.rs, src/trigram.rs, src/bm25.rs, ...       │
└────────────────────────────────────────────────────────┘
```

**Layer 1: Pure Rust core** — Testable without Python. No `pyo3`
import. This is where the actual work happens.

**Layer 2: PyO3 bindings** — Thin wrappers that convert Python types
to Rust types, call the core, convert back. `#[pyfunction]` and
`#[pyclass]` declarations. **Errors here convert via `From<Error> for
PyErr` in `src/error.rs`.**

**Layer 3: Python re-exports** — pydantic models for MCP responses;
`asyncio.to_thread` wrappers for the sync Rust calls. The only place
FastMCP-specific concerns (outputSchema, cursors, tool metadata) live.

### Why Three Layers

- Pure Rust core can be tested and benchmarked without a Python
  interpreter. Critical for our indexer correctness tests.
- PyO3 bindings stay minimal — easy to audit for GIL discipline and
  lifetime correctness.
- Python wrappers give `ty` a chance to type-check every MCP boundary.
- Clean separation makes it possible (one day) to ship a WASM build
  of the router core — nothing in Layer 1 depends on Python.

---

## GIL Discipline on PyO3 0.28 — `detach` and `attach`

### The Rename

**PRs #5209 and #5221**, merged for the 0.28 release, renamed:

| Old (0.23 – 0.27) | New (0.28+) |
|---|---|
| `py.allow_threads(\|\| { ... })` | `py.detach(\|\| { ... })` |
| `Python::with_gil(\|py\| { ... })` | `Python::attach(\|py\| { ... })` |

### Why the Rename

PyO3 is no longer just about the GIL. On free-threaded Python the GIL
does not exist; what actually matters is whether the current thread is
**attached** to the interpreter (holds a reference to interpreter
state) or **detached** (running arbitrary native code). The new names
describe this correctly for both GIL'd and free-threaded builds.

### The Rule for kernel-lore-mcp

**Any Rust call whose body does > 1 ms of work runs inside
`py.detach(|| ...)`.** That includes every tantivy query, every
trigram scan, every mbox parse, every rayon fan-out.

```rust
// src/router.rs — Rust core
pub fn query(&self, q: &ParsedQuery, limit: usize) -> Result<Page, Error> {
    // Pure Rust. No PyO3 types. Safe to call from anywhere.
    // ... tantivy + trigram + merge ...
}
```

```rust
// src/lib.rs — PyO3 binding
#[pymethods]
impl PyRouter {
    fn query(&self, py: Python<'_>, q: &str, limit: usize) -> PyResult<PyPage> {
        let parsed = parse_query(q).map_err(Error::from)?;

        // Release attachment while the actual work runs.
        // Other Python threads can run; rayon inside `query` is free to fan out.
        let page = py.detach(|| self.inner.query(&parsed, limit))?;

        Ok(PyPage::from(page))
    }
}
```

### What Never to Write

```rust
// BAD — `allow_threads` is the old name. Do not write in new code.
py.allow_threads(|| self.inner.query(&parsed, limit))?;

// BAD — `with_gil` is the old name.
Python::with_gil(|py| { /* ... */ });
```

If you find these in a diff — rewrite them before merge. They still
compile as deprecated aliases, but they will disappear in a future
PyO3 release and they misdescribe behavior on free-threaded Python.

### Re-attaching for Short Work

If you've detached to do Rust work and need to hand back to Python
briefly (e.g., to log a warning through a Python logger):

```rust
py.detach(|| {
    let mut reader = self.reader.lock().unwrap();
    reader.reload()?;

    // Attach briefly to call a Python hook
    Python::attach(|py| {
        if let Err(e) = self.on_reload_hook(py) {
            // handle
        }
    });

    Ok(())
})
```

Prefer not to re-attach — it's cheap, but each transition has cost.
Batch Python interactions at the boundary.

---

## PyO3 Binding Rules

These rules are mandatory for every `#[pyclass]`, `#[pyfunction]`, and
`#[pymodule]` in this crate.

### Module Wiring

```rust
// src/lib.rs
use pyo3::prelude::*;

#[pymodule(name = "_core", gil_used = false)]
fn kernel_lore_mcp_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRouter>()?;
    m.add_class::<PyTrigramIndex>()?;
    m.add_function(wrap_pyfunction!(ingest_shard, m)?)?;
    m.add_function(wrap_pyfunction!(parse_query, m)?)?;
    Ok(())
}
```

- `name = "_core"` matches `[tool.maturin] module-name`.
- `gil_used = false` — we are free-threaded-compatible. Required when
  we eventually ship a `3.14t` wheel.

### `#[pyclass]` Naming

Rust side uses `Py*` prefix; Python side sees the clean name via the
`name` attribute.

```rust
#[pyclass(module = "kernel_lore_mcp._core", name = "Router", frozen)]
pub struct PyRouter {
    inner: Arc<router::Router>,
}

#[pyclass(module = "kernel_lore_mcp._core", name = "Page", frozen)]
pub struct PyPage {
    pub hits: Vec<PyHit>,
    pub next_cursor: Option<String>,
}
```

- Use `frozen` for immutable types — unlocks `Sync` without a `Mutex`
  and is required for free-threaded safety.
- Always set `module = "kernel_lore_mcp._core"` so pickling and
  `__repr__` show the fully qualified name.

### Error Conversion

Errors cross the boundary via one central `From` impl — never convert
ad-hoc inside a binding.

```rust
// src/error.rs
use pyo3::exceptions::{PyIOError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("query parse: {0}")]
    QueryParse(String),
    #[error("regex rejected: {0}")]
    RegexComplexity(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tantivy: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    // ... etc
}

impl From<Error> for PyErr {
    fn from(err: Error) -> PyErr {
        match err {
            Error::QueryParse(msg) => PyValueError::new_err(msg),
            Error::RegexComplexity(msg) => PyValueError::new_err(msg),
            Error::Io(e) => PyIOError::new_err(e.to_string()),
            Error::Tantivy(e) => PyRuntimeError::new_err(e.to_string()),
        }
    }
}
```

Every `#[pyfunction]` / `#[pymethod]` returns `PyResult<T>`; the `?`
operator picks up `From<Error> for PyErr` automatically.

### Pickle Support — When Required

Every `#[pyclass]` that might cross a `multiprocessing` boundary or
appear in a test cache must support pickle. For `frozen` classes,
implement `__getnewargs__` (simpler than `__getstate__`/`__setstate__`):

```rust
#[pymethods]
impl PyTrigramIndex {
    fn __getnewargs__(&self) -> (String,) {
        (self.path.clone(),)
    }
}
```

For mutable classes, use `__getstate__`/`__setstate__` + bincode. None
of our `#[pyclass]` types are currently mutable — keep it that way.

### Byte-to-Character Offset Conversion

**Critical for tokenizer bindings and snippet offsets.** Rust `&str`
is bytes; Python `str` is characters. Always convert before returning
text positions to Python.

```rust
fn build_byte_to_char_table(text: &str) -> Vec<usize> {
    let mut table = Vec::with_capacity(text.len() + 1);
    let mut char_idx = 0;
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
```

Fast path: when all bytes are < 128, byte offsets equal character
offsets. Always write round-trip tests with multi-byte text (CJK,
accented characters) — kernel commits from non-US maintainers hit this
regularly.

### Dict Returns → Typed Wrappers on the Python Side

PyO3 bindings should return `#[pyclass]` types where possible.
When a dict is unavoidable (e.g. pass-through to pydantic), wrap it in
Python:

```python
# src/kernel_lore_mcp/router.py
from dataclasses import dataclass

from kernel_lore_mcp import _core


@dataclass(frozen=True, slots=True)
class RawHit:
    message_id: str
    score: float
    tier: str


def _to_raw_hit(raw: dict) -> RawHit:
    return RawHit(
        message_id=raw["message_id"],
        score=raw["score"],
        tier=raw["tier"],
    )


def query(router: _core.Router, q: str, limit: int) -> list[RawHit]:
    page = router.query(q, limit)
    return [_to_raw_hit(h) for h in page.hits_dict()]
```

This is the only place in the Python package where raw dict keys are
accessed. From here up, everything is typed.

### Stub Files (`.pyi`)

Every public function and class in the Rust extension has a stub:

```
src/kernel_lore_mcp/
├── _core.pyi          # typed signatures for the native module
├── py.typed           # PEP 561 marker
└── ...
```

`_core.pyi` is hand-maintained. When you add a `#[pyfunction]` or
`#[pyclass]`, add its stub in the same commit.

```python
# src/kernel_lore_mcp/_core.pyi (excerpt)
from typing import final

@final
class Router:
    generation: int

    @classmethod
    def open(cls, data_dir: str) -> Router: ...
    def query(self, q: str, limit: int) -> Page: ...
    def reload(self) -> None: ...

@final
class Page:
    hits: tuple[Hit, ...]
    next_cursor: str | None

def parse_query(q: str) -> ParsedQuery: ...
def ingest_shard(shard: str, data_dir: str, *, list_name: str) -> None: ...
```

---

## Build Workflow

### Development Cycle

```bash
# 1. Rust QA
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --no-default-features   # Pure Rust tests (no PyO3)

# 2. Build extension (debug is fastest)
uv run maturin develop

# 3. Python QA
uv run ruff format src/kernel_lore_mcp tests/python
uv run ruff check src/kernel_lore_mcp tests/python
uv run ty check src/kernel_lore_mcp tests/python
uv run pytest tests/python -v
```

For release-mode iteration (what CI runs):

```bash
uv run maturin develop --release
```

### `[tool.maturin]` — Verbatim from our `pyproject.toml`

```toml
[build-system]
requires = ["maturin>=1.13,<2.0"]
build-backend = "maturin"

[tool.maturin]
module-name = "kernel_lore_mcp._core"
python-packages = ["kernel_lore_mcp"]
python-source = "src"
features = ["pyo3/extension-module"]

[tool.uv]
cache-keys = [
    { file = "pyproject.toml" },
    { file = "src/**/*.rs" },
    { file = "Cargo.toml" },
    { file = "Cargo.lock" },
]
```

### Cargo.toml Conventions (Summary — full file in repo root)

```toml
[dependencies]
pyo3 = { version = "0.28", features = ["extension-module", "abi3-py312"], optional = true }
# ... tantivy, gix, fst, roaring, mail-parser, regex-automata, zstd, arrow, parquet ...

[features]
default = ["python"]
python = ["pyo3"]

[profile.release]
lto = true
codegen-units = 1
opt-level = 3
```

- `pyo3` is optional — pure-Rust tests run without it
  (`cargo test --no-default-features`).
- `abi3-py312` gives us one wheel that runs on 3.12, 3.13, 3.14.
  Incompatible with `3.14t` — free-threaded build requires
  `--no-default-features` to drop abi3.

---

## Free-Threaded Python Support

**Tracked, not deployed.** The Rust code is free-threaded-ready:

1. `#[pymodule(gil_used = false)]` is set on the `_core` module.
2. All `#[pyclass]` types are `frozen` → `Sync` → safe for parallel
   attachment.
3. Shared state uses `std::sync::Mutex` or `Arc<RwLock<...>>`, never
   `RefCell`.
4. Long-running Rust work runs inside `py.detach(|| ...)`.

What blocks shipping a 3.14t wheel today:

- abi3 and free-threaded are mutually exclusive until **PEP 803**
  ("abi3t") is accepted and implemented.
- The free-threaded build needs `maturin build --no-default-features`
  and a separate CI matrix entry.
- Wheel naming is different (`cp314t-*.whl` vs `cp312-abi3-*.whl`).

When PEP 803 lands we add a second wheel target; until then, do not
flip abi3 off in the main build.

---

## Performance Tips

1. **Batch across the boundary.** `query_many(queries: Vec<String>) ->
   Vec<Page>` beats N calls to `query(query: String) -> Page` when N
   is large.
2. **`extract_bound()` or `.cast()` over `.extract()`** on a `Bound<'py,
   T>` — cheaper type checks.
3. **Pre-allocate output buffers.** `Vec::with_capacity(hint)` in the
   hot loop; avoid repeated `push` reallocations.
4. **Release attachment for any work > 1 ms.** `py.detach()` is the
   one-line optimization that unlocks rayon and lets other Python
   threads progress.
5. **Use `rayon` inside `py.detach()`** for data parallelism. Trivially
   parallel tier dispatch is `.par_iter().map(...).collect()`.
6. **Keep `#[pyclass]` types small and `frozen`.** Every mutable
   field forces a `Mutex` or RefCell path on free-threaded Python.

---

## Cross-references

- [uv.md](uv.md) — `[tool.maturin]` and `[tool.uv] cache-keys`
- [code-quality.md](code-quality.md) — `.pyi` stubs feeding `ty`
- [naming.md](naming.md) — `_core` vs `_native`, `Py*` prefix
- [testing.md](testing.md) — `cargo test --no-default-features` for
  Layer 1; `pytest` for Layers 2–3
- [Rust counterpart](../rust/pyo3-maturin.md) — `Bound<'py, T>`
  patterns, `From<Error> for PyErr` wiring
