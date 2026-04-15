"""Phase 12 — `ctx.sample()` with graceful extractive fallback.

The pattern every Phase-12 tool follows:

    supports = client_supports_sampling(ctx)
    if supports:
        try:
            text = await sample_text(ctx, prompt, max_tokens=...)
            return {"text": text, "backend": "sampled"}
        except (ValueError, RuntimeError) as exc:
            await ctx.warning(f"sampling failed, falling back: {exc}")
    text = extractive_fallback(...)
    return {"text": text, "backend": "extractive"}

The runtime capability check reads `ctx.session.check_client_capability`
per `fastmcp/server/sampling/run.py:156-158`. The fallback path is
**not optional** — it must always produce a usable answer because:

 * Cursor's sampling support is still rolling out.
 * stdio-mode Codex sessions currently do not advertise sampling.
 * Mock tests (in-process fastmcp.Client without a sampling_handler
   set) don't advertise sampling either.

The `backend` field in every response tells the agent which path
fired so it can reason about downstream confidence.
"""

from __future__ import annotations

from fastmcp import Context
from mcp.types import ClientCapabilities, SamplingCapability


def client_supports_sampling(ctx: Context) -> bool:
    """Return True iff the connected client advertises `sampling`.

    Wraps `ctx.session.check_client_capability(...)`. Returns False
    rather than raising if the session is not available (e.g. the
    tool was invoked outside a request lifecycle). This keeps the
    fallback branch flowing without special-casing in every tool.
    """
    try:
        session = ctx.session
    except Exception:
        return False
    try:
        return bool(
            session.check_client_capability(ClientCapabilities(sampling=SamplingCapability()))
        )
    except Exception:
        return False


async def sample_text(
    ctx: Context,
    prompt: str,
    *,
    system_prompt: str | None = None,
    max_tokens: int = 512,
    temperature: float | None = 0.0,
) -> str:
    """Thin wrapper over `ctx.sample` that returns a plain string.

    Callers should treat a `ValueError` / `RuntimeError` as "sampling
    unavailable at this moment" and branch into an extractive path
    rather than bubbling the error up to the MCP client.
    """
    result = await ctx.sample(
        prompt,
        system_prompt=system_prompt,
        max_tokens=max_tokens,
        temperature=temperature,
    )
    return (result.text or "").strip()


__all__ = ["client_supports_sampling", "sample_text"]
