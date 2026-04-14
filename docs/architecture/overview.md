# Architecture — overview

One-paragraph version in [`../../README.md`](../../README.md). Full
version here.

## Goals

1. **LLM-native surface.** MCP is the primary API; REST is for
   everything else. Responses shape for an LLM caller first
   (structured blocks, citations, deterministic IDs) and a human
   second.
2. **Low latency, low cost.** Single-box deploy. Structured queries
   sub-100ms, BM25 sub-500ms at the p95. ~$100/mo steady-state.
3. **Operator honesty.** The server tells callers what it doesn't
   see (private security queues, vendor backports, syzbot pre-public)
   so they calibrate. Silence is worse than a caveat.
4. **Repeatability.** Indices are rebuildable from the compressed
   raw store without refetching lore.

## Non-goals

- Replacing `lei` for human users. `lei` is better at `lei`.
- Being a public-inbox mirror. We re-index a mirror; we don't serve
  `public-inbox-httpd`.
- Semantic / vector search. Maybe v2; not v1.
- Write operations of any kind.

## High-level flow

```
         lore.kernel.org (390 public-inbox git shards)
                         │
                         ▼
              grokmirror (10-min cron)
                         │
                         ▼
          local shards on gp3 (/var/lore-mirror)
                         │
                         ▼
  ┌──────────── Rust ingestor (gix + rayon) ────────────┐
  │                                                      │
  │  per commit in each shard:                           │
  │    read `m` blob (RFC822)                            │
  │    parse with mail-parser                            │
  │    strip quoted replies + signature                  │
  │    split prose / patch                               │
  │    extract touched files + functions from patch      │
  │    append to compressed store (zstd-dict per list)   │
  │    route fields to:                                  │
  │      ├─► metadata tier (Arrow → Parquet)             │
  │      ├─► trigram tier  (roaring + fst)   [patch]     │
  │      └─► BM25 tier     (tantivy)         [prose]     │
  │                                                      │
  └──────────────────────────────────────────────────────┘
                         │
                         ▼
       Query router (Rust, exposed to Python via PyO3)
                         │
           ┌─────────────┼─────────────┐
           ▼             ▼             ▼
        FastMCP     FastAPI REST    /status
     (streamable      (JSON)
       HTTP)
```

## Three-tier index

Why three indices and not one: see
[`three-tier-index.md`](./three-tier-index.md). Short version: a
mailing list archive has three mostly-disjoint query classes
(structured metadata, code/patch substring, prose BM25), each with
a different optimal data structure. One monolithic index either
over-serves or under-serves every class.

## Why Rust + Python

- **Rust** owns everything that touches 15–25M messages: shard
  walking, parsing, indexing, compression, query dispatch.
  Parallel-by-default via rayon; Send/Sync enforced by the type
  system; mmap'd indices stay cold-fast.
- **Python** owns the MCP/REST surface, config, auth, rate
  limiting, deploy glue. All the parts where developer velocity
  beats microseconds.
- **PyO3 0.28** is the seam. Every heavy Rust call releases the
  GIL; Python 3.14 free-threaded is forward-compatible (pending
  PEP 803 abi3t).

## What we explicitly rejected

See [`../../CLAUDE.md`](../../CLAUDE.md) "What NOT to use" section.
