# zstd 0.13

Rust-specific (no Python parallel).

Pinned: `zstd = "0.13"`. Bindings to the C `libzstd`. Owns the
compressed raw store (`src/store.rs`) — the source of truth for
all message bodies, source of the per-candidate confirmation
step in the trigram tier.

See `../../../indexing/compressed-store.md` for the tier-level
spec; this doc covers the Rust idioms.

---

## Why zstd with a trained dictionary

Lore messages are short (often < 4 KB) and highly repetitive
within a list (subject prefixes, signature blocks, reviewer
trailers). Standard zstd on each message alone performs poorly;
the internal dictionary has no history.

zstd's dictionary training mode builds a 64 KB dictionary from
a 100 MB sample of a corpus; every subsequent compression
references that dictionary for back-references. Expected ratios
on our corpus (from `docs/indexing/compressed-store.md`):

- Headers: 15-20×
- Prose: 4-6×
- Patches: 8-12×

Without the dict, those drop to roughly 3×, 2×, 5× — enough to
matter at 25M messages.

**Per-list dicts**, not a global dict: each list's conventions
(reviewer style, subject prefixes) differ. A global dict
trains to the mean and underperforms per-list.

---

## Training a dictionary

```rust
use zstd::dict::from_samples;

fn train_dict(sample_iter: impl Iterator<Item = Vec<u8>>)
    -> crate::Result<Vec<u8>>
{
    // Collect up to 100 MB of samples.
    let mut samples: Vec<Vec<u8>> = Vec::new();
    let mut bytes = 0usize;
    for s in sample_iter {
        bytes += s.len();
        samples.push(s);
        if bytes >= 100 * 1024 * 1024 { break; }
    }

    let dict = from_samples(&samples, 64 * 1024)    // 64 KB dict
        .map_err(|e| crate::Error::State(format!("zstd dict train: {e}")))?;

    Ok(dict)
}
```

Points:

- **Sample target: 100 MB**. More ≠ better; zstd's trainer has
  diminishing returns past that.
- **Dict size: 64 KB**. Standard choice. Larger dicts help
  compression by < 1% and hurt read-side (dict is loaded per
  decoder instance).
- **Sample shape**: one `Vec<u8>` per message, not concatenated.
  The trainer needs document boundaries.
- **Store the dict** at `<data_dir>/store/<list>/dict.zstd`.
  Every decoder loads it.

One-time cost: ~2-3 minutes per list at first ingest. On
re-train (list corpus changes significantly), run offline and
atomic-rename. Compressed segments written with the old dict
are **not readable with a new dict** — re-ingest needed.
Versioning the dict in the filename
(`dict.zstd.v1`) would let us carry two, but today we treat
dict retrains as a full rebuild event (rare).

---

## Compressing a message with the dict

One zstd frame per message so random access = one decompress
call.

```rust
use zstd::stream::Encoder;

pub fn append_message(
    segment: &mut File,
    dict: &[u8],
    body: &[u8],
) -> crate::Result<(u64 /*offset*/, u32 /*length*/)> {
    let offset = segment.stream_position()?;

    let mut enc = Encoder::with_dictionary(&mut *segment, /*level=*/ 19, dict)?;
    // Long-range mode for big prose bodies.
    enc.long_distance_matching(true)?;
    // We want a self-contained frame so random-access decode can start here.
    // (Default behavior for `Encoder` — documented for readers.)
    enc.write_all(body)?;
    enc.finish()?;

    let end = segment.stream_position()?;
    let length = (end - offset) as u32;
    Ok((offset, length))
}
```

Rationale:

- **Level 19**. Patch/prose is write-once, read-many; spend
  CPU on compression.
- **`long_distance_matching(true)`** (aka `--long=27` in the
  CLI). Enables a 128 MB reference window — cross-message
  references within a segment. Reduces total size by ~15% on
  patch-heavy segments.
- **One frame per message.** Zstd supports frames starting at
  any offset; decoders don't need the preceding data.
- **Return `(offset, length)`** for the metadata tier's
  `body_offset` / `body_length` columns.

Segment roll: at 1 GB, seal the current segment (fsync), open
segment N+1. Roll is triggered by the writer, not by zstd.

---

## Random-access decompression — `(offset, length)`

Given `(offset, length)`, decompress exactly one message:

```rust
use zstd::stream::Decoder;
use std::io::{Read, Seek, SeekFrom};

pub fn read_message(
    segment: &File,
    dict: &[u8],
    offset: u64,
    length: u32,
) -> crate::Result<Vec<u8>> {
    // We take a fresh Seekable view; callers keep File handles pooled.
    let mut f = segment;
    f.seek(SeekFrom::Start(offset))?;
    let reader = std::io::Read::take(f, length as u64);

    let mut dec = Decoder::with_dictionary(reader, dict)?;
    let mut out = Vec::with_capacity(/* rough guess */ length as usize * 8);
    dec.read_to_end(&mut out)?;
    Ok(out)
}
```

Alternative for the trigram confirm loop — reuse a `Vec<u8>`
across calls to avoid per-candidate allocation:

```rust
pub fn read_message_into(
    segment: &File, dict: &[u8],
    offset: u64, length: u32,
    buf: &mut Vec<u8>,
) -> crate::Result<()> {
    buf.clear();
    let mut f = segment;
    f.seek(SeekFrom::Start(offset))?;
    let reader = std::io::Read::take(f, length as u64);
    let mut dec = Decoder::with_dictionary(reader, dict)?;
    dec.read_to_end(buf)?;
    Ok(())
}
```

`Decoder::with_dictionary` reparses the dict on every call.
For hot paths (trigram confirmation), reuse a
`zstd::dict::DecoderDictionary`:

```rust
use zstd::dict::DecoderDictionary;

let ddict = DecoderDictionary::copy(dict);   // parse once
// Per candidate:
let mut dec = Decoder::with_prepared_dictionary(reader, &ddict)?;
dec.read_to_end(buf)?;
```

`with_prepared_dictionary` skips the re-parse. Measurable on
the confirmation loop — our `TRIGRAM_CONFIRM_LIMIT` of 4096 per
query means 4096 decodes; per-call dict-parse cost adds up.

---

## File-handle pooling

Segments live as separate `segment-NNNNN.zst` files. Querying
decompresses from one segment at a time (the metadata tier tells
us which `segment_id`). Open-and-close on every call is
expensive; pool file handles:

```rust
use std::sync::Mutex;
use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;

pub struct SegmentPool {
    // (list, segment_id) -> File
    open: Mutex<HashMap<(String, u32), File>>,
    base_dir: PathBuf,
}

impl SegmentPool {
    pub fn get(&self, list: &str, segment_id: u32) -> crate::Result<File> {
        let mut m = self.open.lock().unwrap();
        if let Some(f) = m.get(&(list.to_owned(), segment_id)) {
            return Ok(f.try_clone()?);
        }
        let path = self.base_dir
            .join(list).join(format!("segment-{segment_id:05}.zst"));
        let f = File::open(path)?;
        m.insert((list.to_owned(), segment_id), f.try_clone()?);
        Ok(f)
    }
}
```

`try_clone` gives each caller a handle with its own seek
position — important because rayon tasks doing concurrent
reads can't share a single cursor.

---

## Write path — the append-only contract

Writes respect the compressed-store's durability contract
(see `../../../indexing/compressed-store.md`):

1. Append frame to current segment.
2. Update in-memory `(message_id → segment, offset, length)`
   pending map.
3. On segment roll or session end: **fsync the segment**.
4. **Only after fsync** write the `index.parquet` row that
   references the new offsets.
5. Atomic-rename the index Parquet into place.

This ordering ensures: if we crash between step 3 and step 5,
the segment is durable but unreferenced — reindex can pick it
up. Crash between 1 and 3 means the partial frame is discarded
(zstd decoders refuse to parse an incomplete frame).

Never fsync after every message — latency kills throughput.
Session-end or segment-roll is the right granularity.

---

## Features we don't use

`zstd` 0.13 supports several features we skip:

| Feature | Why not |
|---|---|
| Streaming multi-frame compression | We want one frame per message. |
| Training with `cover` algorithm variants | Default trainer is fine. |
| `bindgen` feature for custom libzstd | Vendored `libzstd` is fine; matches Python zstd bindings. |
| Per-thread dictionaries (advanced API) | We cache one `DecoderDictionary` per list. |
| `experimental` feature | Not ABI-stable. |

---

## 0.13 notes

- `Encoder::with_dictionary(writer, level, &[u8])` is the
  current stable signature.
- `long_distance_matching(bool)` was stabilized in 0.12; was
  an experimental parameter earlier.
- `DecoderDictionary::copy(&[u8])` vs `::new_ref(&[u8])`: the
  first owns the dict; the second borrows. Use `copy` for the
  long-lived pool.
- Compression levels: 1-22 standard, 1-19 classical, 20-22
  "ultra". We use 19.

---

## Don't-do list

| Anti-pattern | Why |
|---|---|
| Concatenating all messages into one frame | Loses random access. |
| Same dict for all lists | 20-30% worse ratio. |
| Re-parsing the dict per call | CPU sink on the confirm loop. |
| Writing without fsync before updating the index | Crash hole. |
| Treating segments as mutable | Append-only, seal-after-roll. |
| Trusting `length` as the decompressed-size cap | `length` is compressed bytes; decompressed can be 10× larger. Bound with a sanity `MAX_MESSAGE_SIZE` (currently 16 MB). |

---

## Checklist for a store change

1. Per-list dict trained, stored at
   `<data_dir>/store/<list>/dict.zstd`.
2. One frame per message (encoder finished between messages).
3. fsync before index-Parquet references new offsets.
4. `DecoderDictionary` cached per list for the confirm hot
   path.
5. `SegmentPool` used for read-side file handles.
6. Max message-size sanity check on decompression.

See also:
- `../../../indexing/compressed-store.md` — tier spec, layout.
- `../../../indexing/trigram-tier.md` — confirm-loop caller.
- `../design/data-structures.md` — `Bytes` for zero-copy splits
  post-decompress.
- `arrow-parquet.md` — `index.parquet` side of the store.
