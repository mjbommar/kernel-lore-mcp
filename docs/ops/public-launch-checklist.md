# Public Launch Checklist

This is the "do we actually trust this hosted box?" checklist for a
full-corpus public deployment.

The goal is not paperwork. The goal is to keep us from launching a
box that is technically up but operationally blind, query-hostile, or
quietly broken.

## 1. Corpus and shard health

- `kernel-lore-mcp status --data-dir "$KLMCP_DATA_DIR"` reports
  `generation >= 1` and `freshness_ok == true`.
- `kernel-lore-doctor --data-dir "$KLMCP_DATA_DIR"` reports:
  `healthy == total`, `broken == 0`, `repairable == 0`.
- `over_db_ready/open_ok == true/true`.
- Expected tier markers are present for the surface we are exposing:
  `bm25`, `trigram`, `tid`, `path_vocab`.

## 2. Hosted posture

- The server is running with `KLMCP_MODE=hosted` or
  `kernel-lore-mcp serve --mode hosted`.
- Bind is intentional:
  `127.0.0.1` behind a reverse proxy, or explicit `0.0.0.0`
  with equivalent network controls.
- Hosted regex posture is enforced:
  list-scoped, anchored, metadata-only regexes succeed; unsafe
  full-corpus or patch/prose regex shapes reject fast with
  `hosted_restriction`.

## 3. Metrics and overload visibility

- `/metrics` is reachable from the monitoring path.
- `kernel_lore_mcp_requests_total`,
  `kernel_lore_mcp_request_latency_seconds`,
  `kernel_lore_mcp_tool_calls_total`,
  `kernel_lore_mcp_tool_latency_seconds`,
  `kernel_lore_mcp_tool_runtime_seconds`,
  `kernel_lore_mcp_tool_queue_wait_seconds`, and
  `kernel_lore_mcp_tool_inflight` are all present.
- A synthetic overload run records non-`ok` statuses, including
  `rate_limited`, in `/metrics`.

## 4. Repeatable hosted-load gate

Run the adversarial harness against the exact binary + config you
intend to expose:

```sh
./.venv/bin/python scripts/bench/bench_hosted_adversarial.py \
    --json-out /tmp/klmcp-hosted-load.json
```

Pass criteria:

- Cheap flood completes without `rate_limited`.
- Moderate and expensive saturation both emit `rate_limited`.
- `/status` remains responsive while those tool classes saturate.
- The JSON report shows both client-observed latency and server-side
  histogram summaries so we can distinguish queueing from tool-body
  work.

## 5. Operator-log readability

- Hosted default logs are readable without hand-filtering.
- Third-party INFO spam is absent by default.
- Startup logs state:
  version, mode, transport, bind/port, data dir, and slow-path
  profiling thresholds.
- Slow requests, slow tool runs, delayed admission, and timeouts emit
  structured warning/info lines with enough context to debug the next
  incident.

## 6. Surface sanity

- `scripts/agentic_smoke.sh local` passes against the target data dir.
- HTTP smoke passes:
  `uv run pytest tests/python/test_http_transport.py -q`
- stdio smoke passes:
  `uv run pytest tests/python/test_stdio_subprocess.py -q`

## 7. Release proof

- The tagged release version matches:
  `pyproject.toml`, `Cargo.toml`, `src/kernel_lore_mcp/__init__.py`.
- `CHANGELOG.md` contains a dated section for the exact version.
- A clean install from PyPI succeeds in a throwaway venv and both
  `kernel-lore-mcp --help` and `kernel-lore-sync --help` work.
