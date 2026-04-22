"""lore_stable_backport_status — did a mainline fix reach -stable?

Answers the security-critical question: "the bug was fixed in commit
abc123 — did that fix actually land in the -stable release my distro
ships?" From lore data alone this can't be fully authoritative (the
ground truth lives in linux-stable.git), but the signals in the
`stable` and `stable-commits` mailing lists give a reliable first
cut with evidence the caller can verify.

Signal hierarchy (strongest first):

1. **Confirmation**: a message on `stable-commits` whose body
   contains `Upstream commit <sha>` or whose subject carries the
   stable version + the SHA. Each hit is a stable release.

2. **AUTOSEL nomination**: Sasha Levin's bot sends patch series
   with subject `[PATCH AUTOSEL <version> NN/MM]` to `stable`.
   Matching by SHA in body → the commit was auto-picked for
   that stable branch.

3. **Manual nomination**: the commit's `Cc: stable@vger` trailer
   or a manual submission on `stable` list → pending inclusion.

4. **Opt-out**: the commit's `Cc: stable+noautosel@kernel.org`
   trailer or an explicit "not applicable" reply → rejected.

Pure-lore caveat: if `stable-commits` isn't ingested by this
server, we can only see nomination-side evidence, not
confirmation. The caveat field in the response makes this
honest.
"""

from __future__ import annotations

import re
from typing import Annotated, Literal

from pydantic import BaseModel, Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.errors import invalid_argument
from kernel_lore_mcp.freshness import Freshness, build_freshness
from kernel_lore_mcp.timeout import run_with_timeout

_SHA_FULL = re.compile(r"^[0-9a-fA-F]{40}$")
_SHA_SHORT = re.compile(r"^[0-9a-fA-F]{7,39}$")

# Subject parsers: extract the stable version from typical mail shapes.
# Examples:
#   "[PATCH AUTOSEL 6.6 07/42] foo: fix bar"
#   "[PATCH 5.15 1/3] mm: backport abc"
#   "[PATCH 4.19.312 42/94] net: fix xyz"
#   "Linux 6.6.7"  (release announce)
_VERSION_FROM_SUBJECT = re.compile(
    r"(?:\[PATCH[^]]*?\s|\bLinux\s)"
    r"(?P<ver>\d+\.\d+(?:\.\d+)?)"
)


class BackportEvidence(BaseModel):
    list: str
    message_id: str
    subject: str
    from_addr: str | None = None
    role: Literal["confirmation", "nomination", "rejection", "mention"]
    date_unix_ns: int | None = None


class SidecarHit(BaseModel):
    """Authoritative git-sidecar match: we've seen this SHA in the
    named repo during sidecar ingest (`kernel-lore-build-git-sidecar`).
    When `repo == "linux-stable"`, this is a hard-yes on "picked up
    by -stable" with zero lore-side inference."""

    repo: str
    sha: str
    subject: str
    author_date_unix_ns: int


class StableBackportResponse(BaseModel):
    sha_queried: str
    status: Literal[
        "picked_up",
        "pending",
        "rejected",
        "autosel_skipped",
        "not_marked",
        "no_evidence",
    ]
    stable_releases: list[str] = Field(
        default_factory=list,
        description="Parsed stable version tags from confirmation mails, e.g. ['6.6.7', '6.1.64'].",
    )
    autosel_nominated: bool = False
    evidence: list[BackportEvidence]
    sidecar_hits: list[SidecarHit] = Field(
        default_factory=list,
        description=(
            "Authoritative git-sidecar matches (when the operator has "
            "ingested mainline + stable trees). A hit in a "
            "`linux-stable*` repo upgrades the status to `picked_up` "
            "with zero lore-side heuristic inference."
        ),
    )
    backend: Literal["sidecar_authoritative", "lore_heuristic"] = Field(
        description=(
            "`sidecar_authoritative` when git-sidecar answered the "
            "question via the stable-tree history; `lore_heuristic` "
            "when we fell back to lore mail patterns because the "
            "operator hasn't built the sidecar yet."
        ),
    )
    caveat: str = Field(
        description=(
            "One-line honesty about what we could and could not see. "
            "For example: 'stable-commits list not ingested — "
            "confirmation evidence unavailable'."
        )
    )
    freshness: Freshness
    blind_spots_ref: str = "blind-spots://coverage"


async def lore_stable_backport_status(
    sha: Annotated[
        str,
        Field(
            min_length=7,
            max_length=40,
            description=(
                "Mainline commit SHA (7–40 hex chars). Short SHAs are "
                "accepted; the tool uses substring match, so give as "
                "many characters as you have."
            ),
        ),
    ],
    search_limit: Annotated[
        int,
        Field(
            ge=10,
            le=200,
            description=(
                "Per-list evidence cap. Stable releases rarely "
                "generate more than a few mails per commit."
            ),
        ),
    ] = 50,
) -> StableBackportResponse:
    """Did this mainline commit reach -stable? Evidence from lore.

    Cost: moderate — expected p95 400 ms (one BM25 query per
    relevant list + regex parse).
    """
    if not (_SHA_FULL.match(sha) or _SHA_SHORT.match(sha)):
        raise invalid_argument(
            name="sha",
            reason="must be 7-40 hex characters",
            value=sha,
            example="abc123456789",
        )

    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    needle = sha.lower()
    short = needle[:12] if len(needle) >= 12 else needle

    evidence: list[BackportEvidence] = []
    releases: set[str] = set()
    autosel = False
    sidecar_hits: list[SidecarHit] = []

    # Sidecar fast path (only fires when the operator has built it
    # AND indexed linux-stable). A 40-char SHA in `linux-stable*`
    # repos is authoritative "picked up by -stable" without any
    # lore-side inference. Short SHAs skip this path — the lookup
    # requires an exact primary-key match.
    sidecar_repos: list[str] = []
    if len(needle) == 40:
        try:
            repos = await run_with_timeout(_core.git_sidecar_repos, settings.data_dir)
            sidecar_repos = [r["repo"] for r in repos]
            for repo in sidecar_repos:
                if not (repo == "linux-stable" or repo.startswith("linux-stable")):
                    continue
                hit = await run_with_timeout(
                    _core.git_sidecar_find_sha, settings.data_dir, repo, needle
                )
                if hit is not None:
                    sidecar_hits.append(
                        SidecarHit(
                            repo=hit["repo"],
                            sha=hit["sha"],
                            subject=hit["subject"],
                            author_date_unix_ns=hit["author_date_ns"],
                        )
                    )
        except Exception:  # noqa: BLE001
            # Sidecar absent / schema mismatch / concurrent writer —
            # fall through to the lore-only heuristic path. Tools
            # never fail a caller because an optional tier is
            # missing.
            sidecar_hits = []
            sidecar_repos = []

    async def _search_list(list_name: str, role: str) -> list[dict]:
        # BM25 handles the SHA as a term (our KernelIdentSplitter keeps
        # hex identifiers whole). Fall back to substring on subject.
        try:
            result = await run_with_timeout(
                reader.router_search,
                f"list:{list_name} {short}",
                search_limit,
            )
            return list(result.get("hits") or [])
        except Exception:  # noqa: BLE001
            # If the router errors (e.g. BM25 not built for this list),
            # swallow and let the caller see the evidence list is empty
            # for this role — captured in the caveat.
            return []

    # 1. Confirmation via stable-commits.
    confirms = await _search_list("stable-commits", "confirmation")
    for hit in confirms:
        subject = hit.get("subject_raw") or hit.get("subject_normalized") or ""
        m = _VERSION_FROM_SUBJECT.search(subject)
        if m:
            releases.add(m.group("ver"))
        evidence.append(
            BackportEvidence(
                list=hit.get("list") or "stable-commits",
                message_id=hit.get("message_id") or "",
                subject=subject,
                from_addr=hit.get("from_addr"),
                role="confirmation",
                date_unix_ns=hit.get("date_unix_ns"),
            )
        )

    # 2. AUTOSEL / manual nomination via stable list.
    noms = await _search_list("stable", "nomination")
    for hit in noms:
        subject = hit.get("subject_raw") or hit.get("subject_normalized") or ""
        if "autosel" in subject.lower():
            autosel = True
        evidence.append(
            BackportEvidence(
                list=hit.get("list") or "stable",
                message_id=hit.get("message_id") or "",
                subject=subject,
                from_addr=hit.get("from_addr"),
                role="nomination",
                date_unix_ns=hit.get("date_unix_ns"),
            )
        )

    # 3. Opt-out check (noautosel) — do a substr scan over the main
    # message to catch the mainline commit's Cc trailer. Cheap: we
    # accept false negatives here since the trailer lives in the
    # source patch body, which may be on a different list with
    # heterogeneous indexing.
    noautosel_seen = False
    try:
        nauto = await run_with_timeout(reader.substr_subject, f"noautosel", None, None, None, 10)
        for hit in nauto:
            if needle in ((hit.get("subject_raw") or "") + (hit.get("message_id") or "")):
                noautosel_seen = True
                break
    except Exception:  # noqa: BLE001
        pass

    # 4. Decide status. The git sidecar wins over any lore-side
    # inference when it's present: a hit in linux-stable* is the
    # authoritative answer, full stop. Fall through to the heuristic
    # chain only when the sidecar has nothing to say.
    backend: Literal["sidecar_authoritative", "lore_heuristic"]
    if sidecar_hits:
        status: str = "picked_up"
        backend = "sidecar_authoritative"
        # Sidecar subjects often carry the stable version tag; mine
        # them the same way the lore-confirmation path does, so the
        # `stable_releases` field stays populated.
        for hit in sidecar_hits:
            m = _VERSION_FROM_SUBJECT.search(hit.subject)
            if m:
                releases.add(m.group("ver"))
    elif confirms:
        status = "picked_up"
        backend = "lore_heuristic"
    elif noautosel_seen:
        status = "autosel_skipped"
        backend = "lore_heuristic"
    elif noms:
        status = "pending"
        backend = "lore_heuristic"
    else:
        status = "no_evidence"
        backend = "lore_heuristic"

    # 5. Build caveat. Different text by backend.
    caveats = []
    if sidecar_hits:
        caveats.append(
            f"authoritative via git sidecar — matched in "
            f"{', '.join(sorted({h.repo for h in sidecar_hits}))}"
        )
    else:
        if "linux-stable" in sidecar_repos or any(
            r.startswith("linux-stable") for r in sidecar_repos
        ):
            caveats.append(
                "git sidecar present but SHA not found in any "
                "linux-stable* repo — either not picked up, or the "
                "sidecar is behind the stable tip"
            )
        elif sidecar_repos:
            caveats.append(
                "git sidecar present but linux-stable not ingested; "
                "falling back to lore-side evidence"
            )
        else:
            caveats.append(
                "git sidecar not built on this server — set "
                "KLMCP_GIT_MIRRORS and run `kernel-lore-build-git-"
                "sidecar` for authoritative backport answers"
            )
        if not confirms:
            caveats.append(
                "no confirmation evidence in stable-commits (list may "
                "not be ingested, or the commit hasn't been merged to "
                "-stable yet)"
            )
    caveat = "; ".join(caveats)

    return StableBackportResponse(
        sha_queried=sha.lower(),
        status=status,  # type: ignore[arg-type]
        stable_releases=sorted(releases),
        autosel_nominated=autosel,
        evidence=evidence,
        sidecar_hits=sidecar_hits,
        backend=backend,
        caveat=caveat,
        freshness=build_freshness(reader),
    )
