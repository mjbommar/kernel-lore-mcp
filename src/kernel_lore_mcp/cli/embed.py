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
    builder = _core.EmbeddingBuilder(data_dir, args.model, embedder.dim)

    pending_texts: list[str] = []
    pending_mids: list[str] = []
    rows_seen = 0
    embedded = 0
    last_log = started
    limit = args.limit

    def _flush_pending() -> None:
        nonlocal embedded
        if not pending_texts:
            return
        batch_vecs = embedder.embed(pending_texts)
        normalized = [l2_normalize(v) for v in batch_vecs]
        builder.add_batch(pending_mids, normalized)
        embedded += len(pending_mids)
        pending_texts.clear()
        pending_mids.clear()

    def _on_batch(batch_rows: list[dict]) -> bool:
        nonlocal rows_seen, last_log
        for row in batch_rows:
            if limit is not None and rows_seen >= limit:
                _flush_pending()
                return False
            rows_seen += 1
            text = _row_to_text(row, reader, args.prose_max_chars)
            if not text:
                continue
            pending_mids.append(row["message_id"])
            pending_texts.append(text)
            if len(pending_texts) >= args.batch:
                _flush_pending()
        now = time.monotonic()
        if now - last_log >= 10.0:
            last_log = now
            log.info(
                "embed.progress",
                rows_seen=rows_seen,
                embedded=embedded,
                elapsed_secs=round(now - started, 1),
            )
        return True

    reader.scan_batches(_on_batch, batch_size=4096)
    _flush_pending()

    log.info(
        "embed.texts_prepared",
        rows_seen=rows_seen,
        embedded=embedded,
        skipped=rows_seen - embedded,
    )

    meta = builder.finalize()
    log.info(
        "embed.complete",
        rows=meta["count"],
        dim=meta["dim"],
        model=meta["model"],
        elapsed_secs=round(time.monotonic() - started, 1),
    )
    return 0


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
