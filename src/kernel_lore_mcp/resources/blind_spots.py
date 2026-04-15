"""`blind-spots://coverage` — honest coverage statement.

Exposed as an MCP resource, not as a per-response payload, so LLM
callers fetch it once per session and cite the URI rather than
paying a per-call token tax. `SearchResponse.blind_spots_ref`
points here.

Important distinction captured below: messages initially sent to
`security@kernel.org` land in our index *once they are declassified
to public lore*. Coverage of the embargoed window itself remains
exactly zero.
"""

from __future__ import annotations

BLIND_SPOTS_URI = "blind-spots://coverage"

BLIND_SPOTS_TEXT = """\
kernel-lore-mcp indexes public lore.kernel.org mailing list archives
and select subsystem maintainer git trees. It does NOT see:

  * private security@kernel.org queue (threads only appear once
    they have been declassified to a public list — typically within
    minutes of declassification, so an LLM caller should not treat
    `lore_search` hits as proof of public disclosure history)
  * distro vendor backports (Oracle/RHEL/SUSE/Azure/Amazon Linux)
  * syzbot pre-public findings
  * ZDI / research-shop internal pipelines
  * CVE Project in-flight embargoes
  * any discussion off-list (IRC, private email, video calls)

Freshness: lore trails vger by 1-5 minutes; our ingestion adds
another 10-20 minutes p95. See /status for per-list timestamps.
"""


def blind_spots_text() -> str:
    """The exposed resource body. One place to change wording."""
    return BLIND_SPOTS_TEXT
