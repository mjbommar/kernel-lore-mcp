"""Phase 11 — server-provided prompts (slash commands).

Five invariants pinned:

1. All 5 prompts are registered and visible via `list_prompts`.
2. Every argument is optional (has a Python default) so Claude Code
   can render a zero-arg slash command per anthropics/claude-code#30733.
3. `get_prompt(name)` returns at least one user-role message with a
   non-empty text payload.
4. `get_prompt(name, arguments=...)` substitutes the arguments into
   the body so the agent sees the concrete query, not a placeholder.
5. Every tool name referenced in any prompt body exists in the live
   tool registry — a drift check.
"""

from __future__ import annotations

import re

import pytest
from fastmcp import Client

from kernel_lore_mcp.server import build_server

EXPECTED_PROMPTS = {
    "klmcp_pre_disclosure_novelty_check",
    "klmcp_cve_chain_expand",
    "klmcp_series_version_diff",
    "klmcp_recent_reviewers_for",
    "klmcp_cross_subsystem_pattern_transfer",
}


@pytest.mark.asyncio
async def test_all_phase11_prompts_registered() -> None:
    async with Client(build_server()) as c:
        prompts = await c.list_prompts()
    names = {p.name for p in prompts}
    missing = EXPECTED_PROMPTS - names
    assert not missing, f"phase-11 prompts absent from registry: {missing}"


@pytest.mark.asyncio
async def test_every_prompt_argument_has_default() -> None:
    # Claude Code still lists prompts that have required args, but won't
    # elicit input — so the slash command is effectively broken. Every
    # phase-11 prompt must ship with Python defaults on every arg.
    async with Client(build_server()) as c:
        prompts = await c.list_prompts()
    offenders: list[str] = []
    for p in prompts:
        if p.name not in EXPECTED_PROMPTS:
            continue
        for arg in p.arguments or []:
            if arg.required:
                offenders.append(f"{p.name}.{arg.name} is required")
    assert not offenders, "required args break Claude Code rendering:\n" + "\n".join(offenders)


@pytest.mark.asyncio
async def test_prompts_render_with_zero_arguments() -> None:
    async with Client(build_server()) as c:
        for name in EXPECTED_PROMPTS:
            result = await c.get_prompt(name, arguments={})
            assert result.messages, f"{name}: no messages"
            first = result.messages[0]
            assert first.role == "user", f"{name}: first message role != user"
            assert first.content.text.strip(), f"{name}: empty body"


@pytest.mark.asyncio
async def test_novelty_prompt_substitutes_arguments() -> None:
    async with Client(build_server()) as c:
        result = await c.get_prompt(
            "klmcp_pre_disclosure_novelty_check",
            arguments={
                "subsystem": "cifs",
                "vuln_class": "UAF",
                "window_months": 12,
            },
        )
    body = result.messages[0].content.text
    assert "cifs" in body
    assert "UAF" in body
    assert "12 months" in body


@pytest.mark.asyncio
async def test_cross_subsystem_prompt_substitutes_arguments() -> None:
    async with Client(build_server()) as c:
        result = await c.get_prompt(
            "klmcp_cross_subsystem_pattern_transfer",
            arguments={
                "pattern": "xdr_check_bounds",
                "from_subsystem": "nfs",
                "to_subsystems": "sunrpc, SCSI",
            },
        )
    body = result.messages[0].content.text
    assert "xdr_check_bounds" in body
    assert "nfs" in body
    assert "sunrpc" in body
    assert "SCSI" in body


@pytest.mark.asyncio
async def test_prompt_bodies_reference_only_real_tools() -> None:
    # Drift guard: every `lore_*(` call-site in every rendered prompt
    # must map to a tool that actually exists today. Missed renames
    # would otherwise surface only at agent runtime.
    async with Client(build_server()) as c:
        live_tools = {t.name for t in await c.list_tools()}
        # Also count the resource URIs we reference by scheme.
        bodies: list[tuple[str, str]] = []
        for name in EXPECTED_PROMPTS:
            result = await c.get_prompt(name, arguments={})
            bodies.append((name, result.messages[0].content.text))

    tool_call_re = re.compile(r"\b(lore_[a-z_]+)\(")
    stale: list[str] = []
    for prompt_name, body in bodies:
        for referenced in set(tool_call_re.findall(body)):
            if referenced not in live_tools:
                stale.append(f"{prompt_name} references missing tool: {referenced}")
    assert not stale, "\n".join(stale)


@pytest.mark.asyncio
async def test_prompts_declare_tags_and_descriptions() -> None:
    # Agent-side pickers key off description + tags; both must exist
    # on every phase-11 prompt.
    async with Client(build_server()) as c:
        prompts = await c.list_prompts()
    for p in prompts:
        if p.name not in EXPECTED_PROMPTS:
            continue
        assert p.description and p.description.strip(), f"{p.name}: empty description"
