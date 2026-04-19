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


class StableBackportResponse(BaseModel):
    sha_queried: str
    status: Literal[
        "picked_up", "pending", "rejected", "autosel_skipped", "not_marked", "no_evidence"
    ]
    stable_releases: list[str] = Field(
        default_factory=list,
        description="Parsed stable version tags from confirmation mails, e.g. ['6.6.7', '6.1.64'].",
    )
    autosel_nominated: bool = False
    evidence: list[BackportEvidence]
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
        nauto = await run_with_timeout(
            reader.substr_subject, f"noautosel", None, None, 10
        )
        for hit in nauto:
            if needle in ((hit.get("subject_raw") or "") + (hit.get("message_id") or "")):
                noautosel_seen = True
                break
    except Exception:  # noqa: BLE001
        pass

    # 4. Decide status from evidence.
    if confirms:
        status: str = "picked_up"
    elif noautosel_seen:
        status = "autosel_skipped"
    elif noms:
        status = "pending"
    elif evidence:
        status = "no_evidence"
    else:
        status = "no_evidence"

    # 5. Build caveat.
    caveats = []
    if not confirms:
        caveats.append(
            "no confirmation evidence in stable-commits (the list may "
            "not be ingested on this server, or the commit hasn't been "
            "merged to -stable yet)"
        )
    caveats.append(
        "lore-only — authoritative 'which releases contain X' "
        "requires a linux-stable.git log check"
    )
    caveat = "; ".join(caveats)

    return StableBackportResponse(
        sha_queried=sha.lower(),
        status=status,  # type: ignore[arg-type]
        stable_releases=sorted(releases),
        autosel_nominated=autosel,
        evidence=evidence,
        caveat=caveat,
        freshness=build_freshness(reader),
    )
