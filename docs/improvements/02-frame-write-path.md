# 02 — frame write path

- status: implemented, 2b only; 2a is dropped
- touches: `src/clip.rs` (new), `src/prefetch.rs`, `src/source.rs`,
  `src/decoder.rs`, `src/pixel.rs`
- expected: a few ms per frame from 2a, up to ~1.6x on the webp set from 2b
- outcome: 2b, the frame build moved into the lookahead workers, so the
  requesting thread's floor is gone. 2a is dropped as unmeasurable. the 132 file
  jpeg corpus goes 1.28 s → 0.81 s at `prefetch=16` (bestsource 2.02 s), the 32
  page png set 20.5 → 18.4 ms/frame at `prefetch=4`, `prefetch=0` is unchanged,
  and every plane of the parity dump is byte identical
- risk: 2b needed a core handle with an extended lifetime, `SharedCore` in
  `src/clip.rs`

## problem

One webp frame used to touch three 36 MB buffers and copy the pixels twice, and
the second copy ran on the thread that asked for the frame, which made it a hard
serial floor. this is the shape the plan was written against, before
[03](03-webp-yuv-output.md) and [04](04-webp-decoder.md); the shape survived
them, the sizes did not.

```text
image-webp canvas   ->   our zeroed pixels   ->   VapourSynth frame
 (decoder internal)      decoder::decode          clip_frame / write_planar
```

`decoder::decode` allocates `vec![0; size]` and passes it to
`ImageDecoder::read_image`. That looks like a single fill, but the decoder
behind `image`'s `webp` feature (`image-webp` 0.2.4) decodes into its own canvas
first and then does `buf.copy_from_slice(canvas)`. It also exposes no accessor
for that canvas, so the copy is not avoidable while `image-webp` is the decoder.
[04](04-webp-decoder.md) replaced that decoder with libwebp, which writes its
planes straight out of the decode, so the copy that is left is a row copy between
two planar buffers.

The zeroing is ours: `read_image` overwrites every byte, so the zero fill is
paid for nothing. `alloc_zeroed` only maps pages, which is why the debug log
shows `buffer≈0.00 ms` — the cost of writing those pages first shows up inside
`read`, together with the decode. the debug log is also what undid this
paragraph: `buffer` stays at 0.01-0.04 ms per frame on 55 MiB frames too, so
there is no zero fill to remove that is worth an unsafe block.

## evidence

`debug=True`, `prefetch=0`, first frames of each set:

| set | decode (`read`) | copy (`convert`) | total |
| --- | --- | --- | --- |
| webp 2903x4128 | 265 ms | 48 ms | 314 ms |
| jxl 1500x2500 | 85 ms | 13 ms | 98 ms |
| jpeg 1404x2000 | 18 ms | 12 ms | 30 ms |
| png 1404x2000 | 9 ms | 3 ms | 12 ms |

the copy is serial. the jpeg curve in [BENCH.md](../BENCH.md) flattens at
~10 ms/frame from `prefetch=2` onwards with cpu at 23-28 ms per frame: the pool
is not the limit there, the copy and the per-frame bookkeeping are. on the webp
set the same floor is ~53 ms/frame, which is why 4 workers (88.8 ms) cannot be
improved much by adding more of them without also lowering the floor.

re-measured on the code just before this change, `prefetch=0`, the copy is still
on the requesting thread, just smaller:

| set | decode (`read`) | copy (`convert`) | total |
| --- | --- | --- | --- |
| jpeg 1404x2000 | 12 ms | 5.5 ms | 17.5 ms |
| png 3672x5274 | 19 ms | 7.3 ms | 27.8 ms |
| jxl 1500x2500 | 85 ms | 13 ms | 98 ms |

webp is missing from that table because [04](04-webp-decoder.md) and
[03](03-webp-yuv-output.md) replaced its decoder and its frame format: libwebp
hands out planar yuv, so its copy is the cheapest of the four. the per stage
table in [BENCH.md](../BENCH.md) has the sandbox rows.

## 2a — stop zeroing the decode buffer

**dropped, and not because it is wrong.** the plan expected a few ms per frame
from removing a 36 MB `alloc_zeroed` and its page faults. the page faults are
not in the allocation: `alloc_zeroed` only maps the pages, `buffer` measures
0.01-0.04 ms per frame at 18.5 MiB as well as at 55.4 MiB, and the cost of first
touching those pages is inside `read`, which is 64% to 68% of the per frame total
in the serial rows of the evidence table above. a few percent of that stage is not
measurable, and the price is an unsafe block around a third party decoder call:
`read_image`'s contract is prose ("writes the pixel data of the image into it",
with the slice required to be exactly `total_bytes()`), it is not enforced
anywhere, and it would have to hold for every decoder behind every route we
have, including the ones in `src/formats/`.

the reason the premise failed is worth recording: the buffer 2a wanted to stop
zeroing was the one `image-webp` copied its canvas into, and
[04](04-webp-decoder.md) removed that whole buffer by not using `image-webp`.
the unsafe-free alternative the plan mentions (a pool keyed by size, with the
allocation returned when the last consumer drops it) stays on the table, but the
numbers do not ask for it yet.

## 2b — get the copy off the requesting thread

**implemented, the structural way.** the goal is unchanged: the frame arrives
finished, and the requesting thread only hands it over. what the pool caches is a
built payload instead of a decoded image, so the worker that read the file is the
one that allocates the frame, writes the pixels into it, and attaches the source
properties.

```text
worker:      probe -> decode -> build the frame of every clip -> cache payload
requesting:  fetch -> hand a finished frame to VapourSynth
```

what changed:

- `src/prefetch.rs`: the pool is generic. `Payload` is what a finished decode
  hands out, `Prepare` turns one `DecodedImage` into a payload, and
  `Prefetcher<P: Prepare>` knows nothing else about what it holds. `Payload` is
  `Clone + Send + 'static` and deliberately not `Sync`: a payload is handed to
  one thread at a time, and a VapourSynth frame is `Send` but not `Sync`.
- `src/clip.rs` is new and owns the hand off: `Clip` and the clip lists of the
  two filters, `FrameBuilder` (the `Prepare` that builds the frame of every clip
  of a call), and `ClipFrames` (the `Payload` those frames are cached in, which
  clones the frame a caller asks for and remembers what the worker measured).
- `src/source.rs`: `Sequence` holds an `Arc<Prefetcher<FrameBuilder>>`, and
  `clip_frame` is a bounds check, a `fetch`, and the debug log. the log gained a
  `fetch=` field, the time the request waited for a worker; `decode`,
  `allocate`, `convert` and `properties` are now what the *worker* spent, so on a
  busy pool they add up to more than the requesting thread's total.
- `src/decoder.rs` and `src/pixel.rs`: `DecodeTimings` and `WriteTimings` are
  `Clone`/`Copy` because they travel with the payload, and three helpers that
  only existed to size a decode buffer (`ImageInfo::frame_bytes`, `Pixels::bytes`,
  `PixelFormat::decodes_to_planes`) are gone: the frame layout is
  `clip::expected_bytes`, which comes from the probe, and the `Pixels` enum
  carries the decode layout on its own.

the three obstacles the plan listed, and what they turned into:

- *`VideoFrame` borrows its core, so a frame cannot be cached as-is.* resolved
  with one `unsafe`, `SharedCore` in `src/clip.rs`: it holds a
  `CoreRef<'static>`, `FrameBuilder::new` is the only place that extends the
  lifetime, and the argument is written down next to both. a filter is freed
  before the core it was built with and the pool lives inside the filter that
  owns it, so the handle cannot dangle; the workers only use it for
  `new_video_frame` and `query_video_format`, which VapourSynth permits from any
  thread because it runs a filter's own `get_frame` wherever it likes.
- *frames would count towards the budget and are larger than a decoded buffer.*
  they do, and they are. the window is planned from the decoded size the probe
  records, so a padded payload cannot shrink it, and a payload is charged what it
  really holds (`stride × height` per plane), so the budget counts the pixels
  that exist.
- *with `mismatch` the frame format is per frame.* not a problem: the format is a
  pure function of the probed `ImageInfo` and the clip, so the worker needs no
  state that the requesting thread has.

## result

parity first, because a write path that produces different pixels is not a write
path. `target/bench/frame-parity.py` walks seven sets (six under `sandbox/` plus
the alpha fixtures) and dumps a blake2b of every plane of every frame, for `Read`
and for both clips of `ReadAlpha`: 75 lines. run against the release build from
before the change and the one after it, the dumps are identical except for the
header line that names the plugin.

`prefetch=0` is the control: same thread, same work, only the measurement moved.
32 jpeg pages of 1404x2000, two rounds, ms/frame:

| round | before | after |
| --- | --- | --- |
| 1 | 18.84 | 17.96 |
| 2 | 16.41 | 19.19 |

with the copy on the requesting thread the per frame floor is what that thread
does, so the change cannot help `prefetch=0`, and it does not hurt it either.

where it helps is a pool that is already faster than the thread that asks. 32
sandbox pages of 3672x5274 (`Gray8`, 18.5 MiB each), two rounds, ms/frame:

| prefetch | before | after |
| --- | --- | --- |
| 4 | 21.32, 19.76 | 18.09, 18.80 |
| 16 | 16.97, 16.12 | 12.43, 16.15 |

and the corpus this was written for, 132 jpeg pages of 1404x2000, wall clock for
the whole set (`--reps 1`, two rounds):

| round | default before | default after | `prefetch=16` before | after |
| --- | --- | --- | --- | --- |
| 1 | 1.177 s | 1.172 s | 1.030 s | 0.870 s |
| 2 | 1.234 s | 1.016 s | 1.522 s | 0.757 s |

bestsource needs 1.938 s, 2.104 s, 2.070 s and 1.967 s on that corpus in the same
four runs, so `prefetch=16` goes from 1.6x to 2.5x of it (mean 1.28 → 0.81 s
against 2.02 s) and the default from 1.67x to 1.85x.

the measurement that says the win is smaller than the plan hoped is the 8 file
jpeg set, where both rows are already pool bound:

| stage | before `prefetch=4` | after `prefetch=4` |
| --- | --- | --- |
| wait for the pool (`decode`, then `fetch`) | 1.93 ms | 8.87 ms |
| copy into the frame on the requesting thread (`convert`) | 6.94 ms | gone |
| total | 8.96 ms | 8.89 ms |

the worker side of that same run moved the other way: `read` 15.13 → 18.11 ms
and `convert` 6.94 → 15.59 ms per frame, because four workers writing planes at
once compete for the memory bandwidth the decoder wants. the write is memory
bound, so it does not scale with workers, and moving it into the pool moves the
floor into the pool. where the pool had room and the requesting thread was the
limit the same change is worth 5% to 35%; where the pool was already the limit it
is worth nothing, and costs nothing.

## acceptance

- 2a: `read` in the debug log drops by a few ms on webp and the total with it;
  `cargo test --locked` still passes. **dropped**: the page faults 2a was aiming
  at are inside `read`, `buffer` measures 0.01-0.04 ms per frame, and the fill
  promise is prose that every decoder behind every route would have to keep for
  us. `retries_failed_decodes` still covers a decode that fails partway through
  the buffer.
- 2b via libwebp: webp `prefetch=0` drops by the two removed copies, with the
  frame bytes unchanged. **not this route**: [04](04-webp-decoder.md) took
  libwebp for its planar output, which removed the canvas copy, and
  [03](03-webp-yuv-output.md) removed the conversion that was left. `prefetch=0`
  still pays the row copy into the frame, which is what a serial path is.
- 2b via frames in the pool: webp wall at `prefetch=6..8` approaches the decode
  throughput instead of staying at ~53 ms. **met in kind, not in the number**:
  the requesting thread's floor is gone, which is what the plan was for, and the
  132 file corpus went 1.28 s → 0.81 s at `prefetch=16`. the sandbox webp set was
  not re-measured for this plan.
- the frame bytes must match the current path exactly. **met**: the parity dump
  is identical, and `cargo test --locked` (45 tests), `cargo fmt --all --
  --check`, `cargo clippy --workspace --all-targets -- -D warnings` and
  `tests/readalpha.vpy` all pass.
