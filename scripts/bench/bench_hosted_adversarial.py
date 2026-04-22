"""Hosted MCP adversarial-load harness.

Purpose:
  * exercise the public-hosted HTTP posture under repeatable load
  * prove `rate_limited` shows up in `/metrics`
  * compare client-observed latency with server-side latency buckets
  * verify `/status` stays responsive while tool classes saturate

This is intentionally synthetic and deterministic enough for CI. It
does NOT try to model full-corpus wall-clock; it models the admission,
metrics, and responsiveness invariants we need before public launch.

Usage:
    uv run python scripts/bench/bench_hosted_adversarial.py
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from collections import Counter
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

import httpx
from fastmcp import Client
from fastmcp.exceptions import ToolError
from prometheus_client.parser import text_string_to_metric_families

from kernel_lore_mcp import _core

_SERVER_BIND = "127.0.0.1"
_STATUS_P95_BUDGET_MS = 500.0
_RATE_LIMITED = "rate_limited"
_PER_CALL_TIMEOUT_S = 10.0


@dataclass(frozen=True, slots=True)
class Scenario:
    name: str
    tool: str
    arguments: dict[str, Any]
    workers: int
    calls_per_worker: int
    expect_rate_limited: bool
    status_probe_calls: int
    status_probe_interval_ms: int


@dataclass(frozen=True, slots=True)
class HistogramSummary:
    count: int
    sum_seconds: float
    approx_p95_seconds: float | None


@dataclass(frozen=True, slots=True)
class ScenarioReport:
    name: str
    tool: str
    total_calls: int
    statuses: dict[str, int]
    client_p50_ms: float | None
    client_p95_ms: float | None
    status_probe_p50_ms: float | None
    status_probe_p95_ms: float | None
    server_request_ok: HistogramSummary
    server_request_rate_limited: HistogramSummary
    server_tool_ok: HistogramSummary
    server_tool_rate_limited: HistogramSummary


def _sample_messages() -> list[bytes]:
    return [
        b"From: Alice <alice@example.com>\r\n"
        b"Subject: [PATCH v3 1/2] ksmbd: tighten ACL bounds\r\n"
        b"Date: Mon, 14 Apr 2026 12:00:00 +0000\r\n"
        b"Message-ID: <m1@x>\r\n"
        b"\r\n"
        b"Prose here explaining the change.\r\n"
        b'Fixes: deadbeef01234567 ("ksmbd: initial ACL handling")\r\n'
        b"Reviewed-by: Carol <carol@example.com>\r\n"
        b"Signed-off-by: Alice <alice@example.com>\r\n"
        b"Cc: stable@vger.kernel.org # 5.15+\r\n"
        b"---\r\n"
        b"diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c\r\n"
        b"--- a/fs/smb/server/smbacl.c\r\n"
        b"+++ b/fs/smb/server/smbacl.c\r\n"
        b"@@ -1,1 +1,2 @@ int smb_check_perm_dacl(struct ksmbd_conn *c)\r\n"
        b" a\r\n"
        b"+b\r\n",
        b"From: Alice <alice@example.com>\r\n"
        b"Subject: [PATCH v3 2/2] ksmbd: follow-up\r\n"
        b"Date: Mon, 14 Apr 2026 12:05:00 +0000\r\n"
        b"Message-ID: <m2@x>\r\n"
        b"In-Reply-To: <m1@x>\r\n"
        b"References: <m1@x>\r\n"
        b"\r\n"
        b"More prose.\r\n"
        b"Signed-off-by: Alice <alice@example.com>\r\n"
        b"---\r\n"
        b"diff --git a/fs/smb/server/smb2pdu.c b/fs/smb/server/smb2pdu.c\r\n"
        b"--- a/fs/smb/server/smb2pdu.c\r\n"
        b"+++ b/fs/smb/server/smb2pdu.c\r\n"
        b"@@ -1,1 +1,2 @@ int smb2_create(struct ksmbd_conn *c)\r\n"
        b" a\r\n"
        b"+b\r\n",
        b"From: Carol <carol@example.com>\r\n"
        b"Subject: Re: [PATCH v3 1/2] ksmbd: tighten ACL bounds\r\n"
        b"Date: Mon, 14 Apr 2026 12:07:00 +0000\r\n"
        b"Message-ID: <m3@x>\r\n"
        b"In-Reply-To: <m1@x>\r\n"
        b"References: <m1@x>\r\n"
        b"\r\n"
        b"Looks sane to me.\r\n"
        b"Reviewed-by: Carol <carol@example.com>\r\n",
    ]


def _make_synthetic_shard(shard_dir: Path) -> Path:
    work = shard_dir.parent / f"{shard_dir.name}-work"
    work.mkdir(parents=True, exist_ok=True)
    env = {
        "GIT_AUTHOR_NAME": "tester",
        "GIT_AUTHOR_EMAIL": "t@e",
        "GIT_COMMITTER_NAME": "tester",
        "GIT_COMMITTER_EMAIL": "t@e",
    }

    def git(*args: str, cwd: Path) -> None:
        subprocess.run(
            ["git", *args],
            cwd=cwd,
            env=env,
            check=True,
            capture_output=True,
        )

    git("init", "-q", "-b", "master", ".", cwd=work)
    for idx, msg in enumerate(_sample_messages()):
        (work / "m").write_bytes(msg)
        git("add", "m", cwd=work)
        git("commit", "-q", "-m", f"m{idx}", cwd=work)

    if shard_dir.exists():
        shutil.rmtree(shard_dir)
    subprocess.run(
        ["git", "clone", "--bare", "-q", str(work), str(shard_dir)],
        check=True,
        capture_output=True,
    )
    return shard_dir


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind((_SERVER_BIND, 0))
        return int(s.getsockname()[1])


def _find_server_cmd(repo_root: Path) -> list[str]:
    cand = repo_root / ".venv" / "bin" / "kernel-lore-mcp"
    if cand.exists():
        return [str(cand)]
    which = shutil.which("kernel-lore-mcp")
    if which:
        return [which]
    raise RuntimeError("kernel-lore-mcp binary not found in .venv or PATH")


def _metric_snapshot(text: str) -> dict[tuple[str, tuple[tuple[str, str], ...]], float]:
    snap: dict[tuple[str, tuple[tuple[str, str], ...]], float] = {}
    for family in text_string_to_metric_families(text):
        for sample in family.samples:
            key = (
                sample.name,
                tuple(sorted((k, str(v)) for k, v in sample.labels.items())),
            )
            snap[key] = float(sample.value)
    return snap


def _counter_delta(
    before: dict[tuple[str, tuple[tuple[str, str], ...]], float],
    after: dict[tuple[str, tuple[tuple[str, str], ...]], float],
    name: str,
    labels: dict[str, str],
) -> float:
    key = (name, tuple(sorted(labels.items())))
    return after.get(key, 0.0) - before.get(key, 0.0)


def _histogram_delta(
    before: dict[tuple[str, tuple[tuple[str, str], ...]], float],
    after: dict[tuple[str, tuple[tuple[str, str], ...]], float],
    name: str,
    labels: dict[str, str],
) -> HistogramSummary:
    count = int(
        _counter_delta(
            before,
            after,
            f"{name}_count",
            labels,
        )
    )
    sum_seconds = _counter_delta(before, after, f"{name}_sum", labels)
    if count <= 0:
        return HistogramSummary(count=0, sum_seconds=0.0, approx_p95_seconds=None)

    buckets: list[tuple[float, float]] = []
    for (sample_name, sample_labels), after_value in after.items():
        if sample_name != f"{name}_bucket":
            continue
        labels_dict = dict(sample_labels)
        if any(labels_dict.get(k) != v for k, v in labels.items()):
            continue
        upper = float("inf") if labels_dict["le"] == "+Inf" else float(labels_dict["le"])
        before_value = before.get((sample_name, sample_labels), 0.0)
        buckets.append((upper, after_value - before_value))

    buckets.sort(key=lambda item: item[0])
    target = count * 0.95
    approx_p95 = None
    for upper, cumulative in buckets:
        if cumulative >= target:
            approx_p95 = upper
            break
    return HistogramSummary(
        count=count,
        sum_seconds=sum_seconds,
        approx_p95_seconds=approx_p95,
    )


def _percentile_ms(samples: list[float], q: float) -> float | None:
    if not samples:
        return None
    xs = sorted(samples)
    idx = min(len(xs) - 1, max(0, int(len(xs) * q)))
    return round(xs[idx], 3)


def _median_ms(samples: list[float]) -> float | None:
    if not samples:
        return None
    xs = sorted(samples)
    return round(xs[len(xs) // 2], 3)


def _classify_tool_error(exc: Exception) -> str:
    text = str(exc)
    if _RATE_LIMITED in text:
        return _RATE_LIMITED
    if "query_timeout" in text:
        return "query_timeout"
    if "hosted_restriction" in text:
        return "hosted_restriction"
    if isinstance(exc, ToolError):
        return "tool_error"
    return exc.__class__.__name__.lower()


async def _wait_for_server(port: int) -> None:
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.settimeout(0.2)
            try:
                s.connect((_SERVER_BIND, port))
                return
            except OSError:
                await asyncio.sleep(0.1)
    raise RuntimeError(f"server never bound port {port}")


async def _fetch_metrics(http: httpx.AsyncClient) -> dict[tuple[str, tuple[tuple[str, str], ...]], float]:
    r = await http.get("/metrics")
    r.raise_for_status()
    return _metric_snapshot(r.text)


async def _status_probe(
    http: httpx.AsyncClient,
    *,
    calls: int,
    interval_ms: int,
) -> list[float]:
    samples: list[float] = []
    for _ in range(calls):
        started = time.monotonic()
        r = await http.get("/status")
        r.raise_for_status()
        body = r.json()
        if body["service"] != "kernel-lore-mcp":
            raise RuntimeError(f"unexpected /status payload: {body}")
        samples.append((time.monotonic() - started) * 1000.0)
        await asyncio.sleep(interval_ms / 1000.0)
    return samples


async def _worker_run(
    url: str,
    scenario: Scenario,
    start_gate: asyncio.Event,
    statuses: Counter[str],
    client_latencies_ms: list[float],
) -> None:
    async with Client(url) as client:
        await start_gate.wait()
        for _ in range(scenario.calls_per_worker):
            started = time.monotonic()
            try:
                await asyncio.wait_for(
                    client.call_tool(scenario.tool, scenario.arguments),
                    timeout=_PER_CALL_TIMEOUT_S,
                )
            except TimeoutError:
                statuses["client_timeout"] += 1
            except Exception as exc:  # noqa: BLE001
                statuses[_classify_tool_error(exc)] += 1
            else:
                statuses["ok"] += 1
            finally:
                client_latencies_ms.append((time.monotonic() - started) * 1000.0)


async def _run_scenario(
    http: httpx.AsyncClient,
    *,
    url: str,
    scenario: Scenario,
) -> ScenarioReport:
    before = await _fetch_metrics(http)
    statuses: Counter[str] = Counter()
    client_latencies_ms: list[float] = []
    start_gate = asyncio.Event()
    workers = [
        asyncio.create_task(
            _worker_run(
                url,
                scenario,
                start_gate,
                statuses,
                client_latencies_ms,
            )
        )
        for _ in range(scenario.workers)
    ]

    status_task = asyncio.create_task(
        _status_probe(
            http,
            calls=scenario.status_probe_calls,
            interval_ms=scenario.status_probe_interval_ms,
        )
    )

    await asyncio.sleep(0.05)
    start_gate.set()
    await asyncio.gather(*workers)
    status_samples_ms = await status_task
    after = await _fetch_metrics(http)

    total_calls = scenario.workers * scenario.calls_per_worker
    if sum(statuses.values()) != total_calls:
        raise RuntimeError(
            f"{scenario.name}: expected {total_calls} calls, saw {sum(statuses.values())}"
        )
    unexpected = {k: v for k, v in statuses.items() if k not in {"ok", _RATE_LIMITED}}
    if unexpected:
        raise RuntimeError(f"{scenario.name}: unexpected statuses {unexpected}")
    saw_rate_limited = statuses.get(_RATE_LIMITED, 0) > 0
    if scenario.expect_rate_limited and not saw_rate_limited:
        raise RuntimeError(f"{scenario.name}: expected rate_limited responses, saw {statuses}")
    if not scenario.expect_rate_limited and saw_rate_limited:
        raise RuntimeError(f"{scenario.name}: unexpected rate_limited responses, saw {statuses}")

    status_p95 = _percentile_ms(status_samples_ms, 0.95)
    if status_p95 is not None and status_p95 > _STATUS_P95_BUDGET_MS:
        raise RuntimeError(
            f"{scenario.name}: /status p95 {status_p95:.3f} ms exceeded "
            f"budget {_STATUS_P95_BUDGET_MS:.1f} ms"
        )

    request_ok = _histogram_delta(
        before,
        after,
        "kernel_lore_mcp_request_latency_seconds",
        {"method": "tools/call", "status": "ok"},
    )
    request_rate_limited = _histogram_delta(
        before,
        after,
        "kernel_lore_mcp_request_latency_seconds",
        {"method": "tools/call", "status": _RATE_LIMITED},
    )
    tool_ok = _histogram_delta(
        before,
        after,
        "kernel_lore_mcp_tool_latency_seconds",
        {"tool": scenario.tool, "status": "ok"},
    )
    tool_rate_limited = _histogram_delta(
        before,
        after,
        "kernel_lore_mcp_tool_latency_seconds",
        {"tool": scenario.tool, "status": _RATE_LIMITED},
    )

    metrics_rate_limited = int(
        _counter_delta(
            before,
            after,
            "kernel_lore_mcp_requests_total",
            {"method": "tools/call", "status": _RATE_LIMITED},
        )
    )
    if scenario.expect_rate_limited and metrics_rate_limited <= 0:
        raise RuntimeError(f"{scenario.name}: metrics did not record rate_limited")

    return ScenarioReport(
        name=scenario.name,
        tool=scenario.tool,
        total_calls=total_calls,
        statuses=dict(statuses),
        client_p50_ms=_median_ms(client_latencies_ms),
        client_p95_ms=_percentile_ms(client_latencies_ms, 0.95),
        status_probe_p50_ms=_median_ms(status_samples_ms),
        status_probe_p95_ms=_percentile_ms(status_samples_ms, 0.95),
        server_request_ok=request_ok,
        server_request_rate_limited=request_rate_limited,
        server_tool_ok=tool_ok,
        server_tool_rate_limited=tool_rate_limited,
    )


async def _run_harness(json_out: Path | None = None) -> int:
    repo_root = Path(__file__).resolve().parents[2]
    port = _free_port()
    server_cmd = _find_server_cmd(repo_root)

    with tempfile.TemporaryDirectory(prefix="klmcp-hosted-harness-") as tmp:
        tmp_path = Path(tmp)
        shard_dir = tmp_path / "shards" / "0.git"
        shard_dir.parent.mkdir(parents=True)
        _make_synthetic_shard(shard_dir)
        data_dir = tmp_path / "data"
        data_dir.mkdir()
        _core.ingest_shard(
            data_dir=data_dir,
            shard_path=shard_dir,
            list="linux-cifs",
            shard="0",
            run_id="hosted-load-harness",
        )

        env = os.environ.copy()
        env["KLMCP_DATA_DIR"] = str(data_dir)
        env["KLMCP_LOG_LEVEL"] = "WARNING"
        env["KLMCP_MODE"] = "hosted"
        env["KLMCP_COST_CAP_MODERATE"] = "1"
        env["KLMCP_COST_CAP_EXPENSIVE"] = "1"
        server_log = tmp_path / "server.log"

        with server_log.open("wb") as log_handle:
            proc = subprocess.Popen(
                [
                    *server_cmd,
                    "serve",
                    "--transport",
                    "http",
                    "--mode",
                    "hosted",
                    "--host",
                    _SERVER_BIND,
                    "--port",
                    str(port),
                ],
                env=env,
                stdout=subprocess.DEVNULL,
                stderr=log_handle,
                cwd=repo_root,
            )
            try:
                await _wait_for_server(port)
                base_url = f"http://{_SERVER_BIND}:{port}"
                mcp_url = f"{base_url}/mcp/"
                scenarios = [
                    Scenario(
                        name="cheap_flood",
                        tool="lore_eq",
                        arguments={"field": "from_addr", "value": "alice@example.com"},
                        workers=8,
                        calls_per_worker=6,
                        expect_rate_limited=False,
                        status_probe_calls=20,
                        status_probe_interval_ms=15,
                    ),
                    Scenario(
                        name="moderate_saturation",
                        tool="lore_patch_search",
                        arguments={"needle": "smb_check_perm_dacl", "list": "linux-cifs"},
                        workers=8,
                        calls_per_worker=1,
                        expect_rate_limited=True,
                        status_probe_calls=20,
                        status_probe_interval_ms=15,
                    ),
                    Scenario(
                        name="expensive_saturation",
                        tool="lore_explain_patch",
                        arguments={"message_id": "m1@x"},
                        workers=8,
                        calls_per_worker=1,
                        expect_rate_limited=True,
                        status_probe_calls=20,
                        status_probe_interval_ms=15,
                    ),
                ]

                reports: list[ScenarioReport] = []
                async with httpx.AsyncClient(base_url=base_url, timeout=10.0) as http:
                    for scenario in scenarios:
                        reports.append(await _run_scenario(http, url=mcp_url, scenario=scenario))

                summary = {
                    "mode": "hosted",
                    "bind": _SERVER_BIND,
                    "port": port,
                    "status_p95_budget_ms": _STATUS_P95_BUDGET_MS,
                    "reports": [asdict(report) for report in reports],
                }
                if json_out is not None:
                    json_out.write_text(json.dumps(summary, indent=2, sort_keys=True))
                print(json.dumps(summary, indent=2, sort_keys=True))
                return 0
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=2)
                if proc.returncode not in (0, -15, 143):
                    stderr = server_log.read_text(errors="replace")
                    raise RuntimeError(f"server exited {proc.returncode}\n{stderr}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="bench_hosted_adversarial")
    parser.add_argument(
        "--json-out",
        type=Path,
        default=None,
        help="Optional path to write the JSON report.",
    )
    args = parser.parse_args(argv)
    return asyncio.run(_run_harness(json_out=args.json_out))


if __name__ == "__main__":
    sys.exit(main())
