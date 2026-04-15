"""Phase 11 — server-provided prompts (slash commands).

Five prompts that encode the canonical kernel-research workflows so
agents can invoke them via `/mcp__kernel-lore__<prompt_name>` in
Claude Code (and via equivalent surfaces in Codex / Cursor as those
mature).

Invariants:

* Every argument carries a Python default. Required arguments are
  blocked by anthropics/claude-code#30733 (Claude Code lists the
  prompt but does not prompt the user for required fields, so the
  slash command is effectively unusable if a required arg exists).

* Prompt bodies return plain `str`. FastMCP
  (`fastmcp/prompts/base.py:285-319`) wraps a `str` into a single
  user-role `TextContent` message; that's the minimal shape.

* Every tool name referenced in a prompt body must exist in the
  live registry — if a phase slips, the prompt still renders but
  the agent would burn a turn on a tool-not-found error. The
  prompt-bodies-reference-real-tools invariant is pinned in
  `tests/python/test_prompts.py`.

* Scope is read-only guidance. These prompts nudge the agent
  through tool sequences; they do not call tools themselves.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from fastmcp import FastMCP


def _novelty_body(
    subsystem: str,
    vuln_class: str,
    window_months: int,
) -> str:
    subsystem = subsystem or "<subsystem>"
    vuln_class = vuln_class or "<vulnerability class, e.g. UAF / OOB / double-free>"
    return f"""\
You are about to check whether a potential vulnerability in
`{subsystem}` of class `{vuln_class}` has been reported before.
Work the steps in order; stop as soon as you can answer
"has this been reported?" with high confidence.

Window: last {window_months} months.

1. Call `lore_expand_citation(token="{vuln_class}")` plus any CVE
   IDs the user has shared — the expansion covers Message-IDs,
   SHAs, and CVE IDs all in one pass.

2. Call `lore_search` with a lei-compatible query that scopes to
   the subsystem:
       list:linux-{subsystem}  OR  dfn:path/under/{subsystem}/
       {vuln_class.lower()}
   If the `{subsystem}` mapping to a mailing list is ambiguous,
   read `blind-spots://coverage` once — it enumerates what is NOT
   in the index.

3. Call `lore_patch_search(needle="<distinctive token from the
   reproducer>")` if the user has a crash dump or syz-prog.

4. Call `lore_regex` with a DFA-only pattern against the `patch`
   field only if step 3 was too narrow. Keep the pattern
   anchored.

5. For any hit ranked > 0, follow up with `lore_thread` to see
   whether resolution has already landed. Cross-reference
   `Fixes:` trailers via `lore_substr_trailers(name="fixes",
   value_substring="<sha>")`.

Deliverables for the user:
 * "Reported / not reported" judgement with citations
   (message-id + lore URL + cite_key on every claim).
 * If reported: link to the fixing commit / stable backport
   chain when visible.
 * If not: a one-line novelty statement the user can paste into
   their disclosure workflow.

Blind spot reminder: private security@kernel.org traffic,
distro vendor backports, syzbot pre-public findings, and
embargoed CVEs are NOT in this index. If the vulnerability
class is highly sensitive, state the blind-spot caveat
explicitly in your answer.
"""


def _cve_chain_body(cve_id: str) -> str:
    cve_id = cve_id or "<CVE-YYYY-NNNNN>"
    return f"""\
Expand `{cve_id}` into: introducing commit → fix commit →
stable backport chain → pre-disclosure discussion.

Phase-18C tool (`lore_cve_chain`) is not yet shipped. Until
it lands, compose the answer from today's primitives:

1. `lore_expand_citation(token="{cve_id}")` — lands on the
   linux-cve-announce message (if present in the corpus).

2. If the announce message carries `Fixes:`, call
   `lore_substr_trailers(name="fixes", value_substring="<sha>")`
   to find every patch that cites the culprit. Group by
   `list` to separate mainline vs. stable postings.

3. `lore_series_timeline(message_id=<any hit>)` — collapses
   sibling v1/v2/v3 postings of the fix.

4. `lore_regex(field="prose", pattern="{cve_id.replace("-", r"\\\\-")}")`
   — catches mentions that aren't structured trailers.

5. Assemble: CVE → fix commit → which stable branches shipped
   it → who reviewed it. Always cite message-id + lore URL.

If the CVE ID is outside our lore coverage window, fall back
to `blind-spots://coverage` and report honestly. External data
sources (CVE V5 JSON, Red Hat CSAF, Debian tracker) ship in
Phase 18C — they are NOT in the index today.
"""


def _series_diff_body(message_id: str) -> str:
    mid = message_id or "<message-id of any version>"
    return f"""\
Tour the version history of the patch series rooted at
`{mid}`. Goal: a one-paragraph summary of what changed between
each consecutive (vN, vN+1) plus any open Reviewed-by threads.

1. `lore_series_timeline(message_id="{mid}")` — returns every
   sibling version, ordered by (series_version, series_index).

2. For each (vN, vN+1) pair of the *cover letter* (or the
   corresponding series_index if no cover), call
   `lore_patch_diff(a=<cover of vN>, b=<cover of vN+1>)`.
   Start with `response_format=concise` to get a token-budget-
   friendly summary; only escalate to `detailed` if you need
   line-accurate detail.

3. For each version, `lore_expand_citation(token=<version's
   message-id>)` to pick up Reviewed-by / Acked-by trailers
   + any downstream replies.

4. Emit a table: version → date → summary-of-deltas →
   outstanding reviewer concerns.

Always cite message-ids + lore URLs for every claim.
"""


def _reviewers_body(file_path: str, window_months: int) -> str:
    file_path = file_path or "<path/under/torvalds/linux>"
    return f"""\
Rank humans most likely to review a patch touching
`{file_path}` in the last {window_months} months. Output a
top-10 list sorted by Reviewed-by frequency, with cite_keys.

1. `lore_activity(file="{file_path}", response_format="detailed",
   limit=500)` — returns every message touching this path.

2. For each row, count trailers per-author:
   * `reviewed_by[]`      (strongest signal)
   * `acked_by[]`         (moderate)
   * `tested_by[]`        (weak)

3. Group by `from_addr` extracted from trailer strings. De-dupe
   via the `<local@host>` tail (some contributors rotate
   canonical addresses).

4. For the top 10, pull a recent example via
   `lore_eq(field="reviewed_by", value="<mangled-form>",
   limit=1)` so the user has a pointer to the reviewer's
   voice on this file.

Reviewer-recommendation shipping as its own tool in Phase 10.9
of the roadmap — until then, this prompt encodes the sequence.
"""


def _cross_subsystem_body(
    pattern: str,
    from_subsystem: str,
    to_subsystems: str,
) -> str:
    pattern = pattern or "<literal string or DFA regex>"
    from_subsystem = from_subsystem or "<subsystem where pattern was found>"
    to_subsystems = to_subsystems or "sunrpc, SCSI, RDMA"
    return f"""\
The canonical high-value workflow: "this XDR overflow pattern
is in my NFS series — is the same shape present in
{to_subsystems}?"

Pattern: `{pattern}`
Origin: `{from_subsystem}`
Targets: {to_subsystems}

1. Verify the pattern in `{from_subsystem}` first so the
   baseline is honest:
       lore_patch_search(needle="{pattern}", list="linux-{from_subsystem}")
   If the pattern is a regex rather than a literal, switch to
       lore_regex(field="patch", pattern="{pattern}",
                  anchor_required=True, list="linux-{from_subsystem}")

2. For each target in `{to_subsystems}`, rerun the same tool
   with the target's list:
       lore_patch_search(needle="{pattern}", list="linux-<target>")

3. When you hit a match in the target, pull the full context
   via `lore_explain_patch(message_id=<hit>)` — that one call
   bundles prose + patch + series + downstream replies so you
   can judge whether the match is the same structural bug.

4. Emit a per-target table:
       target | hits | first-hit cite_key | status (open/landed)
   On any "landed" hit, pair the fix commit via
   `lore_substr_trailers(name="fixes", value_substring="<sha>")`.

5. If a subsystem shows zero hits, state so explicitly — the
   negative result is load-bearing for the user's disclosure
   decision.

Cite message-id + lore URL on every positive match.
Use the blind-spots resource (`blind-spots://coverage`) to
caveat the "zero hits" cases — absence from lore is not
absence from the kernel.
"""


def register_prompts(mcp: FastMCP) -> None:
    """Wire the 5 Phase-11 prompts onto `mcp`.

    Kept in a single function so `build_server()` stays scannable;
    every prompt's body lives in its own private `_…_body` helper
    above so the bodies can be unit-tested in isolation.
    """

    @mcp.prompt(
        name="klmcp_pre_disclosure_novelty_check",
        description=(
            "Walk the agent through checking whether a potential kernel "
            "vulnerability has been reported before. Leans on lore_search "
            "+ lore_patch_search + lore_regex + lore_thread."
        ),
        tags={"kernel-lore", "workflow", "security"},
    )
    def _novelty(
        subsystem: str = "",
        vuln_class: str = "",
        window_months: int = 6,
    ) -> str:
        """Run a novelty check for a potential vulnerability.

        Args:
            subsystem: Kernel subsystem (e.g. 'cifs', 'nfs', 'io_uring').
            vuln_class: Vulnerability class (e.g. 'UAF', 'OOB', 'double-free').
            window_months: How far back to look in the corpus.
        """
        return _novelty_body(subsystem, vuln_class, window_months)

    @mcp.prompt(
        name="klmcp_cve_chain_expand",
        description=(
            "Expand a CVE ID into introducing commit + fix commit + "
            "stable backports + pre-disclosure discussion, using "
            "today's primitives. Phase-18C-aware."
        ),
        tags={"kernel-lore", "workflow", "cve"},
    )
    def _cve_chain(cve_id: str = "") -> str:
        """Expand a CVE ID into its full commit chain.

        Args:
            cve_id: CVE identifier in canonical form, e.g. 'CVE-2024-26924'.
        """
        return _cve_chain_body(cve_id)

    @mcp.prompt(
        name="klmcp_series_version_diff",
        description=(
            "Tour what changed between vN and vN+1 of a patch series, "
            "including Reviewed-by deltas. Uses lore_series_timeline + "
            "lore_patch_diff."
        ),
        tags={"kernel-lore", "workflow", "series"},
    )
    def _series_diff(message_id: str = "") -> str:
        """Walk the version history of a patch series.

        Args:
            message_id: Any Message-ID inside the series (we pick up the
                siblings via series_version / series_index).
        """
        return _series_diff_body(message_id)

    @mcp.prompt(
        name="klmcp_recent_reviewers_for",
        description=(
            "Rank reviewers for a source-tree path by recent "
            "Reviewed-by frequency. Uses lore_activity + trailer scan."
        ),
        tags={"kernel-lore", "workflow", "reviewers"},
    )
    def _reviewers(file_path: str = "", window_months: int = 12) -> str:
        """Recommend reviewers for a given file path.

        Args:
            file_path: Path under torvalds/linux (e.g. 'fs/smb/server/smbacl.c').
            window_months: Time window for trailer scan.
        """
        return _reviewers_body(file_path, window_months)

    @mcp.prompt(
        name="klmcp_cross_subsystem_pattern_transfer",
        description=(
            "Look for a pattern that was observed in one subsystem in "
            "other subsystems — 'is this XDR overflow also in sunrpc/"
            "SCSI/RDMA?'. Highest-leverage workflow per the canonical "
            "user's CLAUDE.md."
        ),
        tags={"kernel-lore", "workflow", "cross-subsystem", "security"},
    )
    def _cross_subsystem(
        pattern: str = "",
        from_subsystem: str = "",
        to_subsystems: str = "",
    ) -> str:
        """Transfer a bug-pattern search across subsystems.

        Args:
            pattern: Literal token or DFA regex to search for.
            from_subsystem: Subsystem where the pattern was first seen.
            to_subsystems: Comma-separated list of targets to cross-check.
        """
        return _cross_subsystem_body(pattern, from_subsystem, to_subsystems)


__all__ = ["register_prompts"]
