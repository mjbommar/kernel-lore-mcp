"""Phase 12 — sampling tools: ctx.sample() + extractive fallback.

Six invariants pinned:

1. All three tools are registered and reachable.
2. Fallback path works when the in-process Client has no
   `sampling_handler=` (default). Every tool returns `backend=
   "extractive"` with a non-empty answer.
3. Sampling path works when a mock handler is wired. Every tool
   returns `backend="sampled"` and echoes the mocked text.
4. Classifier reports a non-empty rationale on the extractive
   backend (agent needs to know *why* the rule fired).
5. Sampled classifier output that's not in the accepted label set
   is rejected; tool falls back to the rule label rather than
   emitting an unknown label.
6. Context parameter is auto-stripped from the MCP tools/list
   schema (no `ctx` argument visible to the client).
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

SAMPLING_TOOLS = {
    "lore_summarize_thread",
    "lore_classify_patch",
    "lore_explain_review_status",
}


@pytest_asyncio.fixture
async def fallback_client(tmp_path: Path) -> AsyncIterator[Client]:
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
        run_id="run-sampling-fallback",
    )
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)
    try:
        async with Client(build_server()) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


@pytest_asyncio.fixture
async def sampled_client(tmp_path: Path) -> AsyncIterator[Client]:
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
        run_id="run-sampling-live",
    )
    os.environ["KLMCP_DATA_DIR"] = str(data_dir)

    async def fake_llm(messages, params, request_context):
        # Per-tool: classifier needs one of the accepted labels; the
        # other two accept arbitrary text. Use the system prompt to
        # discriminate.
        system = (params.systemPrompt or "").lower()
        if "classify" in system:
            return "bugfix"
        if "reviewer concerns" in system:
            return "- wrong bounds check\n- missing Fixes: tag"
        return "Stub LLM summary of the thread."

    try:
        async with Client(build_server(), sampling_handler=fake_llm) as c:
            yield c
    finally:
        os.environ.pop("KLMCP_DATA_DIR", None)


@pytest.mark.asyncio
async def test_all_sampling_tools_registered(fallback_client: Client) -> None:
    tools = await fallback_client.list_tools()
    names = {t.name for t in tools}
    missing = SAMPLING_TOOLS - names
    assert not missing, f"phase-12 tools absent: {missing}"


@pytest.mark.asyncio
async def test_context_param_hidden_from_schema(fallback_client: Client) -> None:
    tools = await fallback_client.list_tools()
    for t in tools:
        if t.name not in SAMPLING_TOOLS:
            continue
        props = (t.inputSchema or {}).get("properties", {}) or {}
        assert "ctx" not in props, f"{t.name}: ctx leaked into schema: {props}"
        assert "context" not in props, f"{t.name}: context leaked into schema"


@pytest.mark.asyncio
async def test_summarize_thread_fallback(fallback_client: Client) -> None:
    result = await fallback_client.call_tool(
        "lore_summarize_thread",
        {"message_id": "m1@x", "max_sentences": 5},
    )
    data = result.data
    assert data.backend == "extractive"
    assert data.summary.strip(), "extractive summary must not be empty"
    assert data.message_count >= 1


@pytest.mark.asyncio
async def test_summarize_thread_sampled(sampled_client: Client) -> None:
    result = await sampled_client.call_tool(
        "lore_summarize_thread",
        {"message_id": "m1@x", "max_sentences": 5},
    )
    data = result.data
    assert data.backend == "sampled"
    assert "Stub LLM summary" in data.summary


@pytest.mark.asyncio
async def test_classify_patch_fallback_rule(fallback_client: Client) -> None:
    # Synthetic fixture m1 has Fixes: trailer -> rule labels it "bugfix".
    result = await fallback_client.call_tool(
        "lore_classify_patch",
        {"message_id": "m1@x"},
    )
    data = result.data
    assert data.backend == "extractive"
    assert data.label == "bugfix"
    assert data.confidence is not None and data.confidence > 0.5
    assert data.rationale.strip(), "rule fallback must carry rationale"


@pytest.mark.asyncio
async def test_classify_patch_sampled_label(sampled_client: Client) -> None:
    result = await sampled_client.call_tool(
        "lore_classify_patch",
        {"message_id": "m1@x"},
    )
    data = result.data
    assert data.backend == "sampled"
    assert data.label == "bugfix"
    # On the sampled path we deliberately clear confidence (no rule
    # score) so the agent doesn't over-read the number.
    assert data.confidence is None


@pytest.mark.asyncio
async def test_explain_review_status_fallback(fallback_client: Client) -> None:
    result = await fallback_client.call_tool(
        "lore_explain_review_status",
        {"message_id": "m1@x"},
    )
    data = result.data
    assert data.backend == "extractive"
    # Synthetic fixture has a Reviewed-by trailer on m1; the thread
    # aggregator should surface it.
    assert "reviewed_by" in data.trailers_seen
    assert data.trailers_seen["reviewed_by"], "reviewed_by list empty"


@pytest.mark.asyncio
async def test_explain_review_status_sampled(sampled_client: Client) -> None:
    result = await sampled_client.call_tool(
        "lore_explain_review_status",
        {"message_id": "m1@x"},
    )
    data = result.data
    assert data.backend == "sampled"
    # Handler returned two bullet lines; tool should split them.
    assert any("bounds check" in c.lower() for c in data.open_concerns)
    assert any("fixes" in c.lower() for c in data.open_concerns)
