"""Concurrent, multi-tool regression bench.

Complements:
  * scratch/bench_mcp_latency.py — single-threaded, per-tool shape.
  * scratch/bench_over_concurrency.py — concurrent but one tool.

This one mixes tool types under concurrency and reports per-tool
p50/p95/p99 + aggregate RPS. The goal is to catch regressions in
the Reader caches (BmReader, Store), the over.db pool, the rate
limiter, and the deadline wiring — the things that should make
concurrent load cheap and bad-apple queries reject fast.

Each worker loops a fixed duration, picking a tool at random per
iteration. Worker count sweeps via `CONCURRENCY` constant.

Usage:
    KLMCP_DATA_DIR=/home/mjbommar/klmcp-local \
      .venv/bin/python scratch/bench_concurrent_mixed.py
"""

from __future__ import annotations

import os
import random
import statistics
import threading
import time
from collections import defaultdict
from collections.abc import Callable
from pathlib import Path
from typing import Any

from kernel_lore_mcp import _core

DATA_DIR = Path(os.environ.get("KLMCP_DATA_DIR", "/home/mjbommar/klmcp-local"))
DURATION_S = float(os.environ.get("KLMCP_BENCH_DURATION_S", "15"))
CONCURRENCY_LEVELS = [1, 4, 16, 32]

SEED_MIDS = [
    "20200403073947.3352D8152C@busybox.osuosl.org",
    "1438938514-10304-1-git-send-email-pablo.de.lara.guarch@intel.com",
    "20150125180702.928127045@linuxfoundation.org",
    "20200519063123.20673-3-chris@chris-wilson.co.uk",
    "20191223143538.20327-2-enric.balletbo@collabora.com",
]
SEED_FROMS = [
    "gregkh@linuxfoundation.org",
    "akpm@linux-foundation.org",
    "davem@davemloft.net",
]
SEED_LISTS = ["lkml", "netdev", "bpf", "linux-arm-kernel"]
SEED_TEXT = [
    "buffer overflow",
    "refcount leak",
    "netfilter",
    "io_uring",
    "use after free",  # exercises hyphen-split rewrite
]


def call_fetch_message(r) -> Any:
    return r.fetch_message(random.choice(SEED_MIDS))


def call_eq_from(r) -> Any:
    return r.eq(
        "from_addr",
        random.choice(SEED_FROMS),
        None,
        None,
        None,
        50,
    )


def call_eq_list(r) -> Any:
    return r.eq(
        "list",
        random.choice(SEED_LISTS),
        None,
        None,
        None,
        50,
    )


def call_thread(r) -> Any:
    return r.thread(random.choice(SEED_MIDS), 50)


def call_router(r) -> Any:
    return r.router_search(random.choice(SEED_TEXT), 10)


def call_prose(r) -> Any:
    return r.prose_search(random.choice(SEED_TEXT), 10)


def call_series(r) -> Any:
    return r.series_timeline(random.choice(SEED_MIDS))


def call_expand_citation(r) -> Any:
    return r.expand_citation(random.choice(SEED_MIDS), 5)


TOOLS: dict[str, Callable[[Any], Any]] = {
    "fetch_message": call_fetch_message,
    "eq(from_addr)": call_eq_from,
    "eq(list)": call_eq_list,
    "thread": call_thread,
    "router_search": call_router,
    "prose_search": call_prose,
    "series_timeline": call_series,
    "expand_citation": call_expand_citation,
}


def run(reader, n_threads: int, duration_s: float) -> None:
    # Per-tool latency samples. Mutated from multiple threads;
    # list.append is GIL-safe enough for this bench.
    samples: dict[str, list[float]] = defaultdict(list)
    errors: dict[str, int] = defaultdict(int)
    stop_at = time.monotonic() + duration_s
    barrier = threading.Barrier(n_threads + 1)

    def worker(seed: int) -> None:
        rnd = random.Random(seed)
        barrier.wait()
        while time.monotonic() < stop_at:
            name, fn = rnd.choice(list(TOOLS.items()))
            t0 = time.monotonic()
            try:
                fn(reader)
                samples[name].append((time.monotonic() - t0) * 1000.0)
            except Exception:  # noqa: BLE001
                errors[name] += 1

    threads = [
        threading.Thread(target=worker, args=(i,))
        for i in range(n_threads)
    ]
    for t in threads:
        t.start()
    barrier.wait()  # release workers
    t0 = time.monotonic()
    for t in threads:
        t.join()
    wall = time.monotonic() - t0

    total_ops = sum(len(v) for v in samples.values())
    rps = total_ops / wall

    print(f"\n--- concurrency={n_threads} wall={wall:.1f}s total_ops={total_ops} RPS={rps:.0f} ---")
    print(f"  {'tool':22s}  {'n':>6s}  {'p50':>7s}  {'p95':>7s}  {'p99':>7s}  {'max':>7s}  {'errs':>5s}")
    for name in TOOLS.keys():
        xs = sorted(samples.get(name, []))
        errs = errors.get(name, 0)
        if not xs:
            print(f"  {name:22s}  {'—':>6s}   (no samples) errs={errs}")
            continue
        n = len(xs)
        p50 = xs[n // 2]
        p95 = xs[min(n - 1, int(n * 0.95))]
        p99 = xs[min(n - 1, int(n * 0.99))]
        mx = xs[-1]
        print(f"  {name:22s}  {n:6d}  {p50:6.2f}  {p95:6.2f}  {p99:6.2f}  {mx:6.2f}  {errs:5d}")
    print(f"  aggregate mean per-call: {statistics.mean([x for v in samples.values() for x in v]):.2f} ms")


def main() -> None:
    print(f"=== Concurrent mixed-tool bench on {DATA_DIR} ===")
    print(f"duration_s={DURATION_S}  tools={list(TOOLS)}")
    reader = _core.Reader(str(DATA_DIR))
    # Warm caches.
    for tool in TOOLS.values():
        try:
            tool(reader)
        except Exception:  # noqa: BLE001
            pass
    for c in CONCURRENCY_LEVELS:
        run(reader, c, DURATION_S)


if __name__ == "__main__":
    main()
