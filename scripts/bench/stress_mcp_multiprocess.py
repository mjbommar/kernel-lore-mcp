"""Async + multiprocess MCP stress harness.

Purpose:
  * drive real hosted MCP load from multiple Python processes
  * keep each child process busy with many asyncio workers
  * mix cheap / moderate / expensive tools under one configurable run
  * probe `/status` from the parent while the load is in flight

This complements the existing single-process benches in this repo.
The goal here is not micro-benchmark precision; it is to push a live
server hard enough to expose admission-control, queueing, and general
HTTP/MCP responsiveness issues.

Examples:
    uv run python scripts/bench/stress_mcp_multiprocess.py \
      --base-url http://s6:8080 \
      --scenario mixed_hot \
      --processes 4 \
      --concurrency-per-process 24 \
      --duration-seconds 60

    uv run python scripts/bench/stress_mcp_multiprocess.py \
      --base-url http://s6:8080 \
      --scenario all \
      --processes 6 \
      --concurrency-per-process 32 \
      --duration-seconds 45 \
      --json-out /tmp/klmcp-stress.json
"""

from __future__ import annotations

import argparse
import asyncio
import concurrent.futures
import json
import multiprocessing
import os
import random
import sys
import threading
import time
from collections import Counter, defaultdict
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

import httpx
from fastmcp import Client
from fastmcp.exceptions import ToolError

_OK = "ok"
_RATE_LIMITED = "rate_limited"
_DEFAULT_DURATION_SECONDS = 30.0
_DEFAULT_REQUEST_TIMEOUT_SECONDS = 15.0
_DEFAULT_STATUS_TIMEOUT_SECONDS = 5.0
_DEFAULT_STATUS_INTERVAL_MS = 1000
_DEFAULT_LATENCY_SAMPLE_CAP = 5000
_SCENARIO_ALL = "all"


@dataclass(frozen=True, slots=True)
class ToolPlan:
    tool: str
    arguments: dict[str, Any]
    weight: int = 1


@dataclass(frozen=True, slots=True)
class ScenarioPlan:
    name: str
    tools: tuple[ToolPlan, ...]


class LatencyReservoir:
    """Reservoir-sample latency values so the client does not OOM itself."""

    def __init__(self, cap: int, *, seed: int) -> None:
        self._cap = max(1, cap)
        self._rng = random.Random(seed)  # noqa: S311 - deterministic bench sampling
        self._seen = 0
        self._samples: list[float] = []

    def add(self, value_ms: float) -> None:
        self._seen += 1
        if len(self._samples) < self._cap:
            self._samples.append(value_ms)
            return
        slot = self._rng.randrange(self._seen)
        if slot < self._cap:
            self._samples[slot] = value_ms

    def values(self) -> list[float]:
        return list(self._samples)


def _log(message: str) -> None:
    print(f"[stress] {message}", file=sys.stderr, flush=True)


def _classify_error(exc: Exception) -> str:
    text = str(exc).lower()
    if _RATE_LIMITED in text or "rate limit" in text or "429" in text:
        return _RATE_LIMITED
    if "query_timeout" in text:
        return "query_timeout"
    if "hosted_restriction" in text:
        return "hosted_restriction"
    if isinstance(exc, ToolError):
        return "tool_error"
    return exc.__class__.__name__.lower()


def _status_subset(payload: dict[str, Any]) -> dict[str, Any]:
    return {
        "version": payload.get("version"),
        "generation": payload.get("generation"),
        "freshness_ok": payload.get("freshness_ok"),
        "last_ingest_age_seconds": payload.get("last_ingest_age_seconds"),
    }


def _percentile_ms(samples: list[float], q: float) -> float | None:
    if not samples:
        return None
    xs = sorted(samples)
    idx = max(0, min(len(xs) - 1, int((len(xs) - 1) * q)))
    return round(xs[idx], 3)


def _latency_summary(samples: list[float]) -> dict[str, Any]:
    if not samples:
        return {
            "sample_count": 0,
            "p50_ms": None,
            "p95_ms": None,
            "p99_ms": None,
            "max_ms": None,
        }
    xs = sorted(samples)
    return {
        "sample_count": len(xs),
        "p50_ms": _percentile_ms(xs, 0.50),
        "p95_ms": _percentile_ms(xs, 0.95),
        "p99_ms": _percentile_ms(xs, 0.99),
        "max_ms": round(xs[-1], 3),
    }


def _build_mcp_url(base_url: str, explicit_mcp_url: str | None) -> str:
    if explicit_mcp_url:
        return explicit_mcp_url
    return base_url.rstrip("/") + "/mcp/"


def _fetch_status_summary(base_url: str, *, timeout_seconds: float) -> dict[str, Any] | None:
    try:
        with httpx.Client(timeout=timeout_seconds) as client:
            response = client.get(base_url.rstrip("/") + "/status")
            response.raise_for_status()
            return _status_subset(response.json())
    except Exception:
        return None


def _status_probe_loop(
    *,
    base_url: str,
    timeout_seconds: float,
    interval_ms: int,
    sample_cap: int,
    seed: int,
    stop_event: threading.Event,
    sink: dict[str, Any],
) -> None:
    latencies = LatencyReservoir(sample_cap, seed=seed)
    failures: Counter[str] = Counter()
    last_status: dict[str, Any] | None = None
    url = base_url.rstrip("/") + "/status"

    with httpx.Client(timeout=timeout_seconds) as client:
        while not stop_event.is_set():
            started = time.perf_counter()
            try:
                response = client.get(url)
                response.raise_for_status()
                last_status = _status_subset(response.json())
            except Exception as exc:
                failures[_classify_error(exc)] += 1
            else:
                latencies.add((time.perf_counter() - started) * 1000.0)
            finally:
                stop_event.wait(interval_ms / 1000.0)

    sink["latency_samples_ms"] = latencies.values()
    sink["failures"] = dict(failures)
    sink["last_status"] = last_status


def _pick_tool(tools: list[dict[str, Any]], total_weight: int, rng: random.Random) -> dict[str, Any]:
    pick = rng.randint(1, total_weight)
    running = 0
    for tool in tools:
        running += int(tool["weight"])
        if pick <= running:
            return tool
    return tools[-1]


async def _worker_loop(
    *,
    mcp_url: str,
    tools: list[dict[str, Any]],
    total_weight: int,
    request_timeout_seconds: float,
    stop_at: float | None,
    calls_per_worker: int | None,
    start_gate: asyncio.Event,
    global_statuses: Counter[str],
    global_latencies: LatencyReservoir,
    per_tool_statuses: dict[str, Counter[str]],
    per_tool_latencies: dict[str, LatencyReservoir],
    seed: int,
) -> None:
    rng = random.Random(seed)  # noqa: S311 - deterministic bench sampling
    calls_made = 0
    async with Client(mcp_url) as client:
        await start_gate.wait()
        while True:
            if calls_per_worker is not None and calls_made >= calls_per_worker:
                return
            if stop_at is not None and time.monotonic() >= stop_at:
                return

            plan = _pick_tool(tools, total_weight, rng)
            tool_name = str(plan["tool"])
            started = time.perf_counter()
            status = _OK

            try:
                await asyncio.wait_for(
                    client.call_tool(tool_name, plan["arguments"]),
                    timeout=request_timeout_seconds,
                )
            except TimeoutError:
                status = "client_timeout"
            except Exception as exc:
                status = _classify_error(exc)
            finally:
                elapsed_ms = (time.perf_counter() - started) * 1000.0
                global_statuses[status] += 1
                global_latencies.add(elapsed_ms)
                per_tool_statuses[tool_name][status] += 1
                per_tool_latencies[tool_name].add(elapsed_ms)
                calls_made += 1


async def _run_process_async(config: dict[str, Any]) -> dict[str, Any]:
    tools: list[dict[str, Any]] = config["tools"]
    total_weight = sum(int(tool["weight"]) for tool in tools)
    global_statuses: Counter[str] = Counter()
    global_latencies = LatencyReservoir(config["latency_sample_cap"], seed=config["seed"])
    per_tool_statuses: dict[str, Counter[str]] = defaultdict(Counter)
    per_tool_latencies = {
        str(tool["tool"]): LatencyReservoir(config["latency_sample_cap"], seed=config["seed"] + idx + 1)
        for idx, tool in enumerate(tools)
    }

    start_gate = asyncio.Event()
    stop_at = None
    if config["duration_seconds"] is not None:
        stop_at = time.monotonic() + float(config["duration_seconds"])

    tasks = [
        asyncio.create_task(
            _worker_loop(
                mcp_url=config["mcp_url"],
                tools=tools,
                total_weight=total_weight,
                request_timeout_seconds=float(config["request_timeout_seconds"]),
                stop_at=stop_at,
                calls_per_worker=config["calls_per_worker"],
                start_gate=start_gate,
                global_statuses=global_statuses,
                global_latencies=global_latencies,
                per_tool_statuses=per_tool_statuses,
                per_tool_latencies=per_tool_latencies,
                seed=config["seed"] + worker_idx,
            )
        )
        for worker_idx in range(int(config["concurrency_per_process"]))
    ]
    await asyncio.sleep(0)
    start_gate.set()
    await asyncio.gather(*tasks)

    return {
        "pid": os.getpid(),
        "statuses": dict(global_statuses),
        "latency_samples_ms": global_latencies.values(),
        "per_tool_statuses": {tool: dict(counts) for tool, counts in per_tool_statuses.items()},
        "per_tool_latency_samples_ms": {
            tool: reservoir.values() for tool, reservoir in per_tool_latencies.items()
        },
    }


def _run_process(config: dict[str, Any]) -> dict[str, Any]:
    return asyncio.run(_run_process_async(config))


async def _resolve_message_id(
    *,
    mcp_url: str,
    list_name: str,
    needle: str,
    request_timeout_seconds: float,
) -> str:
    async with Client(mcp_url) as client:
        result = await asyncio.wait_for(
            client.call_tool(
                "lore_patch_search",
                {"needle": needle, "list": list_name, "limit": 1},
            ),
            timeout=request_timeout_seconds,
        )
    rows = getattr(result.data, "results", None)
    if not rows:
        raise RuntimeError(
            "could not bootstrap a message_id from lore_patch_search; "
            f"try --message-id explicitly or adjust --list/--needle (list={list_name!r}, needle={needle!r})"
        )
    return str(rows[0].message_id)


def _scenario_catalog(
    *,
    field: str,
    value: str,
    limit: int,
    list_name: str,
    needle: str,
    message_id: str | None,
) -> dict[str, ScenarioPlan]:
    cheap_eq = ScenarioPlan(
        name="cheap_eq",
        tools=(
            ToolPlan(
                tool="lore_eq",
                arguments={"field": field, "value": value, "limit": limit},
            ),
        ),
    )
    moderate_patch_search = ScenarioPlan(
        name="moderate_patch_search",
        tools=(
            ToolPlan(
                tool="lore_patch_search",
                arguments={"needle": needle, "list": list_name, "limit": min(limit, 20)},
            ),
        ),
    )

    catalog: dict[str, ScenarioPlan] = {
        cheap_eq.name: cheap_eq,
        moderate_patch_search.name: moderate_patch_search,
    }

    if message_id is not None:
        catalog["expensive_explain"] = ScenarioPlan(
            name="expensive_explain",
            tools=(
                ToolPlan(tool="lore_explain_patch", arguments={"message_id": message_id}),
            ),
        )
        catalog["mixed_hot"] = ScenarioPlan(
            name="mixed_hot",
            tools=(
                ToolPlan(
                    tool="lore_eq",
                    arguments={"field": field, "value": value, "limit": limit},
                    weight=6,
                ),
                ToolPlan(
                    tool="lore_patch_search",
                    arguments={"needle": needle, "list": list_name, "limit": min(limit, 10)},
                    weight=4,
                ),
                ToolPlan(tool="lore_patch", arguments={"message_id": message_id}, weight=2),
                ToolPlan(
                    tool="lore_thread",
                    arguments={
                        "message_id": message_id,
                        "max_messages": 25,
                        "response_format": "concise",
                    },
                    weight=2,
                ),
                ToolPlan(tool="lore_explain_patch", arguments={"message_id": message_id}, weight=1),
            ),
        )

    return catalog


def _select_scenarios(
    *,
    requested_names: list[str],
    catalog: dict[str, ScenarioPlan],
) -> list[ScenarioPlan]:
    if not requested_names:
        default_name = "mixed_hot" if "mixed_hot" in catalog else "moderate_patch_search"
        requested_names = [default_name]

    names: list[str] = []
    for name in requested_names:
        if name == _SCENARIO_ALL:
            names.extend(k for k in ("cheap_eq", "moderate_patch_search", "expensive_explain", "mixed_hot") if k in catalog)
        else:
            names.append(name)

    scenarios: list[ScenarioPlan] = []
    seen: set[str] = set()
    for name in names:
        if name in seen:
            continue
        try:
            scenarios.append(catalog[name])
        except KeyError as exc:
            available = ", ".join(sorted(catalog))
            raise SystemExit(f"unknown scenario {name!r}; available: {available}, {_SCENARIO_ALL}") from exc
        seen.add(name)
    return scenarios


def _aggregate_scenario(
    *,
    scenario: ScenarioPlan,
    worker_reports: list[dict[str, Any]],
    wall_seconds: float,
    status_before: dict[str, Any] | None,
    status_after: dict[str, Any] | None,
    status_probe: dict[str, Any] | None,
    latency_sample_cap: int,
) -> dict[str, Any]:
    statuses: Counter[str] = Counter()
    latency_samples: list[float] = []
    per_tool_statuses: dict[str, Counter[str]] = defaultdict(Counter)
    per_tool_latency_samples: dict[str, list[float]] = defaultdict(list)
    worker_pids: list[int] = []

    for report in worker_reports:
        worker_pids.append(int(report["pid"]))
        statuses.update(report["statuses"])
        latency_samples.extend(report["latency_samples_ms"])
        for tool, counts in report["per_tool_statuses"].items():
            per_tool_statuses[tool].update(counts)
        for tool, samples in report["per_tool_latency_samples_ms"].items():
            per_tool_latency_samples[tool].extend(samples)

    total_calls = sum(statuses.values())
    per_tool: dict[str, Any] = {}
    for tool, counts in sorted(per_tool_statuses.items()):
        per_tool[tool] = {
            "calls": sum(counts.values()),
            "statuses": dict(counts),
            "latency_ms": _latency_summary(per_tool_latency_samples.get(tool, [])),
        }

    summary = {
        "name": scenario.name,
        "tools": [asdict(tool) for tool in scenario.tools],
        "worker_pids": worker_pids,
        "wall_seconds": round(wall_seconds, 3),
        "total_calls": total_calls,
        "requests_per_second": round(total_calls / wall_seconds, 3) if wall_seconds > 0 else None,
        "statuses": dict(statuses),
        "latency_ms": _latency_summary(latency_samples),
        "latency_sample_cap_per_process": latency_sample_cap,
        "status_before": status_before,
        "status_after": status_after,
        "status_probe": status_probe,
        "per_tool": per_tool,
    }
    return summary


def _run_scenario(
    *,
    scenario: ScenarioPlan,
    base_url: str,
    mcp_url: str,
    processes: int,
    concurrency_per_process: int,
    duration_seconds: float | None,
    calls_per_worker: int | None,
    request_timeout_seconds: float,
    status_timeout_seconds: float,
    status_interval_ms: int,
    latency_sample_cap: int,
    seed: int,
) -> dict[str, Any]:
    _log(
        f"scenario={scenario.name} processes={processes} "
        f"concurrency_per_process={concurrency_per_process} "
        f"duration_seconds={duration_seconds!r} calls_per_worker={calls_per_worker!r}"
    )
    status_before = _fetch_status_summary(base_url, timeout_seconds=status_timeout_seconds)

    status_probe_result: dict[str, Any] | None = None
    probe_stop = threading.Event()
    probe_thread: threading.Thread | None = None
    if status_interval_ms > 0:
        status_probe_result = {}
        probe_thread = threading.Thread(
            target=_status_probe_loop,
            kwargs={
                "base_url": base_url,
                "timeout_seconds": status_timeout_seconds,
                "interval_ms": status_interval_ms,
                "sample_cap": latency_sample_cap,
                "seed": seed + 10_000,
                "stop_event": probe_stop,
                "sink": status_probe_result,
            },
            daemon=True,
        )
        probe_thread.start()

    configs = [
        {
            "mcp_url": mcp_url,
            "tools": [asdict(tool) for tool in scenario.tools],
            "concurrency_per_process": concurrency_per_process,
            "duration_seconds": duration_seconds,
            "calls_per_worker": calls_per_worker,
            "request_timeout_seconds": request_timeout_seconds,
            "latency_sample_cap": latency_sample_cap,
            "seed": seed + (idx * 1000),
        }
        for idx in range(processes)
    ]

    started = time.perf_counter()
    ctx = multiprocessing.get_context("spawn")
    with concurrent.futures.ProcessPoolExecutor(max_workers=processes, mp_context=ctx) as pool:
        futures = [pool.submit(_run_process, config) for config in configs]
        worker_reports = [future.result() for future in futures]
    wall_seconds = time.perf_counter() - started

    if probe_thread is not None and status_probe_result is not None:
        probe_stop.set()
        probe_thread.join(timeout=5)
        if "latency_samples_ms" not in status_probe_result:
            status_probe_result["latency_samples_ms"] = []
            status_probe_result["failures"] = {"probe_join_timeout": 1}
            status_probe_result["last_status"] = None

    status_after = _fetch_status_summary(base_url, timeout_seconds=status_timeout_seconds)
    status_probe_summary = None
    if status_probe_result is not None:
        status_probe_summary = {
            "interval_ms": status_interval_ms,
            "latency_ms": _latency_summary(status_probe_result["latency_samples_ms"]),
            "failures": status_probe_result["failures"],
            "last_status": status_probe_result["last_status"],
        }

    return _aggregate_scenario(
        scenario=scenario,
        worker_reports=worker_reports,
        wall_seconds=wall_seconds,
        status_before=status_before,
        status_after=status_after,
        status_probe=status_probe_summary,
        latency_sample_cap=latency_sample_cap,
    )


async def _run_harness(args: argparse.Namespace) -> dict[str, Any]:
    base_url = args.base_url.rstrip("/")
    mcp_url = _build_mcp_url(base_url, args.mcp_url)

    bootstrap_message_id = args.message_id
    if bootstrap_message_id is None and any(
        name in (args.scenario or [])
        for name in (_SCENARIO_ALL, "expensive_explain", "mixed_hot")
    ):
        _log(f"bootstrapping message_id via lore_patch_search list={args.list_name} needle={args.needle!r}")
        bootstrap_message_id = await _resolve_message_id(
            mcp_url=mcp_url,
            list_name=args.list_name,
            needle=args.needle,
            request_timeout_seconds=args.request_timeout_seconds,
        )
        _log(f"bootstrapped message_id={bootstrap_message_id}")

    catalog = _scenario_catalog(
        field=args.field,
        value=args.value,
        limit=args.limit,
        list_name=args.list_name,
        needle=args.needle,
        message_id=bootstrap_message_id,
    )
    scenarios = _select_scenarios(requested_names=args.scenario or [], catalog=catalog)

    reports: list[dict[str, Any]] = []
    for index, scenario in enumerate(scenarios):
        reports.append(
            _run_scenario(
                scenario=scenario,
                base_url=base_url,
                mcp_url=mcp_url,
                processes=args.processes,
                concurrency_per_process=args.concurrency_per_process,
                duration_seconds=args.duration_seconds if args.calls_per_worker is None else None,
                calls_per_worker=args.calls_per_worker,
                request_timeout_seconds=args.request_timeout_seconds,
                status_timeout_seconds=args.status_timeout_seconds,
                status_interval_ms=args.status_interval_ms,
                latency_sample_cap=args.latency_sample_cap,
                seed=args.seed + (index * 100_000),
            )
        )

    return {
        "base_url": base_url,
        "mcp_url": mcp_url,
        "processes": args.processes,
        "concurrency_per_process": args.concurrency_per_process,
        "duration_seconds": args.duration_seconds if args.calls_per_worker is None else None,
        "calls_per_worker": args.calls_per_worker,
        "request_timeout_seconds": args.request_timeout_seconds,
        "status_timeout_seconds": args.status_timeout_seconds,
        "status_interval_ms": args.status_interval_ms,
        "latency_sample_cap": args.latency_sample_cap,
        "field": args.field,
        "value": args.value,
        "list_name": args.list_name,
        "needle": args.needle,
        "bootstrap_message_id": bootstrap_message_id,
        "reports": reports,
    }


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="stress_mcp_multiprocess")
    parser.add_argument(
        "--base-url",
        default="http://127.0.0.1:8080",
        help="HTTP base URL for the target server (default: %(default)s).",
    )
    parser.add_argument(
        "--mcp-url",
        default=None,
        help="Optional explicit MCP endpoint. Defaults to <base-url>/mcp/.",
    )
    parser.add_argument(
        "--scenario",
        action="append",
        default=None,
        help=(
            "Scenario name; repeat to run multiple. "
            "Choices: cheap_eq, moderate_patch_search, expensive_explain, mixed_hot, all."
        ),
    )
    parser.add_argument(
        "--processes",
        type=int,
        default=max(1, min(4, os.cpu_count() or 1)),
        help="Number of OS processes to spawn (default: %(default)s).",
    )
    parser.add_argument(
        "--concurrency-per-process",
        type=int,
        default=16,
        help="Async workers per child process (default: %(default)s).",
    )
    parser.add_argument(
        "--duration-seconds",
        type=float,
        default=_DEFAULT_DURATION_SECONDS,
        help="Wall-clock duration per scenario when --calls-per-worker is unset (default: %(default)s).",
    )
    parser.add_argument(
        "--calls-per-worker",
        type=int,
        default=None,
        help="Fixed calls per async worker. Overrides --duration-seconds when set.",
    )
    parser.add_argument(
        "--request-timeout-seconds",
        type=float,
        default=_DEFAULT_REQUEST_TIMEOUT_SECONDS,
        help="Per-tool client timeout (default: %(default)s).",
    )
    parser.add_argument(
        "--status-timeout-seconds",
        type=float,
        default=_DEFAULT_STATUS_TIMEOUT_SECONDS,
        help="Timeout for parent-side /status probes (default: %(default)s).",
    )
    parser.add_argument(
        "--status-interval-ms",
        type=int,
        default=_DEFAULT_STATUS_INTERVAL_MS,
        help="Parent-side /status probe interval in ms; 0 disables probing (default: %(default)s).",
    )
    parser.add_argument(
        "--latency-sample-cap",
        type=int,
        default=_DEFAULT_LATENCY_SAMPLE_CAP,
        help="Reservoir-sample cap per process and per tool (default: %(default)s).",
    )
    parser.add_argument(
        "--field",
        default="list",
        help="Field used by the cheap_eq scenario (default: %(default)s).",
    )
    parser.add_argument(
        "--value",
        default="linux-pci",
        help="Value used by the cheap_eq scenario (default: %(default)s).",
    )
    parser.add_argument(
        "--list",
        dest="list_name",
        default="linux-pci",
        help="List filter used by patch-search bootstrap and moderate scenarios (default: %(default)s).",
    )
    parser.add_argument(
        "--needle",
        default="pci_enable_device",
        help="Needle used by patch-search bootstrap and moderate scenarios (default: %(default)s).",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=20,
        help="Result limit used by search-style scenarios (default: %(default)s).",
    )
    parser.add_argument(
        "--message-id",
        default=None,
        help="Explicit message-id for expensive / message-centric scenarios. Optional.",
    )
    parser.add_argument(
        "--seed",
        type=int,
        default=1,
        help="Base RNG seed for repeatable sampling (default: %(default)s).",
    )
    parser.add_argument(
        "--json-out",
        type=Path,
        default=None,
        help="Optional path to write the JSON report.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)

    if args.processes < 1:
        raise SystemExit("--processes must be >= 1")
    if args.concurrency_per_process < 1:
        raise SystemExit("--concurrency-per-process must be >= 1")
    if args.duration_seconds <= 0 and args.calls_per_worker is None:
        raise SystemExit("--duration-seconds must be > 0 when --calls-per-worker is unset")
    if args.calls_per_worker is not None and args.calls_per_worker < 1:
        raise SystemExit("--calls-per-worker must be >= 1")
    if args.status_interval_ms < 0:
        raise SystemExit("--status-interval-ms must be >= 0")
    if args.latency_sample_cap < 1:
        raise SystemExit("--latency-sample-cap must be >= 1")

    summary = asyncio.run(_run_harness(args))
    rendered = json.dumps(summary, indent=2, sort_keys=True)
    if args.json_out is not None:
        args.json_out.write_text(rendered + "\n")
    print(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
