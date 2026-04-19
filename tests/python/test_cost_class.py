"""Unit tests for kernel_lore_mcp.cost_class — per-class concurrency
cap + structured rate_limited rejection.
"""

from __future__ import annotations

import asyncio

import pytest

from kernel_lore_mcp import cost_class
from kernel_lore_mcp.errors import LoreError


def test_cost_class_of_parses_docstring_line() -> None:
    async def _cheap():
        """Does nothing.

        Cost: cheap — expected p95 10 ms.
        """

    async def _moderate():
        """Does nothing.

        Cost: moderate — expected p95 100 ms.
        """

    async def _expensive():
        """Does nothing.

        Cost: expensive — expected p95 1000 ms.
        """

    async def _noline():
        """Does nothing but has no Cost: hint."""

    assert cost_class.cost_class_of(_cheap) == "cheap"
    assert cost_class.cost_class_of(_moderate) == "moderate"
    assert cost_class.cost_class_of(_expensive) == "expensive"
    # Default is the SAFE bucket, not the cheap one.
    assert cost_class.cost_class_of(_noline) == "moderate"


@pytest.mark.asyncio
async def test_cost_limited_admits_under_capacity(monkeypatch) -> None:
    """Within the cap, the wrapped fn runs normally."""

    # Shrink the cap to 2 for this test so we can reason about the boundary.
    monkeypatch.setitem(cost_class._LIMITS, "moderate", 2)
    monkeypatch.setitem(
        cost_class._SEMAPHORES, "moderate", asyncio.Semaphore(2)
    )

    call_count = 0

    async def _work():
        """Placeholder.

        Cost: moderate — expected p95 100 ms.
        """
        nonlocal call_count
        call_count += 1
        return "ok"

    wrapped = cost_class.cost_limited(_work)
    results = await asyncio.gather(wrapped(), wrapped())
    assert results == ["ok", "ok"]
    assert call_count == 2


@pytest.mark.asyncio
async def test_cost_limited_rejects_over_capacity(monkeypatch) -> None:
    """Third concurrent call exceeds a cap of 2 and gets a structured
    rate_limited LoreError instead of queueing.
    """
    monkeypatch.setitem(cost_class._LIMITS, "moderate", 2)
    monkeypatch.setitem(
        cost_class._SEMAPHORES, "moderate", asyncio.Semaphore(2)
    )

    # A slow fn so the first two hold the semaphore while the third tries.
    gate = asyncio.Event()

    async def _slow():
        """Placeholder.

        Cost: moderate — expected p95 100 ms.
        """
        await gate.wait()
        return "ok"

    wrapped = cost_class.cost_limited(_slow)

    # Kick off two slow calls. They acquire 2/2 and wait on the gate.
    first = asyncio.create_task(wrapped())
    second = asyncio.create_task(wrapped())
    # Let them actually enter the wrapper and grab the semaphore.
    await asyncio.sleep(0.01)

    # Third call should reject fast.
    with pytest.raises(LoreError) as exc_info:
        await wrapped()
    msg = str(exc_info.value)
    assert "rate_limited" in msg
    assert "moderate" in msg

    # Let the slow calls complete so the test tears down cleanly.
    gate.set()
    await asyncio.gather(first, second)


@pytest.mark.asyncio
async def test_cost_limited_releases_on_exception(monkeypatch) -> None:
    """A failed call must release its semaphore slot so subsequent
    callers don't see phantom saturation.
    """
    monkeypatch.setitem(cost_class._LIMITS, "expensive", 1)
    monkeypatch.setitem(
        cost_class._SEMAPHORES, "expensive", asyncio.Semaphore(1)
    )

    async def _boom():
        """Placeholder.

        Cost: expensive — expected p95 10 ms.
        """
        raise ValueError("planned failure")

    wrapped = cost_class.cost_limited(_boom)

    with pytest.raises(ValueError):
        await wrapped()
    # A second call must still acquire.
    with pytest.raises(ValueError):
        await wrapped()
