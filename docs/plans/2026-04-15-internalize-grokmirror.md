# Plan: internalize grokmirror into the Rust/PyO3 layer

**Status:** proposed for v0.2.0.
**Author:** Michael Bommarito + Claude Opus 4.6.
**Date:** 2026-04-15.
**Depends on:** v0.1.x stabilization (all review-round bugs landed).

## Motivation

Today the sync pipeline has three moving parts:

```
grok-pull (Python, grokmirror 2.0.12)
  → post-pull-hook.sh (touch trigger file)
    → klmcp-ingest.sh → kernel-lore-ingest (Rust or Python CLI)
```

Three processes, two languages, a systemd timer + path-trigger +
debounce dance, a template-rendered config file with `@VAR@`
substitution, and a runtime dep on the `grokmirror` Python package
that has its own dep chain (`requests`, `urllib3`, etc.).

All of this exists because we adopted grokmirror as a black box.
But grokmirror does exactly three things we care about:

1. **HTTP GET `manifest.js.gz`** — fetch + gzip-decode + JSON parse.
2. **Diff fingerprints** — compare each shard's `fingerprint` field
   against a local cache to decide what changed.
3. **`git fetch`** per changed shard — smart-protocol delta packfile.

Steps 1–2 are ~50 LOC. Step 3 is a `gix` fetch, which we already
carry as a dep (`gix 0.81` with `max-performance-safe`).

Internalizing replaces the three-process pipeline with:

```
kernel-lore-sync --data-dir $KLMCP_DATA_DIR
  (one binary: fetch manifest → diff → gix fetch → ingest → bump gen)
```

## What we gain

### 1. Remove the grokmirror Python dep

- `grokmirror` pulls `requests`, `urllib3`, `charset-normalizer`,
  `idna`, `certifi`. None of these are needed by the MCP server
  itself.
- `uv tool install grokmirror` is an extra step in the quickstart
  that confuses users ("why do I need two Python packages?").
- grokmirror's config format is its own INI dialect; our
  `@VAR@` template-rendering hack in `klmcp-grok-pull.sh` is
  fragile and already caused a bug (the first attempt used
  `%(VAR)s` interpolation which ConfigParser doesn't expand from
  env).

### 2. Atomic pull + ingest in one process

Today:
- `grok-pull` finishes → touches a trigger file.
- `klmcp-ingest.path` (systemd) fires `klmcp-ingest.service`.
- The ingest script checks a debounce timestamp to avoid racing.

With an internal sync:
- One binary holds the writer lock for the entire pull + ingest.
- Generation bumps exactly once, AFTER ingest + BM25 commit + tid
  rebuild (the ordering bug we just fixed in round 2).
- No trigger file, no debounce, no race window between "shards
  are on disk" and "ingest has started."

### 3. Safety

grokmirror shells out to `git fetch` via `subprocess`. Our gix-
based walker already does incremental walks without subprocess;
extending it to also do the fetch is a natural fit.

The writer lock (`state/writer.lock`) covers the full pipeline so
concurrent sync invocations are a clean no-op (flock fails, exit 0).

### 4. Performance

grokmirror is serial per-shard. Our Rust binary can rayon-fan-out
across shards for the fetch step (network-bound, each shard is
independent) AND for the ingest step (CPU-bound). For lkml's 19
shards, this cuts wall-clock by ~3–5x on a multi-core box.

### 5. Manifest diffing is trivial

The lore manifest shape (verified against live
`lore.kernel.org/manifest.js.gz`, April 2026):

```json
{
  "/netdev/git/0.git": {
    "fingerprint": "abc123...",
    "modified": 1713196800,
    "reference": null,
    "symlinks": [],
    "owner": "netdev"
  },
  ...
}
```

Diffing against a local JSON cache:

```rust
let remote: HashMap<String, ShardMeta> = serde_json::from_reader(resp)?;
let local: HashMap<String, String> = load_local_fingerprints(data_dir)?;
let changed: Vec<&str> = remote.iter()
    .filter(|(path, meta)| local.get(*path) != Some(&meta.fingerprint))
    .map(|(path, _)| path.as_str())
    .collect();
```

~10 LOC, zero ambiguity.

### 6. No template-rendering hack

The binary reads `Settings.data_dir` directly. The grokmirror.conf
template, the `@KLMCP_DATA_DIR@` sed substitution, and the
`KLMCP_GROKMIRROR_CONF_TEMPLATE` env var all go away.

## What we lose (and why it's fine)

### grokmirror's objstore dedup

grokmirror can share git objects across shards via a shared
object-store directory, reducing disk usage when multiple shards
reference the same packfiles. We don't use this — each shard is
an independent bare repo. The 4 netdev shards total ~3.9 GB; with
objstore dedup they might be ~3.2 GB. The ~700 MB saving isn't
worth the complexity for a single-list mirror, and at full-corpus
scale the bottleneck is network, not disk.

### grokmirror's fsck / gc

grokmirror can run `git fsck` and `git gc` on mirrored repos. We
never modify the shards (read-only after fetch), so fsck/gc are
unnecessary. If a shard is corrupt, gix's open will fail and we
log the error; the next pull replaces the shard.

### grokmirror's battle-testing

grokmirror is maintained by Konstantin Ryabitsev and has been
stable for 10+ years. Our replacement is new code.

Mitigations:
- The replacement is <200 LOC of HTTP + JSON + gix fetch.
- We already test gix walks end-to-end in 72 Rust tests.
- The manifest diff is a deterministic pure function (easy to test).
- The HTTP fetch is a single `reqwest::get()` — not a protocol
  implementation.
- We keep grokmirror as a documented alternative in the runbook
  for operators who prefer the battle-tested path.

## Implementation plan

### New files

```
src/sync.rs              — manifest fetch + diff + gix fetch
src/bin/sync.rs          — CLI: kernel-lore-sync
src/kernel_lore_mcp/cli/sync.py  — Python wrapper (for wheel users)
```

### Dependencies (Rust)

```toml
# Already in dep tree via gix:
# reqwest (or ureq for sync/no-tokio)
# serde_json (already explicit)

# New explicit dep for HTTP:
ureq = "3"   # tiny, sync, no tokio, TLS via rustls (already in gix)
```

`ureq` is preferred over `reqwest` because:
- Sync API (no async runtime needed; the binary is single-threaded
  for the HTTP step, rayon for the fan-out).
- Tiny: ~500 KB binary delta.
- TLS via `rustls` which gix already vendors.

### `src/sync.rs` — core logic

```rust
pub struct SyncResult {
    pub shards_checked: usize,
    pub shards_changed: usize,
    pub shards_fetched: usize,
    pub shards_failed: usize,
    pub messages_ingested: u64,
}

/// One-shot: fetch manifest, diff, fetch changed shards, ingest,
/// bump generation.
pub fn sync_once(
    data_dir: &Path,
    manifest_url: &str,       // default: https://lore.kernel.org/manifest.js.gz
    include: &[&str],         // fnmatch patterns (default: ["*"])
    exclude: &[&str],         // fnmatch patterns (default: [])
) -> Result<SyncResult> {
    // 1. Fetch + decompress manifest.
    let manifest = fetch_manifest(manifest_url)?;
    
    // 2. Load local fingerprint cache.
    let local_fps = load_fingerprints(data_dir)?;
    
    // 3. Diff.
    let changed = diff_manifest(&manifest, &local_fps, include, exclude);
    
    // 4. Acquire writer lock.
    let state = State::new(data_dir)?;
    let _lock = state.acquire_writer_lock()?;
    
    // 5. Fetch changed shards via gix (rayon fan-out).
    let fetched = fetch_shards(data_dir, &changed)?;
    
    // 6. Ingest changed shards (existing pipeline).
    let stats = ingest_changed(data_dir, &fetched)?;
    
    // 7. Bump generation ONCE.
    state.bump_generation()?;
    
    // 8. Save updated fingerprints.
    save_fingerprints(data_dir, &manifest)?;
    
    Ok(SyncResult { ... })
}
```

### `src/bin/sync.rs` — CLI

```
kernel-lore-sync
    --data-dir PATH          (or $KLMCP_DATA_DIR)
    --manifest-url URL       (default: https://lore.kernel.org/manifest.js.gz)
    --include PATTERN        (repeatable; default: *)
    --exclude PATTERN        (repeatable; default: none)
    --dry-run                (fetch manifest + diff, don't fetch/ingest)
    --json                   (output SyncResult as JSON)
```

### Python wrapper

```python
# src/kernel_lore_mcp/cli/sync.py
# Exposed as `kernel-lore-sync` console script in the wheel.
# Calls _core.sync_once() via PyO3.
```

### `pyproject.toml` change

```toml
[project.scripts]
kernel-lore-mcp    = "kernel_lore_mcp.__main__:main"
kernel-lore-ingest = "kernel_lore_mcp.cli.ingest:main"
kernel-lore-embed  = "kernel_lore_mcp.cli.embed:main"
kernel-lore-sync   = "kernel_lore_mcp.cli.sync:main"    # NEW
```

### Systemd simplification

Before (3 units):
```
klmcp-grokmirror.timer → klmcp-grokmirror.service (grok-pull)
klmcp-ingest.path → klmcp-ingest.service (kernel-lore-ingest)
```

After (1 unit):
```
klmcp-sync.timer → klmcp-sync.service (kernel-lore-sync)
```

Timer fires every 5 min. Service runs `kernel-lore-sync --data-dir
$KLMCP_DATA_DIR`. Done. No trigger files, no debounce, no
template rendering.

### Config change

```python
# Settings gains:
manifest_url: str = Field(
    default="https://lore.kernel.org/manifest.js.gz",
    description="grokmirror manifest URL."
)
sync_include: list[str] = Field(
    default_factory=lambda: ["*"],
    description="fnmatch patterns for shard inclusion."
)
sync_exclude: list[str] = Field(
    default_factory=list,
    description="fnmatch patterns for shard exclusion."
)
```

### gix fetch: what we need

gix's `Repository::find_remote("origin")?.fetch()` does a smart-
protocol fetch that:
- Negotiates wants/haves with the remote.
- Downloads only the delta packfile.
- Updates the local refs.

This is exactly what `git fetch origin` does, minus the subprocess.
gix handles the protocol, packfile decoding, and ref updates
natively. We already depend on `gix 0.81` with the `revision` +
`parallel` features.

The one thing we need that we don't currently use is gix's
**network client**. Our `Cargo.toml` deliberately excludes
`blocking-network-client` (line 37 in CLAUDE.md). To enable fetch,
we add:

```toml
gix = { version = "0.81", features = [
    "max-performance-safe",
    "revision",
    "parallel",
    "sha1",
    "blocking-network-client",  # NEW — enables gix fetch
] }
```

This adds `gix-protocol`, `gix-transport`, and their deps to the
build. Binary size impact: ~1-2 MB. Acceptable.

### Alternative: keep grokmirror as a fallback

For operators who prefer not to trust our fetch code, the runbook
keeps the grokmirror path as a documented alternative:

```
# Option A (recommended): built-in sync
kernel-lore-sync --data-dir $KLMCP_DATA_DIR

# Option B (legacy): external grokmirror
uv tool install grokmirror
KLMCP_GROKMIRROR_CONF_TEMPLATE=... ./scripts/klmcp-grok-pull.sh
kernel-lore-ingest --data-dir ... --lore-mirror ...
```

Both produce the same on-disk layout; the MCP server doesn't care
which path populated the shards.

## Migration path

1. **v0.1.x (now):** grokmirror + external ingest. Works, tested,
   shipped.
2. **v0.2.0-alpha:** ship `kernel-lore-sync` alongside the existing
   scripts. Both paths work; the sync binary is opt-in. Test against
   the full 390-shard corpus.
3. **v0.2.0:** `kernel-lore-sync` is the default in the quickstart
   and systemd units. grokmirror path stays documented in the
   runbook as the legacy option.
4. **v0.3.0:** remove `scripts/grokmirror*.conf`,
   `scripts/klmcp-grok-pull.sh`, `scripts/klmcp-ingest.sh`,
   `scripts/post-pull-hook.sh`, and the `klmcp-ingest.path` +
   `klmcp-grokmirror.*` systemd units. grokmirror is no longer
   mentioned in the quickstart.

## Effort estimate

| Component | Effort | Notes |
|---|---|---|
| `src/sync.rs` (manifest + diff + fetch) | 1–2 d | Manifest parse is trivial; gix fetch is well-documented |
| `src/bin/sync.rs` + Python wrapper | 0.5 d | Same pattern as ingest binary |
| Tests (manifest parse, diff, mock fetch) | 1 d | Mock the HTTP layer; real gix fetch test against lore |
| Systemd unit simplification | 0.5 d | Replace 3 units with 1 |
| Runbook + README update | 0.5 d | |
| **Total** | **3–4 d** | |

## Risks

1. **gix fetch on public-inbox repos.** public-inbox v2 repos have
   unusual ref layouts (`refs/heads/master` only, all history
   linear). gix should handle this; confirm with a test fetch
   against one real shard before shipping.

2. **TLS cert handling.** `ureq` uses `rustls` by default. Verify
   that `lore.kernel.org`'s cert chain validates under rustls's
   `webpki-roots` (it should — kernel.org uses Let's Encrypt).

3. **Rate limiting by kernel.org.** Our manifest fetch is a single
   2 MB GET every 5 min — identical to what grokmirror does. The
   per-shard fetches are git-protocol, same as grokmirror. No
   change in load profile from kernel.org's perspective.

4. **Binary size.** Adding `blocking-network-client` to gix grows
   the binary by ~1-2 MB. Acceptable for a tool that already ships
   a 12 MB .so.

## Non-goals

- **NNTP.** Some mirrors use NNTP instead of git. We don't. If
  someone needs NNTP, they use the grokmirror legacy path.
- **Push-based / pubsub.** grokmirror v3 added SNS-like pubsub.
  We poll on a 5-min cadence; the latency gain from push (~30s vs
  ~5 min p50) doesn't justify the complexity for v0.2.0. Revisit
  when the hosted instance is live.
- **Multi-upstream.** We mirror one upstream (lore.kernel.org). If
  someone wants to mirror a different public-inbox instance, they
  pass a different `--manifest-url`. Multi-upstream fan-in is not
  a v0.2.0 goal.

## References

- [grokmirror on korg docs](https://korg.docs.kernel.org/grokmirror.html)
- [grokmirror GitHub](https://github.com/mricon/grokmirror)
- [gix fetch documentation](https://docs.rs/gix/latest/gix/struct.Remote.html#method.connect)
- [gix blocking-network-client feature](https://github.com/Byron/gitoxide/blob/main/Cargo.toml)
- [ureq crate](https://docs.rs/ureq/)
- [lore manifest format](https://lore.kernel.org/manifest.js.gz)
- [`docs/ops/update-frequency.md`](../ops/update-frequency.md) — cadence policy
- [`CLAUDE.md`](../../CLAUDE.md) § "Non-negotiable product constraints"
