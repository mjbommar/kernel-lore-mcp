# Checklist: Testing

Adapted from `.../kaos-modules/docs/python/checklists/04-test.md`.

Prove correctness with real data. The acceptance gate for any
non-trivial change is `uv run pytest -v` passing — unit tier on
synthetic fixtures, integration tier on a real lore shard.

---

## Non-negotiables (from `CLAUDE.md`)

- [ ] **`uv run maturin develop` before any `pytest` run** that
      touches Rust. Stale `_core.so` produces ghost failures.
- [ ] **Synthetic fixtures live under `tests/python/fixtures/`.**
      Do not fetch lore from developer machines — the deploy box
      does that.
- [ ] **Live tests run against a real lore shard** (opt-in marker,
      local path). Never in CI unless explicitly allowed.
- [ ] **In-process `fastmcp.Client`** is the supported way to
      exercise tools. No out-of-process MCP dance in unit tests.

---

## Items

- [ ] **Write tests before or alongside implementation.** Each
      feature has at least one test that would have failed before
      the code existed.

- [ ] **Use real fixtures.** Synthetic mbox files, real patches
      (public lore samples) committed under
      `tests/python/fixtures/`. Never `b"fake"` as a stand-in for
      a patch body.

- [ ] **Use the in-process `fastmcp.Client` fixture.** Example:
      ```python
      @pytest_asyncio.fixture
      async def client(server):
          async with fastmcp.Client(server) as c:
              yield c

      async def test_lore_search(client):
          result = await client.call_tool("lore_search", {"query": "f:mm/slub.c rt:30d"})
          assert result.structured_content["default_applied"] == []
          assert all("message_id" in hit for hit in result.structured_content["hits"])
      ```

- [ ] **Assert content, not existence.** `len(hits) > 0` is
      almost never enough. Assert message IDs, subject tags,
      tier provenance, ordering, HMAC validity of cursors.

- [ ] **Test at the MCP boundary.** Given a tool input dict, do
      you get the expected `structured_content` shape? You
      should not need to inspect `_core` internals to write the
      test.

- [ ] **Use pytest markers.** `unit`, `integration`, `live`.
      Live tests require a real shard path via env
      (`KLMCP_TEST_SHARD=/path/to/shard`). Skip if missing.

- [ ] **Async tests via pytest-asyncio.** `asyncio_mode = "auto"`
      is configured in `pyproject.toml`. Use `@pytest_asyncio.fixture`
      for async setup/teardown.

- [ ] **Exercise both tiers of the query router.** For a change
      touching router dispatch, write tests that hit metadata,
      trigram, and BM25 tiers independently plus one that
      crosses tiers — verify `tier_provenance[]` in the output.

- [ ] **Test reader-reload coherence.** If your change touches
      the generation file, write a test that bumps the generation
      between two queries and verifies the second picks up the
      new state.

- [ ] **Test error paths.** Phrase-on-prose must return
      `Error::QueryParse`. Catastrophic regex must return
      `Error::RegexComplexity`. Tampered cursor must fail HMAC
      verification. Every three-part error message has its own
      assertion.

- [ ] **Test default echoing.** If a request does not supply
      `rt:`, the response `default_applied` must include
      `"rt:5y"`. If it does supply one, `default_applied` must
      not.

- [ ] **Use `@pytest.mark.parametrize` for matrix coverage.**
      Query operators, subject-tag extraction, trailer parsing —
      all have natural parameter tables.

- [ ] **Benchmark critical paths.** `cargo bench` (criterion) for
      Rust hot paths, `pytest-benchmark` for the Python shim if
      that's where time goes. Don't ship a perf claim without
      numbers.

- [ ] **Never declare a bug fixed without a test proving it.** A
      failing regression test committed with the fix is the only
      acceptable evidence.

- [ ] **Run the full suite before pushing.**
      ```bash
      uv run maturin develop
      uv run pytest -v
      cargo test
      ```
      All green. Live marker run when touching the router or
      ingest.
