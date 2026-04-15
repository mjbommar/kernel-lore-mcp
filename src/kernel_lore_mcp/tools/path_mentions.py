"""lore_path_mentions — find messages that mention a kernel file path.

Unlike `lore_activity(file=...)` which only searches `diff --git`
headers (`touched_files[]`), this tool catches reviewer discussions,
bug reports, shortlogs, and free prose mentions of filenames.

Backed by an Aho-Corasick automaton built from the corpus's own
`touched_files[]` vocabulary — zero false positives by
construction.

Three match modes:
  exact    — full path must match exactly.
  basename — matches any full path whose basename equals the query
             (e.g. "smbacl.c" → "fs/smb/server/smbacl.c").
  prefix   — matches any path starting with the query prefix
             (e.g. "fs/smb/server/" → every file under that dir).
"""

from __future__ import annotations

from typing import Annotated, Literal

from pydantic import Field

from kernel_lore_mcp.config import get_settings
from kernel_lore_mcp.freshness import build_freshness
from kernel_lore_mcp.mapping import row_to_search_hit
from kernel_lore_mcp.models import RowsResponse
from kernel_lore_mcp.timeout import run_with_timeout


async def lore_path_mentions(
    path: Annotated[
        str,
        Field(
            min_length=1,
            max_length=512,
            description=(
                "Kernel source-tree path or basename to search for. "
                "Examples: 'fs/smb/server/smbacl.c' (exact), 'smbacl.c' "
                "(basename), 'fs/smb/server/' (prefix)."
            ),
        ),
    ],
    match: Annotated[
        Literal["exact", "basename", "prefix"],
        Field(
            description=(
                "'exact' — full path match. "
                "'basename' — any path whose filename component equals the query. "
                "'prefix' — any path starting with the query prefix."
            ),
        ),
    ] = "exact",
    list: Annotated[str | None, Field(description="Restrict to one mailing list.")] = None,
    since_unix_ns: Annotated[
        int | None, Field(description="Date lower-bound (ns since epoch).")
    ] = None,
    limit: Annotated[int, Field(ge=1, le=500)] = 100,
) -> RowsResponse:
    """Find messages that mention a kernel source-tree path anywhere
    in their body — prose, quoted diffs, shortlogs, or patches.

    Unlike lore_activity(file=...) which only searches diff headers,
    this tool catches reviewer discussions, bug reports, and free
    mentions of filenames. Backed by an Aho-Corasick automaton over
    the corpus's own file vocabulary.

    Cost: moderate — expected p95 500 ms (AC body scan over candidate
    rows; will drop to ~50 ms once posting lists ship in v0.2.x).
    """
    from kernel_lore_mcp import _core

    settings = get_settings()
    reader = _core.Reader(settings.data_dir)
    rows = await run_with_timeout(reader.path_mentions, path, match, list, since_unix_ns, limit)
    hits = [row_to_search_hit(r, tier_provenance=["path"]) for r in rows]
    return RowsResponse(
        results=hits,
        total=len(hits),
        freshness=build_freshness(reader),
    )


__all__ = ["lore_path_mentions"]
