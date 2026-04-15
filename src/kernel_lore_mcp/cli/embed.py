"""kernel-lore-embed — bootstrap or rebuild the embedding tier.

Walks every metadata Parquet row, fetches the prose body via the
compressed store, embeds prose+subject via the configured Embedder,
then hands the (message_ids, vectors) batch to
`_core.build_embedding_index`. Idempotent — overwrites the existing
index atomically.

CPU-only by default. ~5 ms/message for bge-small at batch 64 on
Graviton; ~30k messages/min, ~6 hours for the full lore corpus.

Usage:
    KLMCP_DATA_DIR=/var/klmcp/data \\
    kernel-lore-embed --model BAAI/bge-small-en-v1.5 --batch 64
"""

from __future__ import annotations

import argparse
import logging
import os
import sys
import time
from pathlib import Path

import structlog


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="kernel-lore-embed")
    p.add_argument(
        "--data-dir",
        default=os.environ.get("KLMCP_DATA_DIR"),
        help="Project data root. Env: KLMCP_DATA_DIR.",
    )
    p.add_argument(
        "--model",
        default="BAAI/bge-small-en-v1.5",
        help="fastembed model name.",
    )
    p.add_argument(
        "--batch",
        type=int,
        default=64,
        help="Embedding batch size (per fastembed call).",
    )
    p.add_argument(
        "--limit",
        type=int,
        default=None,
        help="Cap rows for testing. None = all.",
    )
    p.add_argument(
        "--prose-max-chars",
        type=int,
        default=2000,
        help=(
            "Truncate prose body before embedding. bge-small caps at "
            "512 tokens; 2000 chars is a comfortable head + leaves "
            "room for the subject prefix."
        ),
    )
    p.add_argument(
        "--log-level",
        default=os.environ.get("KLMCP_LOG_LEVEL", "INFO"),
    )
    return p


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    if not args.data_dir:
        print("ERROR: --data-dir or KLMCP_DATA_DIR required", file=sys.stderr)
        return 2
    data_dir = Path(args.data_dir)
    if not data_dir.exists():
        print(f"ERROR: data_dir {data_dir} does not exist", file=sys.stderr)
        return 2

    logging.basicConfig(
        level=getattr(logging, args.log_level.upper(), logging.INFO),
        stream=sys.stderr,
        format="%(message)s",
    )
    structlog.configure(
        processors=[
            structlog.processors.add_log_level,
            structlog.processors.TimeStamper(fmt="iso", utc=True),
            structlog.processors.JSONRenderer(),
        ],
        logger_factory=structlog.PrintLoggerFactory(file=sys.stderr),
    )
    log = structlog.get_logger()

    from kernel_lore_mcp import _core
    from kernel_lore_mcp.embedding import FastembedEmbedder, l2_normalize

    log.info("embed.start", data_dir=str(data_dir), model=args.model, batch=args.batch)
    started = time.monotonic()

    embedder = FastembedEmbedder(model_name=args.model)
    log.info("embed.model_loaded", model=args.model, dim=embedder.dim)

    reader = _core.Reader(data_dir)
    rows = _scan_all(reader)
    if args.limit:
        rows = rows[: args.limit]
    log.info("embed.rows_collected", rows=len(rows))

    mids: list[str] = []
    texts: list[str] = []
    for row in rows:
        text = _row_to_text(row, reader, args.prose_max_chars)
        if not text:
            continue
        mids.append(row["message_id"])
        texts.append(text)

    log.info("embed.texts_prepared", embed_count=len(texts), skipped=len(rows) - len(texts))

    vectors: list[list[float]] = []
    for i in range(0, len(texts), args.batch):
        chunk = texts[i : i + args.batch]
        batch_vecs = embedder.embed(chunk)
        for v in batch_vecs:
            vectors.append(l2_normalize(v))
        if (i // args.batch) % 50 == 0:
            elapsed = time.monotonic() - started
            log.info(
                "embed.progress",
                done=i + len(chunk),
                total=len(texts),
                elapsed_secs=round(elapsed, 1),
            )

    meta = _core.build_embedding_index(
        data_dir=data_dir,
        model=args.model,
        dim=embedder.dim,
        message_ids=mids,
        vectors=vectors,
    )
    log.info(
        "embed.complete",
        rows=meta["count"],
        dim=meta["dim"],
        model=meta["model"],
        elapsed_secs=round(time.monotonic() - started, 1),
    )
    return 0


def _scan_all(reader) -> list[dict]:
    """Pull every metadata row by walking expand_citation('') ... no.

    The reader exposes specific queries; for "give me everything" we
    use eq() with a sentinel field that always passes is awkward.
    Cheap correct approach: walk all known message-ids via the
    metadata files directly. We don't have a public scan_all on the
    Python side, so we reconstruct it via lore_substr_subject('') —
    the empty substring matches every subject_raw.
    """
    # The empty needle would normally be invalid; substring scans
    # accept it (every string contains "" and substr_subject is
    # case-insensitive against subject_raw which is always non-null
    # for parsed messages). We use limit=10**9 to mean "everything".
    return reader.substr_subject("", None, None, 10**9)


def _row_to_text(row: dict, reader, prose_max_chars: int) -> str | None:
    """Compose the embedding input. Format:
        <subject_normalized | subject_raw>\n\n<prose body, truncated>
    Strips any patch payload (already split at ingest).
    """
    subject = row.get("subject_normalized") or row.get("subject_raw") or ""
    body = reader.fetch_body(row["message_id"])
    if body is None:
        return subject or None
    try:
        text = body.decode("utf-8")
    except UnicodeDecodeError:
        text = body.decode("latin-1", errors="replace")
    # Strip patch payload (anything after the first `^diff --git`).
    cut = text.find("\ndiff --git ")
    if cut >= 0:
        text = text[:cut]
    # Strip MIME headers preserved by the synthetic format — keep
    # only the parsed prose. Cheapest: drop everything before the
    # first blank line.
    sep = text.find("\n\n")
    prose = text[sep + 2 :] if sep >= 0 else text
    prose = prose.strip()
    if prose_max_chars > 0:
        prose = prose[:prose_max_chars]
    composed = (subject + "\n\n" + prose).strip()
    return composed or None


if __name__ == "__main__":
    sys.exit(main())
