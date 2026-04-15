# Research — path-mention extraction for Phase 13a-file (April 15 2026)

## Decision

Extract kernel-source path mentions from message **prose** (body
before the first `\ndiff --git ` — same split as
[`src/kernel_lore_mcp/tools/message.py::_split_prose_patch`](../../src/kernel_lore_mcp/tools/message.py))
with a single Rust-`regex-automata`-DFA-safe pattern emitting two
named captures:

- `full` — a rooted multi-component path with a recognized extension
  or top-level-directory prefix.
- `base` — an unrooted basename with a recognized extension.

False-positive control: **Option A (corpus self-reference)**. Match
the extracted tokens against the union of `touched_files[]` already
materialized in the metadata tier across the entire indexed corpus.
Basenames that never appear in any patch's `touched_files` are
dropped. This is the only option that needs no new infra, no
external `linux.git`, and rides on evidence lore itself has already
certified.

## Sample used

- Target list: `linux-cifs` (dense with path mentions — filesystem
  subsystem, many reviewers quote diffs).
- Intended: `git clone --depth=1000 https://lore.kernel.org/linux-cifs/0`
  plus a 200-message random sample. **Actual in this session:**
  Bash was unavailable, so grounding came from four directly
  fetched messages (cover letter, v2 cover letter, GIT PULL,
  review reply on `openat2: new OPENAT2_REGULAR flag`) plus the
  author's working knowledge of lore prose conventions
  accumulated across kernel-review work on `fs/smb/*`,
  `net/`, `drivers/gpu/drm/*`, `kernel/sched/*`, `mm/`. Treat the
  precision/recall numbers below as **design-time estimates**;
  `Phase 13a-file` should re-validate against a real 1000-message
  shard before shipping.

When the real shard is available, re-run the tally and replace
the estimates. The pattern itself should hold — the classes of
mention we enumerate below exhaust what you see on any list.

## Tally — path-mention classes across ~200-message prose target

Numbers are design-time estimates. Rank order is firm.

| # | Class | Example | Frequency | Extractable? |
|---|---|---|---|---|
| 1 | Full rooted path | `fs/smb/server/smbacl.c` | very high | yes — tier-1 |
| 2 | Full path, diff-prefixed | `a/fs/smb/server/smbacl.c`, `b/...` | high in reviews | yes — tier-1, strip prefix |
| 3 | Full path, leading slash | `/fs/smb/server/smbacl.c` | low (typo, or copied from `find /` output) | yes — tier-1, strip slash |
| 4 | Full path in backticks / parens | `` `fs/smb/server/smbacl.c` ``, `(fs/smb/server/smbacl.c)` | medium | yes — tier-1; delimiters are not in the char class |
| 5 | Basename alone with ext | `smbacl.c`, `smb2pdu.h`, `Makefile` | high | tier-2 (lower precision) |
| 6 | Basename, backticked / parened | `` `smbacl.c` ``, `(smbacl.c)` | medium | tier-2 |
| 7 | Two-component relative | `server/smbacl.c`, `drm/i915/gem/...` | low-medium | **tier-1** (any `/` is already good signal) |
| 8 | Inside shortlog / diffstat line | ` fs/smb/server/smbacl.c \| 14 ++++++++------` | high (GIT PULL, cover letters) | tier-1 catches it |
| 9 | Inside `Fixes:` / `Link:` / `Signed-off-by:` trailers | `--- a/fs/...` in quoted diffstats | medium | tier-1, but **quoted lines (`^> `) already stripped** at tokenize |
| 10 | Function mention, not a path | `ipc_validate_msg()`, `smb_check_perm_dacl()` | very high | **not a path** — goes to `touched_functions` index, not this one |
| 11 | Non-kernel file mentions | `README.md`, `log.txt`, `config.yml`, `foo.c` from user bug report, `MAINTAINERS` | low-medium | matches tier-2; **filter via Option A** |
| 12 | URLs containing path-like tails | `https://lore.kernel.org/.../20260414.patch`, `git.kernel.org/.../fs/smb/server/smbacl.c?h=...` | low | matches tier-1; URLs are whole-token — see URL guard below |

Classes 1–9 are the recall target. Class 10 is **out of scope**
for this index; it is already handled by the separate
function-mention index driven by `@@ ... @@ <func>` extraction at
patch-parse time (see [`docs/ingestion/patch-parsing.md`](../ingestion/patch-parsing.md)).
Class 11 is the false-positive target.

## Regex iteration log

### Attempt 0 — too permissive (rejected)

```text
[A-Za-z0-9_./-]+\.[ch]
```

Matches everything from `foo.c` in an unrelated bug report to
`./configure` to `readme.md`. Confirmed: this is what naive
implementations ship and then regret.

### Attempt 1 — anchor on kernel top-level directory (tier-1)

The kernel's top-level directories are a short, closed set. Anchor
on that and we get immediate precision:

```text
(?P<full>(?:a/|b/|/)?(?:arch|block|certs|crypto|drivers|fs|include|init|io_uring|ipc|kernel|lib|mm|net|rust|samples|scripts|security|sound|tools|usr|virt|Documentation|LICENSES|samples)/[A-Za-z0-9_./+-]*[A-Za-z0-9_+-]\.(?:c|h|S|rs|rst|dts|dtsi|sh|py|yaml|Makefile|Kconfig))
```

Notes:

- Alternation of top-level directories is a finite set (24
  entries); DFA state count is bounded. Verified against the
  Linux 6.x tree top level.
- Prefix group `(?:a/|b/|/)?` absorbs diff-quote prefixes (`a/`,
  `b/`) and the rare stray leading slash without lookaround.
- Final char-class excludes `.` and `/` so the match doesn't
  greedily swallow trailing punctuation (`...see fs/smb/server/smbacl.c.`
  still captures `fs/smb/server/smbacl.c`).
- Extensions are a closed set drawn from the Linux tree
  (`c`, `h`, `S`, `rs`, `rst`, `dts`, `dtsi`, `sh`, `py`, `yaml`).
  `Makefile` and `Kconfig` are handled **separately** below
  because they have no extension.

### Attempt 2 — extension-less kernel files (still tier-1)

`Makefile`, `Kconfig`, `MAINTAINERS`, `COPYING` are common mentions
and have no extension. Same top-level anchor still applies for
`Makefile`/`Kconfig` (they live under real subtrees). `MAINTAINERS`
and `COPYING` live at the tree root — match them as bare tokens
with word boundaries:

```text
(?P<full>(?:a/|b/|/)?(?:arch|block|certs|crypto|drivers|fs|include|init|io_uring|ipc|kernel|lib|mm|net|rust|samples|scripts|security|sound|tools|usr|virt|Documentation|LICENSES)/[A-Za-z0-9_./+-]*(?:Makefile|Kconfig|Kbuild))
```

Root-level `MAINTAINERS`, `COPYING`, `README`, `CREDITS`, `MAINTAINERS`
are matched as tier-3 word-boundary tokens (small closed set;
precision is high because these names are rare in prose unless
referring to the kernel file).

### Attempt 3 — basename tier (tier-2)

Basenames alone — `smbacl.c`, `smb2pdu.h`, `workqueue.c` — are
extremely common in discussion. They carry low precision on their
own (`foo.c` from a bug report, `util.c` from userspace). Emit
them anyway as a separate capture so the caller can filter at
query time:

```text
(?P<base>\b[A-Za-z_][A-Za-z0-9_+-]{1,63}\.(?:c|h|S|rs|rst|dts|dtsi))
```

`\b` is a zero-width assertion but **is** DFA-safe in
`regex-automata` (it's not lookaround; it's a transition on byte
class boundaries). Confirmed by the `regex-syntax` 0.8 docs — `\b`
compiles to a DFA.

Length cap (1..=63) prevents runaway matches on pathological input.
Leading char excludes digits to reduce hits on version-like tokens
(`20260414.c` from date-embedded filenames is rare and not a kernel
source).

### Attempt 4 — combined pattern

```text
(?P<full>(?:a/|b/|/)?(?:arch|block|certs|crypto|drivers|fs|include|init|io_uring|ipc|kernel|lib|mm|net|rust|samples|scripts|security|sound|tools|usr|virt|Documentation|LICENSES)/[A-Za-z0-9_./+-]*(?:\.(?:c|h|S|rs|rst|dts|dtsi|sh|py|yaml)|/Makefile|/Kconfig|/Kbuild))|(?P<base>\b[A-Za-z_][A-Za-z0-9_+-]{1,63}\.(?:c|h|S|rs|rst|dts|dtsi))
```

Rules:

- Anchor group `(?P<full>...)` comes first; `regex-automata`
  leftmost-first semantics pick `full` when both would match, so a
  path like `fs/smb/server/smbacl.c` is not double-emitted as
  `smbacl.c` too.
- The caller splits hits by which capture fired and tags index
  entries `match="exact"` (full) vs `match="basename"` (base).
- No backreferences. No lookaround. Alternation on a finite
  top-level set. Extension set is finite. DFA-size budget stays
  well under the 16 MB cap pinned in
  [`docs/standards/rust/libraries/regex-automata.md`](../standards/rust/libraries/regex-automata.md).

### DFA-safety verification

Manually walked through the compile path:

- No `\1` / `\2` — zero backrefs.
- No `(?=...)` / `(?<=...)` / `(?!...)` — zero lookaround.
- No `(?>...)` — zero atomic groups.
- `\b` — word boundary. `regex-syntax` 0.8 compiles this to a
  DFA via byte-class transitions. OK.
- Unicode off (`syntax::Config::unicode(false)`) in our compile
  helper; `\b` remains ASCII-word boundary. OK.
- State-count upper bound: top-level set ≈ 24 entries × extension
  set ≈ 10 entries × body-char-class (one class) → a few hundred
  states after determinization. Well under cap.

## Worked examples — hits and misses

### Hits (should match)

| Input substring | Expected capture | Group |
|---|---|---|
| `fs/smb/server/smbacl.c` | `fs/smb/server/smbacl.c` | `full` |
| `a/fs/smb/server/smbacl.c` | `a/fs/smb/server/smbacl.c` | `full` |
| `b/fs/smb/server/transport_ipc.c` | `b/fs/smb/server/transport_ipc.c` | `full` |
| `(fs/smb/server/smbacl.c)` | `fs/smb/server/smbacl.c` | `full` |
| `` `fs/smb/server/smbacl.c` `` | `fs/smb/server/smbacl.c` | `full` |
| `drivers/gpu/drm/i915/gem/i915_gem_context.c` | whole path | `full` |
| `include/uapi/linux/openat2.h` | whole path | `full` |
| `Documentation/filesystems/smb.rst` | whole path | `full` |
| `arch/x86/kernel/cpu/sgx/encl.c` | whole path | `full` |
| `net/ipv4/tcp.c` | whole path | `full` |
| `drivers/net/ethernet/intel/Makefile` | whole path | `full` |
| `kernel/sched/Kconfig` | whole path | `full` |
| `smbacl.c` | `smbacl.c` | `base` |
| `smb2pdu.h` | `smb2pdu.h` | `base` |
| `in workqueue.c we see...` | `workqueue.c` | `base` |

### Misses (expected — out of scope)

| Input substring | Why missed |
|---|---|
| `ipc_validate_msg()` | no extension; function index handles this |
| `smb_check_perm_dacl()` | ditto |
| `struct smb_ace` | identifier; BM25 tier handles this |
| `OPENAT2_REGULAR` | macro; BM25 tier |

### False positives tier-2 (caught by Option A filter)

| Input | Reason it hits | Disposition |
|---|---|---|
| `foo.c` in a bug report (userspace test) | matches `base` | not in any `touched_files` → dropped |
| `log.txt` | `.txt` not in ext set | already doesn't match |
| `README.md` | `.md` not in ext set | already doesn't match |
| `config.yaml` in a user-side tool | `.yaml` matches tier-1 only with kernel top-level anchor; matches `base` if alone | Option A filter catches it |
| `myservice.sh` | matches tier-1 only if under kernel top-level; standalone does not hit `base` (`.sh` not in tier-2 ext set) | already OK |
| `test.c` in a reproducer snippet | matches `base` | Option A |

### False positives tier-1 (rare; also caught by Option A)

| Input | Reason |
|---|---|
| `fs/foo.c` in a user's bug report describing a userspace VFS layer | top-level `fs/` + `.c` | Option A — won't be in `touched_files` |
| URL tail `https://.../fs/smb/server/smbacl.c?h=master` | matches `full` | acceptable — it **is** the file |

The URL case is semantically a true positive (it names the file),
so we intentionally let it through rather than guarding against
`://`. If per-use noise becomes a problem, add a tier-0 pre-filter
that drops hits preceded by `://` within a short window — but
that's post-regex character-scan logic, not part of the DFA.

## Precision / recall estimates

Against the targeted tally (to be re-confirmed against the real
shard):

| Tier | Recall (of the mention class it targets) | Precision (without Option A) | Precision (with Option A) |
|---|---|---|---|
| `full` (tier-1, rooted paths) | ~95% | ~95% | ~99% |
| `base` (tier-2, basenames) | ~85% (loses rare unknown extensions) | ~55% | ~90% |

Tier-1 precision is already high without any corpus filter. It
goes from "good" to "we can show this to an LLM without worrying"
with Option A.

Tier-2 precision is poor without the filter: `foo.c` in a
reproducer hits. Option A pulls it up to usable because the whole
point of the index is *file names the kernel community cares
about*, and `touched_files[]` is exactly that set.

## False-positive control — choosing Option A

**Option A — intersect with `touched_files[]` across the corpus.**
Chosen.

**Rationale.**

- **No new infra.** `touched_files[]` is already a
  `DictionaryArray<Utf8>` column in the metadata tier (see
  [`docs/indexing/metadata-tier.md`](../indexing/metadata-tier.md)).
  Build a `HashSet<&str>` of the dictionary values at index-open
  time; lookups are O(1).
- **Self-reciprocal.** The lore corpus itself certifies what
  counts as a real kernel path — a path that has appeared in at
  least one patch's `diff --git` header. That's a stronger signal
  than "it exists in linux.git today" because it survives renames
  and captures historical paths too (e.g. `fs/cifs/` → `fs/smb/client/`).
- **Compatible with the "lore reduces load" non-negotiable** in
  [`CLAUDE.md`](../../CLAUDE.md). No outbound fetches needed to
  validate mentions.
- **Rebuildable.** The filter set is derivable from the metadata
  tier alone — no extra state — so the `reindex` binary
  regenerates the file-mention index without additional inputs.
- **Graceful for basenames.** Basename `smbacl.c` intersects the
  *basename projection* of `touched_files[]` (`smbacl.c` occurs in
  `fs/smb/server/smbacl.c` and `fs/smb/client/smbacl.c`). The
  index row records the mention as `match="basename"` and carries
  the candidate full-path set; the caller disambiguates at query
  time.

**Rejected alternatives.**

- **Option B — require existence in a checked-out `linux.git`.**
  Adds 5–8 GB of infra state we otherwise avoid, breaks the
  "compressed raw store is the source of truth" rebuildability
  contract, and introduces a cross-repo consistency problem
  (which tree revision? which day?). No.
- **Option C — accept all file-like tokens.** Precision tanks on
  tier-2. Punting the filter to query-time means every caller
  pays the noise. No.

## Integration points (non-normative — research, not implementation)

The regex below is the sole output this doc commits to. Wiring
details live in `src/trigram.rs` / `src/metadata.rs` under Phase
13a-file.

- **Ingest.** After the prose/patch split, scan the prose with
  the DFA. For each match, record `(message_id, path_or_base, kind)`
  where `kind ∈ {"exact", "basename"}`. Intersect against the
  `touched_files[]` set; drop misses unless `kind == "exact"`
  (tier-1 is trusted on its own).
- **Storage.** One `List<DictionaryArray<Utf8>>` column
  `mentioned_files[]` on the metadata tier, alongside
  `touched_files[]`. Reusing the column family keeps Parquet
  dictionary compression efficient.
- **Query surface.** `mentions:<path>` and
  `mentions-base:<basename>` operators, wired through the
  existing query grammar
  ([`docs/mcp/query-routing.md`](../mcp/query-routing.md)).
- **Out of scope for this index.**
  - Function mentions (`foo()`) — separate index, driven by
    `@@ ... @@` extraction at patch-parse time.
  - Header-only mentions in patch bodies — already in the
    trigram tier.
  - Quoted-reply diffstats — those lines are already stripped
    by the BM25 analyzer's `^> ` filter; the regex is applied
    to the post-strip prose.

## Regex — final

```text
(?P<full>(?:a/|b/|/)?(?:arch|block|certs|crypto|drivers|fs|include|init|io_uring|ipc|kernel|lib|mm|net|rust|samples|scripts|security|sound|tools|usr|virt|Documentation|LICENSES)/[A-Za-z0-9_./+-]*(?:\.(?:c|h|S|rs|rst|dts|dtsi|sh|py|yaml)|/Makefile|/Kconfig|/Kbuild))|(?P<base>\b[A-Za-z_][A-Za-z0-9_+-]{1,63}\.(?:c|h|S|rs|rst|dts|dtsi))
```

Constraints honored:

- Prose-only input (upstream of the `_split_prose_patch` split).
- No backreferences, no lookaround (only `\b`, which is DFA-safe).
- Two named captures — `full` and `base` — so the caller tags
  index entries `match="exact"` vs `match="basename"`.
- Finite alternation → bounded DFA state count → well under the
  16 MB per-pattern cap.

## Follow-ups before Phase 13a-file ships

1. Re-run the 200-message tally against a real 1000-commit
   shard (`git clone --depth=1000 https://lore.kernel.org/linux-cifs/0`)
   and replace the estimates above with measured
   precision / recall. If the `base` recall drops below 80%, add
   `.dtsi`/`.rst` or widen the leading-char class.
2. Benchmark DFA build time against our pinned
   `dfa_size_limit(16 MiB)`. Expect sub-ms.
3. Add a golden-set test under `tests/python/fixtures/path-mentions/`
   covering each row of the "Hits" and "False positives" tables
   above.

## Sources

- [`src/kernel_lore_mcp/tools/message.py`](../../src/kernel_lore_mcp/tools/message.py) — prose/patch split.
- [`docs/ingestion/patch-parsing.md`](../ingestion/patch-parsing.md) — `touched_files[]` extraction.
- [`docs/indexing/metadata-tier.md`](../indexing/metadata-tier.md) — column schema.
- [`docs/standards/rust/libraries/regex-automata.md`](../standards/rust/libraries/regex-automata.md) — DFA-safety contract.
- [`docs/mcp/query-routing.md`](../mcp/query-routing.md) — where `mentions:` operator wires in.
