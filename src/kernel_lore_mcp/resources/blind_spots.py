"""`blind_spots://coverage` — honest coverage statement.

Exposed as an MCP resource, not as a per-response payload, so LLM
callers fetch it once and cite it rather than paying a per-call
token tax. `SearchResponse.blind_spots_ref` points here.

Registration happens in `server.build_server()` once we wire
`@mcp.resource("blind_spots://coverage")` — see TODO.md phase 2.
"""

from __future__ import annotations

BLIND_SPOTS_TEXT = """\
kernel-lore-mcp indexes public lore.kernel.org mailing list archives
and select subsystem maintainer git trees. It does NOT see:

  * private security@kernel.org queue (only messages later made public
    appear here)
  * distro vendor backports (Oracle/RHEL/SUSE/Azure/Amazon Linux)
  * syzbot pre-public findings
  * ZDI / research-shop internal pipelines
  * CVE Project in-flight embargoes
  * any discussion off-list (IRC, private email, video calls)

Freshness: lore trails vger by 1-5 minutes; our ingestion adds
another 10-20 minutes p95. See /status for per-list timestamps.
"""
