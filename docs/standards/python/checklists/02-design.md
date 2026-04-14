# Checklist: Design and Architecture

Adapted from `.../kaos-modules/docs/python/checklists/02-design.md`.

Sketch the wire shape and module placement before writing code.
The data model is the contract; everything else follows.

---

## Non-negotiables (from `CLAUDE.md`)

- [ ] **Every MCP tool returns a pydantic `BaseModel`.** Never a
      bare `dict`. FastMCP auto-derives `outputSchema` +
      `structuredContent` from it.
- [ ] **Every hit carries the full envelope:** `message_id`,
      `cite_key`, `from_addr`, `lore_url`, `subject_tags[]`,
      `is_cover_letter`, `series_version`, `series_index`,
      `patch_stats` (if `has_patch`),
      `snippet{offset,length,sha256,text}`, `tier_provenance[]`,
      `is_exact_match`, `cross_posted_to[]`.
- [ ] **No phrase queries on prose body** in v1. Router returns
      `Error::QueryParse` with actionable message — never silent
      degradation.
- [ ] **Pagination cursors are HMAC-signed.** Plain b64 tuples are
      rejected. Key from `KLMCP_CURSOR_KEY`.
- [ ] **Tools are read-only.** Annotate `readOnlyHint: true`.
- [ ] **`blind_spots` is a resource**, not per-response.

---

## Items

- [ ] **Write the pydantic request + response models first.** Put
      them in `src/kernel_lore_mcp/models.py`. Type every field.
      Use `ConfigDict(frozen=True)` on response models.
      > Ref: [../libraries/pydantic.md](../libraries/pydantic.md)

- [ ] **Sketch the wire shape.** What does the tool's JSON input
      and JSON output look like, field by field? Write it in the
      design comment or `docs/mcp/tool-schemas.md` before coding.

- [ ] **Design typed function signatures.** Accept specific
      pydantic types, return specific types. Keyword-only for
      optional args. Never pass `dict[str, Any]` across layers.

- [ ] **Identify boundaries.** Input enters via FastMCP tool call
      (already validated by pydantic). It leaves as
      `structuredContent`. Internal `_core.*` calls use simple
      primitives — validate before the call, trust after.
      > Ref: [../design/boundaries.md](../design/boundaries.md)

- [ ] **Plan module placement.** New tool -> one file per tool
      under `src/kernel_lore_mcp/tools/`. New model -> add to
      `models.py`. New route -> `routes/`. New resource ->
      `resources/`. Do not scatter.
      > Ref: [../design/modules.md](../design/modules.md)

- [ ] **Choose explicit registration.** The server wires up each
      tool in `server.py` via `mcp.tool(...)(fn)`. Do NOT register
      by importing a module for its side effects.
      > Ref: [../libraries/fastmcp.md](../libraries/fastmcp.md)

- [ ] **Design the error surface.** Router errors from Rust come
      through as typed `PyErr` subclasses. Map each to an MCP
      `isError` response with a three-part message: what went
      wrong, how to fix it, what to try instead.
      > Ref: [../design/errors.md](../design/errors.md)

- [ ] **Decide Python vs Rust.** Pure Python for MCP glue,
      FastMCP surface, and I/O. Rust for CPU-bound inner loops
      (ingest, indexing, query dispatch). If you add a new Rust
      surface, plan the `.pyi` stub at design time.
      > Ref: [../pyo3-maturin.md](../pyo3-maturin.md)

- [ ] **Plan concurrency.** All tool handlers are `async def`.
      Calls into `_core` go through `await asyncio.to_thread(...)`.
      Decide at design time — not during implementation.

- [ ] **Plan default arguments.** `rt:` default is 5 years and
      must be echoed in `default_applied: ["rt:5y"]` so the LLM
      knows. Any other defaulted filter must do the same.

- [ ] **Follow naming conventions.** Tools: `lore_{action}`
      (e.g., `lore_search`, `lore_patch_diff`). Pydantic
      response models: `{Tool}Response`. Enums: `{Thing}Kind`.
      > Ref: [../naming.md](../naming.md)

- [ ] **Plan reader-reload discipline.** Any new query path
      must stat the generation file at request entry and call
      `reader.reload()` on advance. Design this in, not bolted on.

- [ ] **Check the dependency DAG.** Python imports:
      `config -> models -> tools -> server`. Rust symbols only
      flow in via the lazy `_core` import inside the tool module.
      No cycles; no sibling-tool imports.

- [ ] **Confirm no new runtime dependency is required.** If it
      is, log the reason in the design note and flag it in the
      PR description — pins are pinned on purpose.
