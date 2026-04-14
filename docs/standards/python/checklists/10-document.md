# Checklist: Documentation

Adapted from `.../kaos-modules/docs/python/checklists/10-document.md`.

Document decisions and interfaces, not obvious code. Docs ship in
the same commit as the code.

---

## Non-negotiables (from `CLAUDE.md`)

- [ ] **Comments explain WHY, not WHAT.** Identifiers name what
      the code does. Comments explain non-obvious constraints,
      bug workarounds, and design trade-offs.
- [ ] **Every doc has a single home.** `architecture/`,
      `ingestion/`, `indexing/`, `mcp/`, `ops/`, `research/`.
      If you can't decide, update the taxonomy — don't scatter.
- [ ] **Do not create new top-level docs without a reason.** No
      auto-generated `CHANGELOG.md`, `CONTRIBUTING.md`, etc.,
      unless explicitly requested.

---

## Items

- [ ] **Update the relevant `docs/` subdir in the same commit.**
      Changed the router? `docs/mcp/query-routing.md`. Changed
      ingestion? `docs/ingestion/`. Changed a tier? `docs/indexing/`.
      Doc drift is worse than no doc.

- [ ] **Update `TODO.md`.** Tick items finished. Add new items
      uncovered. `TODO.md` is the execution contract — keep it
      honest.

- [ ] **Cross-link.** New doc -> linked from
      `docs/README.md` (or the subdir index) AND from the guide
      it relates to. No orphan pages.

- [ ] **Update `CLAUDE.md` only when a proscription or convention
      changes project-wide.** It is the authoritative source;
      churn has high cost. Prefer docs/ for everything else.

- [ ] **Update tool descriptions.** MCP tool descriptions are
      consumed by LLM agents — they must say what the tool does,
      when to use it, when NOT to, and what to call before/after.
      Treat them as UX, not as code comments.

- [ ] **Update `src/kernel_lore_mcp/_core.pyi` stubs** when the
      Rust surface changed. Without stubs, `ty check` misses
      real errors.
      > Ref: [../pyo3-maturin.md](../pyo3-maturin.md)

- [ ] **Update `__init__.py` exports** when adding public
      Python API. Internal symbols stay prefixed with `_`.
      > Ref: [../design/modules.md](../design/modules.md)

- [ ] **Docstrings on public functions only.** One-line summary
      plus parameter descriptions. Internal helpers with clear
      names and types need none.

- [ ] **Document non-obvious design choices in the commit body.**
      "chose algo A over B because B required positions, which
      our BM25 analyzer disables." The commit body is the place
      future agents look.

- [ ] **Keep `README.md` aligned.** If public MCP tools changed
      or the stack pins shifted, update. Same for
      `docs/ops/deploy.md` when deploy shape changes.

- [ ] **Blind-spot register upkeep.** New ingestion gap? Add it
      to `blind_spots://coverage` — not to a per-response field.

- [ ] **Research notes in `docs/research/` are dated.** Filename
      format: `YYYY-MM-DD-short-title.md`. Findings feed the
      authoritative docs; don't let research pages drift into
      the canon by accident.
