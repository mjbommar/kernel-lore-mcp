"""Embedding-tier glue: text → L2-normalized f32 vector via fastembed.

The embedding model is decoupled from the rest of the project. Any
class with the shape:

    class Embedder(Protocol):
        model_name: str
        dim: int
        def embed(self, texts: list[str]) -> list[list[float]]: ...

works. Default is `FastembedEmbedder("BAAI/bge-small-en-v1.5")`,
which is ~30M params, CPU-fast, ~384-dim. Swap to a kernel-tuned
model later via env or constructor argument — no other code change.
"""

from __future__ import annotations

from typing import Protocol

# Default model. Small, fast, CPU-only, ~75 MB on disk.
DEFAULT_MODEL = "BAAI/bge-small-en-v1.5"
DEFAULT_DIM = 384


class Embedder(Protocol):
    model_name: str
    dim: int

    def embed(self, texts: list[str]) -> list[list[float]]: ...


class FastembedEmbedder:
    """Production embedder. Lazy-imports `fastembed` so the package
    works for users who don't need the embedding tier (e.g. CI on the
    Rust side only).
    """

    def __init__(self, model_name: str = DEFAULT_MODEL) -> None:
        self.model_name = model_name
        from fastembed import TextEmbedding

        self._impl = TextEmbedding(model_name=model_name)
        # fastembed's TextEmbedding doesn't expose dim directly until
        # we run it once; one no-op embed gets it cheaply.
        sample = next(iter(self._impl.embed(["dim probe"])))
        self.dim = int(sample.shape[0])

    def embed(self, texts: list[str]) -> list[list[float]]:
        if not texts:
            return []
        # fastembed already L2-normalizes bge-* outputs.
        return [vec.tolist() for vec in self._impl.embed(texts)]


def l2_normalize(vec: list[float]) -> list[float]:
    """Best-effort L2 normalization for any embedder that doesn't
    normalize already. Cheap; safe to call on already-normalized
    vectors (renormalization is a no-op).
    """
    s = sum(x * x for x in vec) ** 0.5
    if s < 1e-12:
        return vec
    return [x / s for x in vec]
