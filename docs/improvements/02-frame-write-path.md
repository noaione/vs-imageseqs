# 02 — frame write path

- status: proposed
- touches: `src/decoder.rs`, `src/source.rs`, `src/pixel.rs`
- expected: a few ms per frame from 2a, up to ~1.6x on the webp set from 2b
- risk: 2a low, 2b needs care with VapourSynth frame lifetimes

## problem

One webp frame currently touches three 36 MB buffers and copies the pixels
twice, and the second copy runs on the thread that asked for the frame, which
makes it a hard serial floor.

```text
image-webp canvas   ->   our zeroed pixels   ->   VapourSynth frame
 (decoder internal)      decoder::decode          clip_frame / write_planar
```

`decoder::decode` allocates `vec![0; size]` and passes it to
`ImageDecoder::read_image`. That looks like a single fill, but the decoder
behind `image`'s `webp` feature (`image-webp` 0.2.4) decodes into its own canvas
first and then does `buf.copy_from_slice(canvas)`. It also exposes no accessor
for that canvas, so the copy is not avoidable while `image-webp` is the decoder.

The zeroing is ours: `read_image` overwrites every byte, so the zero fill is
paid for nothing. `alloc_zeroed` only maps pages, which is why the debug log
shows `buffer≈0.00 ms` — the cost of writing those pages first shows up inside
`read`, together with the decode.

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

## 2a — stop zeroing the decode buffer

`vec![0; size]` → an uninitialised buffer, for example `Vec::with_capacity`
plus `set_len` inside a small unsafe block. `read_image` either fills every byte
or returns an error, and on error the buffer is dropped, so no uninitialised
byte is ever observed.

expected: a few ms per frame on the large sets, unmeasurable on png. it also
removes a 36 MB `alloc_zeroed` plus its page faults per frame, which shows up
partly in `read` and partly in system time.

unsafe-free alternative: a buffer pool keyed by size. that keeps the zero fill
unless the pool is combined with 2a, but it does remove the map/unmap churn.
note that decoded images are handed to consumers as `Arc<DecodedImage>`, so a
pool needs the allocation back when the last consumer drops it — a `Drop` impl
that returns the buffer, or a pool handing out `Arc`s with a custom deleter.
more machinery than the unsafe block is worth unless the buffers are reused
across frames for other reasons.

## 2b — get the copy off the requesting thread

the goal: the frame arrives finished, and the requesting thread only sets
properties. two ways to get there.

**cheap, and blocked by `image-webp`:** decode straight into the frame plane.
`WebPDecodeRGBInto`/`WebPDecodeYUVInto` in libwebp take a destination buffer
*and a destination stride*, and a VapourSynth plane is a contiguous buffer with
a padded stride, so the decoder could write the frame directly and both extra
copies would disappear. libwebp is also what [04](04-webp-decoder.md) proposes,
so this is a real option, not a hypothetical one. `image-webp` cannot do it.

**structural, needed if the decode stays on a worker:** let workers produce the
finished frame, and let the requesting thread return it. obstacles:

- `core.new_video_frame` takes a `CoreRef`, and `VideoFrame` borrows the core,
  so a frame cannot be stored in the pool as-is. this needs an extended
  lifetime (unsafe) plus an argument for why it is sound: the prefetcher lives
  in the filter instance, and VapourSynth frees the filter before the core, so
  the pool cannot outlive the core. that has to be argued and tested, not
  assumed.
- frames would then count towards the memory budget of
  [01](01-lookahead-scheduling.md), since they are larger than the decoded
  buffers for rgb formats (a frame is padded, a decode buffer is not).
- with `mismatch` sequences the frame format is per frame, which is why the
  format is currently queried while handling the request.

**the half step that costs nothing:** keep the copy where it is and make sure
the decoders stay slower than it, so it overlaps instead of adding. from
`prefetch>=4` on the webp set that already holds. it caps the set at ~53 ms per
frame, which [03](03-webp-yuv-output.md) lowers by making the frames smaller.

## acceptance

- 2a: `read` in the debug log drops by a few ms on webp and the total with it;
  `cargo test --locked` still passes (`retries_failed_decodes` covers a decode
  that fails partway through the buffer).
- 2b via libwebp: webp `prefetch=0` total drops by the two removed copies
  (~48 ms + the canvas copy), and the frame bytes must still match the current
  path exactly.
- 2b via frames in the pool: webp wall at `prefetch=6..8` approaches the decode
  throughput (268 / 6 ≈ 45 ms, 268 / 8 ≈ 34 ms) instead of staying at ~53 ms.
