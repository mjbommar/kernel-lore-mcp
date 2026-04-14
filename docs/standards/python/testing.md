# Testing, Debugging, and Benchmarking

Adapted from `../../../../../273v/kaos-modules/docs/python/testing.md`.

Development in kernel-lore-mcp runs on three feedback loops:
**introspection** (understand the code), **testing** (prove it works),
and **benchmarking** (prove it's fast enough). All three are
continuous, not afterthoughts.

See also: [Rust counterpart](../rust/testing.md) for `cargo test` and
`criterion` on the Rust side.

---

## REPL-Driven Development

Before writing code against FastMCP, the Rust extension, or any
library, **examine it first**. Use `uv run python -c ...` for quick
introspection.

### Inspect the Rust Extension

The Rust core imports as `kernel_lore_mcp._core`. Never guess its
surface — inspect it.

```bash
# Public surface of the extension
uv run python -c "
from kernel_lore_mcp import _core
print([x for x in dir(_core) if not x.startswith('_')])
"

# Module-level docstring + version
uv run python -c "
from kernel_lore_mcp import _core
print(_core.__doc__)
print(getattr(_core, '__version__', 'no-version'))
"

# Signature of a specific binding
uv run python -c "
import inspect
from kernel_lore_mcp import _core
print(inspect.signature(_core.Router.query))
"
```

### Inspect the Python Package

```bash
# Top-level public API
uv run python -c "
import kernel_lore_mcp
print([x for x in dir(kernel_lore_mcp) if not x.startswith('_')])
"

# Tool signature
uv run python -c "
import inspect
from kernel_lore_mcp.tools.search import lore_search
print(inspect.signature(lore_search))
"

# Walk the subpackages
uv run python -c "
import pkgutil, kernel_lore_mcp
for _, name, ispkg in pkgutil.iter_modules(kernel_lore_mcp.__path__, 'kernel_lore_mcp.'):
    print(f'{name}: package={ispkg}')
"
```

### Inspect External Libraries

```bash
# FastMCP server class
uv run python -c "
import fastmcp
print([x for x in dir(fastmcp) if not x.startswith('_')])
"

# The in-process Client we use in tests
uv run python -c "
from fastmcp import Client
print([x for x in dir(Client) if not x.startswith('_')])
"
```

**Never guess.** Wrong guesses cause subtle bugs that pass mocked tests
but fail in production.

---

## Debugging with pdb

Python's built-in debugger is always available. Use it.

### Breakpoints

```python
from kernel_lore_mcp.router import Router

async def lore_search(query: str, *, limit: int = 20):
    breakpoint()  # Drops into pdb here
    router = Router.shared()
    hits = await asyncio.to_thread(router.query, query, limit=limit)
    return hits
```

### Essential Commands

| Command | Action |
|---------|--------|
| `n` | Next line (step over) |
| `s` | Step into function |
| `c` | Continue to next breakpoint |
| `p expr` | Print expression value |
| `pp expr` | Pretty-print expression |
| `l` | List source around current line |
| `ll` | List entire current function |
| `w` | Call stack |
| `u` / `d` | Move up/down the call stack |
| `b N` | Set breakpoint at line N |
| `q` | Quit debugger |
| `!stmt` | Execute Python statement |

### Async Debugging

`breakpoint()` works inside async functions. For deeper async debugging
on 3.14, the new `asyncio.ps`/`pstree` introspection helps:

```bash
# Inspect a live asyncio task tree (3.14)
uv run python -m asyncio ps $PID
uv run python -m asyncio pstree $PID
```

---

## Testing with pytest

### Configuration (from `pyproject.toml`)

```toml
[tool.pytest.ini_options]
testpaths = ["tests/python"]
asyncio_mode = "auto"
```

`asyncio_mode = "auto"` means every `async def test_*` function is
automatically marked as an asyncio test — no per-test decorator needed.

### Test Organization

```
tests/python/
├── conftest.py                    # shared fixtures (Client, paths)
├── fixtures/                      # synthetic lore shards, .mbox samples
│   ├── shards/
│   │   ├── tiny-lkml/             # 50-message synthetic git shard
│   │   └── tiny-stable/           # 30-message synthetic git shard
│   ├── messages/                  # hand-curated .eml samples
│   └── expected/                  # golden query results (JSON)
├── unit/
│   ├── test_router_parse.py       # query grammar unit tests
│   ├── test_models.py             # pydantic model validation
│   ├── test_cursor.py             # HMAC cursor round-trip
│   └── test_tokenizer.py          # calls into _core directly
├── integration/
│   ├── test_lore_search.py        # full pipeline against tiny shards
│   ├── test_lore_thread.py
│   └── test_reindex.py            # reindex binary end-to-end
└── mcp/
    ├── test_tool_contracts.py     # Client + in-process FastMCP
    ├── test_tool_names.py         # tool-name / readOnlyHint sanity
    └── test_structured_content.py # outputSchema and structuredContent
```

### The In-Process `fastmcp.Client` Fixture

Our canonical way to exercise MCP tools end-to-end without a network
hop. The Client talks to the FastMCP server instance directly via
in-memory transport — faster than stdio, deterministic, and gives us
full access to `structuredContent`.

```python
# tests/python/conftest.py
from collections.abc import AsyncIterator

import pytest
import pytest_asyncio
from fastmcp import Client

from kernel_lore_mcp.server import build_server
from kernel_lore_mcp.config import Settings

@pytest.fixture
def settings(tmp_path) -> Settings:
    """Isolated settings rooted in a tmp data directory."""
    return Settings(
        data_dir=tmp_path / "data",
        cursor_key="test-key-do-not-use-in-prod-32bytes",
        bind="127.0.0.1",
    )

@pytest_asyncio.fixture
async def client(settings) -> AsyncIterator[Client]:
    """In-process MCP client bound to a fresh server instance."""
    server = build_server(settings)
    async with Client(server) as c:
        yield c
```

Using the fixture:

```python
# tests/python/mcp/test_tool_contracts.py
async def test_lore_search_returns_structured_content(client):
    result = await client.call_tool(
        "lore_search",
        {"query": "signed-off-by:torvalds@linux-foundation.org", "limit": 5},
    )

    # FastMCP auto-serializes pydantic responses into structuredContent
    assert result.structured_content is not None
    hits = result.structured_content["hits"]
    assert isinstance(hits, list)
    for hit in hits:
        # Every hit must carry these per CLAUDE.md
        assert "message_id" in hit
        assert "cite_key" in hit
        assert "from_addr" in hit
        assert "lore_url" in hit
        assert "tier_provenance" in hit


async def test_tool_metadata_is_readonly(client):
    tools = await client.list_tools()
    by_name = {t.name: t for t in tools}

    # Every tool we ship is read-only
    for name in (
        "lore_search",
        "lore_thread",
        "lore_patch",
        "lore_activity",
        "lore_message",
        "lore_series_versions",
        "lore_patch_diff",
    ):
        assert name in by_name, f"missing tool: {name}"
        assert by_name[name].annotations.read_only_hint is True
```

### Synthetic Fixtures (Not Mocks)

Our synthetic fixtures live under `tests/python/fixtures/` and are
**real git shards** containing a small curated set of real lore
messages (anonymized where needed). They are compressed + checked in.

```python
# tests/python/integration/test_lore_search.py
import json
from pathlib import Path

import pytest

FIXTURE_ROOT = Path(__file__).parent.parent / "fixtures"

@pytest.fixture
def tiny_lkml_shard(tmp_path) -> Path:
    """A 50-message synthetic LKML shard, ingested into a fresh store."""
    from kernel_lore_mcp._core import ingest_shard

    shard_src = FIXTURE_ROOT / "shards" / "tiny-lkml"
    data_dir = tmp_path / "data"
    ingest_shard(str(shard_src), str(data_dir), list_name="lkml")
    return data_dir


async def test_lore_search_finds_known_message(client_for_shard, tiny_lkml_shard):
    """Against the tiny-lkml shard, a known Message-ID should surface."""
    result = await client_for_shard.call_tool(
        "lore_search",
        {"query": "m:20240115-scheduler-fix-v2-0@example.com"},
    )
    hits = result.structured_content["hits"]
    assert any(h["message_id"] == "20240115-scheduler-fix-v2-0@example.com" for h in hits)
```

Golden results are stored as JSON next to the fixture for drift
detection:

```python
def test_lore_search_golden_v1(client_for_shard, tiny_lkml_shard):
    expected = json.loads((FIXTURE_ROOT / "expected" / "golden-v1.json").read_text())
    # ...call tool, compare, fail loudly if the shape changes
```

### Async Tests

With `asyncio_mode = "auto"`, just define them:

```python
async def test_router_generation_reload(settings, tmp_path):
    """Router re-reads the generation counter before each query."""
    from kernel_lore_mcp._core import Router

    router = Router.open(str(settings.data_dir))
    g0 = router.generation
    _bump_generation(settings.data_dir)
    _ = router.query("b:hello", limit=1)  # triggers stat() + reload
    assert router.generation > g0
```

No `@pytest.mark.asyncio` decorator required.

### Parametrized Tests

```python
import pytest

@pytest.mark.parametrize(
    "query, must_match_tier",
    [
        ("b:scheduler",            "bm25"),      # prose-body term
        ("/CVE-\\d{4}-\\d+/",      "trigram"),   # regex
        ("signed-off-by:*",        "metadata"),  # trailer filter
    ],
)
def test_router_tier_selection(query, must_match_tier):
    from kernel_lore_mcp._core import parse_query

    parsed = parse_query(query)
    assert must_match_tier in parsed.tiers
```

### Test Markers

We use `--strict-markers`; every marker must be registered. Current
set:

```toml
[tool.pytest.ini_options]
testpaths = ["tests/python"]
asyncio_mode = "auto"
markers = [
    "unit: fast, no fixtures, no Rust rebuild",
    "integration: requires synthetic shard ingest",
    "mcp: exercises the FastMCP Client surface",
    "live: runs against a real small lore shard (opt-in, see below)",
    "benchmark: perf benchmarks, not run by default",
]
```

Select subsets:

```bash
uv run pytest tests/python -m unit -v             # fastest
uv run pytest tests/python -m "not benchmark" -v  # default CI
uv run pytest tests/python -m live -v             # acceptance gate
```

---

## The Live Test Mandate — Adapted

**Mocked tests are NOT proof of correctness.** A mocked test proves
the code matches the mock — not lore.kernel.org, not the real mbox
parser, not the real tier behavior.

For kernel-lore-mcp, the live mandate is:

1. **Every tool must have an integration test against a real (small)
   lore shard**, ingested end-to-end via the real
   `ingest_shard` path — not via a mock.
2. **Unit tests that poke `_core` directly are fine for correctness of
   individual algorithms** (tokenizer boundaries, trigram extraction,
   BM25 scoring on a toy corpus).
3. **Assertions must verify content understanding**, not just
   `len(hits) > 0`. Check `tier_provenance`, `cite_key`, and actual
   known-good Message-IDs.
4. **Fixtures are real data**: a small but real chunk of a public lore
   list, checked into `tests/python/fixtures/shards/`. Not
   synthesized mbox strings, not mocked gix.
5. **Never declare a bug "fixed" without a regression test that
   reproduces it against the fixture set.**

The distinction from KAOS: we do **not** hit
`https://lore.kernel.org/` from the test suite. That is a public
service and we are not going to DoS it with CI runs. The "live" tier
means real Rust code + real tantivy + real fst+roaring + real gix +
real mbox parsing against a known local shard.

### Running the Tiers

```bash
# Fast feedback (no shard ingest)
uv run pytest tests/python -m unit -v

# Everything that isn't benchmark
uv run pytest tests/python -m "not benchmark" -v

# Full acceptance gate (ingests the tiny shards first)
uv run pytest tests/python -v
```

---

## Benchmarking

### When to Benchmark

- Before/after any Rust-side perf change (trigram candidate cap,
  tantivy tokenizer rework, posting-list intersection).
- Before/after any Python-side perf change on the MCP hot path
  (cursor signing, pydantic dump).
- When we change a dep version that affects inner loops.

### Micro-Benchmarks from the Shell

```bash
uv run python -c "
import timeit
from kernel_lore_mcp._core import parse_query

t = timeit.timeit(
    lambda: parse_query('signed-off-by:torvalds@linux-foundation.org AND b:scheduler'),
    number=10_000,
)
print(f'parse_query: {t / 10_000 * 1e6:.2f} us/call')
"
```

### Profiling with cProfile

```bash
uv run python -c "
import cProfile
from pathlib import Path
from kernel_lore_mcp._core import Router

r = Router.open(str(Path('tests/python/fixtures/warm/data')))
cProfile.run('r.query(\"b:scheduler\", limit=20)', sort='cumulative')
"
```

### pytest-benchmark for Comparative Benchmarks

```python
import pytest

@pytest.mark.benchmark
def test_lore_search_bm25_benchmark(benchmark, warm_router):
    result = benchmark(warm_router.query, "b:scheduler regression", 20)
    assert len(result.hits) > 0
```

### Document Performance in Commits

```
perf(trigram): cap candidate set before regex confirm

Before: 180 ms/query (pathological /CVE-\d{4}/ over stable tree)
After:   12 ms/query
Speedup: 15x
```

---

## The Development Loop

1. **Understand** — Introspect the module, read the source, examine
   the fixture.
2. **Write a failing test** — Define "correct" before writing code.
3. **Implement** — Minimum code to pass the test.
4. **QA** — `ruff format` → `ruff check` → `ty check` → `pytest`.
5. **Benchmark** — If perf matters, measure before and after.
6. **Commit** — Atomic, descriptive, see [git.md](git.md).
7. **Repeat.**

Never skip step 1 (understanding) or step 4 (QA). The time saved by
skipping is always less than the time spent debugging.

---

## Cross-references

- [code-quality.md](code-quality.md) — the QA pipeline this feeds into
- [uv.md](uv.md) — `uv run pytest` and friends
- [pyo3-maturin.md](pyo3-maturin.md) — how `_core` gets built before tests
- [Rust counterpart](../rust/testing.md) — `cargo test` for pure-Rust
  algorithm coverage
