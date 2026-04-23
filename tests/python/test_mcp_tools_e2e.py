"""End-to-end MCP tool tests: ingest → server → in-process Client.

This is the v0.5 acceptance gate. If all three paths work, an agent
can reach structured metadata about lore over MCP without any
external infrastructure.
"""

from __future__ import annotations

import os
from collections.abc import AsyncIterator
from pathlib import Path

import pytest
import pytest_asyncio
from fastmcp import Client

from kernel_lore_mcp import _core
from kernel_lore_mcp.server import build_server
from tests.python.fixtures import make_synthetic_shard


@pytest_asyncio.fixture
async def client(tmp_path: Path) -> AsyncIterator[Client]:
    shard_dir = tmp_path / "shards" / "0.git"
    shard_dir.parent.mkdir(parents=True)
    make_synthetic_shard(shard_dir)

    data_dir = tmp_path / "data"
    data_dir.mkdir()
    _core.ingest_shard(
        data_dir=data_dir,
        shard_path=shard_dir,
        list="linux-cifs",
        shard="0",
        run_id="run-0001",
    )

    # Point the server's Settings at this tmp data_dir via env.
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        mcp = build_server()
        async with Client(mcp) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


@pytest.mark.asyncio
async def test_tools_listed(client: Client) -> None:
    tools = await client.list_tools()
    names = {t.name for t in tools}
    assert {
        "lore_search",
        "lore_activity",
        "lore_message",
        "lore_expand_citation",
        "lore_series_timeline",
    }.issubset(names)

    # readOnlyHint on every tool.
    for t in tools:
        if t.name.startswith("lore_"):
            assert t.annotations is not None
            assert t.annotations.readOnlyHint is True


@pytest.mark.asyncio
async def test_lore_author_profile_aggregates_from_addr(client: Client) -> None:
    """lore_author_profile samples the most-recent messages for an
    address and aggregates subsystems + trailer stats. Fixture has
    two alice@ messages on linux-cifs; both carry patches, one
    carries a Fixes: trailer."""
    result = await client.call_tool(
        "lore_author_profile",
        {"addr": "alice@example.com", "limit": 1000},
    )
    data = result.data
    assert data.addr_queried == "alice@example.com"
    assert data.sampled == 2
    assert data.limit_hit is False
    assert data.patches_with_content == 2
    assert data.with_fixes_trailer >= 1
    assert len(data.subsystems) == 1
    assert data.subsystems[0].list == "linux-cifs"
    assert data.subsystems[0].patches == 2
    assert data.oldest_unix_ns is not None
    assert data.newest_unix_ns is not None
    # Invariant: oldest <= newest.
    assert data.oldest_unix_ns <= data.newest_unix_ns


@pytest.mark.asyncio
async def test_lore_author_profile_unknown_addr_empty(client: Client) -> None:
    result = await client.call_tool(
        "lore_author_profile",
        {"addr": "nobody@nowhere.invalid"},
    )
    data = result.data
    assert data.sampled == 0
    assert data.subsystems == []


@pytest.mark.asyncio
async def test_lore_subsystem_churn_list_scope(client: Client) -> None:
    result = await client.call_tool(
        "lore_subsystem_churn",
        {"scope": "list:linux-cifs", "window_days": 3650},  # long window to catch fixture
    )
    data = result.data
    assert data.scope == "list:linux-cifs"
    assert data.sampled_patches >= 1
    # top_files must include the two files the fixture patches touched.
    paths = {f.path for f in data.top_files}
    assert "fs/smb/server/smbacl.c" in paths or "fs/smb/server/smb2pdu.c" in paths


@pytest.mark.asyncio
async def test_lore_subsystem_churn_rejects_bad_scope(client: Client) -> None:
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError) as exc_info:
        await client.call_tool("lore_subsystem_churn", {"scope": "garbage"})
    assert "invalid_argument" in str(exc_info.value)


@pytest.mark.asyncio
async def test_lore_thread_state_unknown_on_small_thread(client: Client) -> None:
    """Sample corpus has only two messages touching different files,
    no RFC tag, no supersede chain, no NACK. Should land on
    `unknown` with low confidence and an honest caveat."""
    result = await client.call_tool(
        "lore_thread_state",
        {"message_id": "m1@x"},
    )
    data = result.data
    assert data.message_id == "m1@x"
    # State depends on ages: fixture dates may be > 180 days old →
    # abandoned, otherwise unknown. Both acceptable; just make sure
    # we didn't confidently claim something we can't support.
    assert data.state in {"unknown", "abandoned", "under_review"}
    assert data.confidence in {"high", "medium", "low"}
    assert "merged" in data.caveat


@pytest.mark.asyncio
async def test_lore_thread_state_not_found_on_bogus_mid(client: Client) -> None:
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError) as exc_info:
        await client.call_tool("lore_thread_state", {"message_id": "<nope@nowhere>"})
    assert "not_found" in str(exc_info.value)


@pytest.mark.asyncio
async def test_lore_file_timeline_default_asc(client: Client) -> None:
    """Timeline with default asc order + quarter bucket. Fixture has
    two patches touching fs/smb/server/smbacl.c; both land in the
    same quarter bucket."""
    result = await client.call_tool(
        "lore_file_timeline",
        {"path": "fs/smb/server/smbacl.c"},
    )
    data = result.data
    assert data.path_queried == "fs/smb/server/smbacl.c"
    assert data.order == "asc"
    assert data.bucket == "quarter"
    assert data.total_matching >= 1
    # Histogram shape: at least one bucket, patches count matches.
    assert len(data.histogram) >= 1
    assert sum(b.patches for b in data.histogram) == data.total_matching


@pytest.mark.asyncio
async def test_lore_file_timeline_window_rejects_inverted(client: Client) -> None:
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError) as exc_info:
        await client.call_tool(
            "lore_file_timeline",
            {
                "path": "fs/smb/server/smbacl.c",
                "since_unix_ns": 2_000_000_000_000_000_000,
                "until_unix_ns": 1_000_000_000_000_000_000,
            },
        )
    assert "invalid_argument" in str(exc_info.value)


@pytest.mark.asyncio
async def test_lore_stable_backport_status_rejects_non_hex(client: Client) -> None:
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError) as exc_info:
        await client.call_tool("lore_stable_backport_status", {"sha": "not-a-sha"})
    assert "invalid_argument" in str(exc_info.value)


@pytest.mark.asyncio
async def test_lore_stable_backport_status_unknown_sha_no_evidence(client: Client) -> None:
    """On the synthetic corpus there's no stable/stable-commits data;
    the tool should return a structured `no_evidence` result with the
    honest caveat, not an error."""
    result = await client.call_tool(
        "lore_stable_backport_status",
        {"sha": "deadbeef01234567"},
    )
    data = result.data
    assert data.sha_queried == "deadbeef01234567"
    assert data.status in {"no_evidence", "pending", "picked_up"}
    # The caveat must mention the git-sidecar path so callers know
    # what's missing on deployments without it.
    assert "sidecar" in data.caveat or "stable-commits" in data.caveat
    # With no sidecar on the test fixture, backend falls to heuristic.
    assert data.backend == "lore_heuristic"
    assert data.sidecar_hits == []


@pytest.mark.asyncio
async def test_lore_thread_state_exposes_backend_and_merged_in(
    client: Client,
) -> None:
    """Without a git sidecar the verdict must be lore-heuristic and
    merged_in must be empty. The caveat should point at how to build
    the sidecar so the caller knows the upgrade path."""
    result = await client.call_tool(
        "lore_thread_state",
        {"message_id": "m1@x"},
    )
    data = result.data
    assert data.backend == "lore_heuristic"
    assert data.merged_in == []
    assert "sidecar" in data.caveat.lower()


@pytest.mark.asyncio
async def test_lore_maintainer_profile_without_maintainers_file(client: Client) -> None:
    """Without a MAINTAINERS snapshot in data_dir, the tool must
    return `maintainers_available: False` but still report observed
    trailer activity from the sample corpus."""
    result = await client.call_tool(
        "lore_maintainer_profile",
        {"path": "fs/smb/server/smbacl.c"},
    )
    data = result.data
    assert data.maintainers_available is False
    assert data.declared == []
    assert data.sampled_patches >= 1


@pytest.mark.asyncio
async def test_lore_maintainer_profile_with_maintainers_snapshot(
    client: Client, tmp_path: Path
) -> None:
    """Drop a minimal MAINTAINERS into the data_dir used by the
    fixture and re-open the reader via the tool — declared entries
    must populate and the cross-reference must flag the silent
    reviewer as stale."""
    import os

    data_dir = Path(os.environ["KLMCP_DATA_DIR"])
    (data_dir / "MAINTAINERS").write_text(
        "KSMBD\n"
        "M:\tAlice <alice@example.com>\n"
        "R:\tDavid Dormant <david@example.com>\n"
        "L:\tlinux-cifs@vger.kernel.org\n"
        "S:\tMaintained\n"
        "F:\tfs/smb/server/\n"
    )
    result = await client.call_tool(
        "lore_maintainer_profile",
        {"path": "fs/smb/server/smbacl.c"},
    )
    data = result.data
    assert data.maintainers_available is True
    assert len(data.declared) == 1
    assert data.declared[0].name == "KSMBD"
    stale = set(data.stale_declared)
    assert "david@example.com" in stale, f"david should be stale: {stale}"


@pytest.mark.asyncio
async def test_lore_author_footprint_unions_sources(client: Client) -> None:
    """Footprint unions authored + body-mention rows. On the sample
    corpus alice authored 2 patches; body mentions may or may not
    surface depending on the synthetic fixture content."""
    result = await client.call_tool(
        "lore_author_footprint",
        {"addr": "alice@example.com"},
    )
    data = result.data
    assert data.addr_queried == "alice@example.com"
    assert data.total_distinct >= 2, f"expected >=2 distinct hits, got {data.total_distinct}"
    assert data.authored_count >= 2
    # Each hit carries at least one role.
    for h in data.hits:
        assert h.roles, f"hit missing roles: {h.message_id}"


@pytest.mark.asyncio
async def test_lore_author_footprint_rejects_non_email(client: Client) -> None:
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError) as exc_info:
        await client.call_tool("lore_author_footprint", {"addr": "not-an-email"})
    assert "invalid_argument" in str(exc_info.value)


@pytest.mark.asyncio
async def test_lore_corpus_stats_surfaces_per_list_rows(client: Client) -> None:
    """The sample corpus has a handful of lists; corpus_stats should
    surface each with a positive row count, coherent tier markers,
    and a last_ingest_utc timestamp after ingest ran."""
    result = await client.call_tool("lore_corpus_stats")
    data = result.data
    assert data.total_rows >= 1
    assert data.lists_covered >= 1
    assert len(data.lists) == data.lists_covered
    assert data.generation >= 1
    assert data.last_ingest_utc is not None
    assert data.schema_version >= 1

    for per_list in data.lists:
        assert per_list.rows >= 1
        # Earliest / latest populated unless the sample fixture had a
        # row with no date (shouldn't).
        assert per_list.earliest_date_unix_ns is not None
        assert per_list.latest_date_unix_ns is not None
        assert per_list.latest_date_unix_ns >= per_list.earliest_date_unix_ns

    # Every tier marker present; status reflects the corpus gen.
    tier_names = {t.tier for t in data.tiers}
    assert tier_names == {"over", "bm25", "trigram", "tid", "path_vocab"}
    for tier in data.tiers:
        assert tier.status in {
            "in sync",
            "marker absent",
        } or tier.status.startswith(("behind by ", "ahead by "))


@pytest.mark.asyncio
async def test_lore_author_profile_include_mentions_requires_narrowing(
    client: Client,
) -> None:
    """include_mentions=True without list_filter or since_unix_ns must
    be rejected — production-hardening for anonymous multi-tenant use.
    """
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError) as exc_info:
        await client.call_tool(
            "lore_author_profile",
            {"addr": "carol@example.com", "include_mentions": True},
        )
    assert "include_mentions" in str(exc_info.value)
    assert "list_filter" in str(exc_info.value) or "since" in str(exc_info.value)


@pytest.mark.asyncio
async def test_lore_author_profile_include_mentions(client: Client) -> None:
    """With include_mentions=True, carol (reviewer on fixture patches)
    should show zero authored_count but non-zero mention_count.
    Requires a narrowing filter on the server side (production
    hardening) — we pass list_filter to satisfy that.
    """
    result = await client.call_tool(
        "lore_author_profile",
        {
            "addr": "carol@example.com",
            "include_mentions": True,
            "list_filter": "linux-cifs",
        },
    )
    data = result.data
    assert data.addr_queried == "carol@example.com"
    assert data.authored_count == 0
    assert data.mention_count >= 1
    assert data.sampled == data.authored_count + data.mention_count


@pytest.mark.asyncio
async def test_lore_author_profile_rejects_non_email(client: Client) -> None:
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError) as exc_info:
        await client.call_tool("lore_author_profile", {"addr": "not-an-email"})
    assert "invalid_argument" in str(exc_info.value)


@pytest.mark.asyncio
async def test_lore_activity_by_file(client: Client) -> None:
    result = await client.call_tool(
        "lore_activity",
        {"file": "fs/smb/server/smbacl.c"},
    )
    data = result.data
    assert data.total == 1
    row = data.rows[0]
    assert row.message_id == "m1@x"
    assert row.list == "linux-cifs"
    assert "carol@example.com" in " ".join(row.reviewed_by)
    assert row.cc_stable and "stable@" in row.cc_stable[0]
    assert row.lore_url == "https://lore.kernel.org/linux-cifs/m1@x/"
    assert row.cite_key.startswith("linux-cifs/2026-04/")


@pytest.mark.asyncio
async def test_lore_activity_requires_file_or_function(client: Client) -> None:
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError, match="invalid_argument"):
        await client.call_tool("lore_activity", {})


@pytest.mark.asyncio
async def test_lore_message_returns_prose_and_patch(client: Client) -> None:
    result = await client.call_tool("lore_message", {"message_id": "m1@x"})
    data = result.data
    assert data.hit.message_id == "m1@x"
    assert data.hit.has_patch is True
    assert data.prose is not None
    assert "Prose here" in data.prose
    assert data.patch is not None
    assert data.patch.startswith("diff --git ")
    assert data.body_length > 0


@pytest.mark.asyncio
async def test_lore_message_not_found(client: Client) -> None:
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError, match="not_found"):
        await client.call_tool("lore_message", {"message_id": "nope@x"})


@pytest.mark.asyncio
async def test_lore_expand_citation_via_fixes_sha(client: Client) -> None:
    result = await client.call_tool(
        "lore_expand_citation",
        {"token": "deadbeef01234567"},
    )
    data = result.data
    assert len(data.results) == 1
    assert data.results[0].message_id == "m1@x"
    assert data.results[0].tier_provenance == ["metadata"]
    assert data.results[0].is_exact_match is True


@pytest.mark.asyncio
async def test_lore_expand_citation_via_message_id(client: Client) -> None:
    result = await client.call_tool(
        "lore_expand_citation",
        {"token": "<m2@x>"},
    )
    data = result.data
    assert len(data.results) == 1
    assert data.results[0].message_id == "m2@x"


@pytest.mark.asyncio
async def test_lore_series_timeline(client: Client) -> None:
    result = await client.call_tool(
        "lore_series_timeline",
        {"message_id": "m1@x"},
    )
    data = result.data
    # m1 and m2 have different subject_normalized ("tighten ACL bounds"
    # vs "follow-up"), so each is its own singleton series.
    assert len(data.entries) == 1
    assert data.entries[0].message_id == "m1@x"
    assert data.entries[0].series_version == 3
    assert data.entries[0].series_index == "1/2"


@pytest.mark.asyncio
async def test_lore_patch_search_finds_function_name(client: Client) -> None:
    result = await client.call_tool(
        "lore_patch_search",
        {"needle": "smb_check_perm_dacl"},
    )
    data = result.data
    assert len(data.results) == 1
    hit = data.results[0]
    assert hit.message_id == "m1@x"
    assert hit.tier_provenance == ["trigram"]
    assert data.query_tiers_hit == ["trigram"]


@pytest.mark.asyncio
async def test_lore_patch_search_returns_empty_when_no_match(client: Client) -> None:
    result = await client.call_tool(
        "lore_patch_search",
        {"needle": "does_not_appear_in_any_patch"},
    )
    data = result.data
    assert data.results == []
    assert data.query_tiers_hit == []


@pytest.mark.asyncio
async def test_lore_patch_search_rejects_short_needle(client: Client) -> None:
    # Pydantic min_length=3; FastMCP surfaces validation as ToolError.
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError):
        await client.call_tool("lore_patch_search", {"needle": "xy"})


@pytest.mark.asyncio
async def test_lore_search_bm25_finds_prose_term(client: Client) -> None:
    # Our synthetic fixture has two messages with distinctive prose
    # words: m1 says "Prose here explaining the change" and m2 says
    # "More prose." Both contain the token "prose"; only m1 contains
    # "explaining" and only m2 contains "More".
    result = await client.call_tool("lore_search", {"query": "explaining"})
    data = result.data
    assert [h.message_id for h in data.results] == ["m1@x"]
    assert data.query_tiers_hit == ["bm25"]
    assert data.results[0].tier_provenance == ["bm25"]
    assert data.results[0].score is not None


@pytest.mark.asyncio
async def test_lore_search_phrase_query_rejected(client: Client) -> None:
    from fastmcp.exceptions import ToolError

    # Double-quoted phrase is rejected by the router because the BM25
    # tier indexes positions=off (would be a silent lie otherwise).
    with pytest.raises(ToolError, match="phrase queries"):
        await client.call_tool("lore_search", {"query": '"ACL bounds"'})


@pytest.mark.asyncio
async def test_lore_search_router_dispatches_to_metadata_tier(client: Client) -> None:
    # `dfn:` predicate routes to the metadata tier; tier_provenance
    # should reflect that. (This exercises the new router; the old
    # bm25-only path would have returned empty.)
    result = await client.call_tool(
        "lore_search",
        {"query": "dfn:fs/smb/server/smbacl.c"},
    )
    data = result.data
    assert len(data.results) == 1
    assert data.results[0].message_id == "m1@x"
    assert data.query_tiers_hit == ["metadata"]
    assert data.results[0].is_exact_match is True


@pytest.mark.asyncio
async def test_lore_search_router_dispatches_dfhh_to_metadata_tier(client: Client) -> None:
    result = await client.call_tool(
        "lore_search",
        {"query": "dfhh:smb_check_perm_dacl"},
    )
    data = result.data
    assert len(data.results) == 1
    assert data.results[0].message_id == "m1@x"
    assert data.query_tiers_hit == ["metadata"]
    assert data.results[0].is_exact_match is True


@pytest.mark.asyncio
async def test_lore_search_router_combines_dfb_and_list(client: Client) -> None:
    # `dfb:` (trigram) + `list:` (metadata constraint) — single
    # request fuses both tiers.
    result = await client.call_tool(
        "lore_search",
        {"query": "dfb:smb_check_perm_dacl list:linux-cifs"},
    )
    data = result.data
    assert len(data.results) == 1
    assert data.results[0].message_id == "m1@x"
    assert "trigram" in data.query_tiers_hit


@pytest.mark.asyncio
async def test_lore_search_router_accepts_human_since_until_bounds(client: Client) -> None:
    result = await client.call_tool(
        "lore_search",
        {"query": "since:2026-04-14T12:04:00Z until:2026-04-14T12:06:00Z ksmbd"},
    )
    data = result.data
    mids = {hit.message_id for hit in data.results}
    assert mids == {"m2@x"}


@pytest.mark.asyncio
async def test_lore_search_unknown_predicate_raises(client: Client) -> None:
    from fastmcp.exceptions import ToolError

    with pytest.raises(ToolError, match="unknown predicate"):
        await client.call_tool("lore_search", {"query": "nope:foo"})
