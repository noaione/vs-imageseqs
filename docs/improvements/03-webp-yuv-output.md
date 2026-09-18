# 03 — yuv output for lossy webp

- status: implemented, together with [04](04-webp-decoder.md)
- touches: `src/formats/webp.rs`, `src/decoder.rs`, `src/pixel.rs`,
  `src/source.rs`, `src/color.rs`
- result: the sandbox webp set reads in 3.39 s instead of 5.66 s at the
  default `prefetch`, and 11.94 s instead of 19.97 s with `prefetch=0`, which
  topples bestsource on the total
- risk taken: the output format now depends on the input, and a folder that
  mixes codings needs `mismatch=True` where it used to read as one `RGB24`
  sequence

## problem

lossy webp stores yuv 4:2:0. the plugin decoded it to rgb, wrote `RGB24`, and
the rest of the graph in a typical manga or video pipeline converted it back to
yuv. that cost three things per frame:

- a colour conversion inside the decoder that nothing asked for,
- 36 MB per frame instead of 18 MB, for every allocation, copy and page fault,
- half as many frames in the lookahead budget, so fewer useful decodes.

bestsource kept its decoder's format (`YUV420P8` on this set) and was 4.5x
faster per frame than imgseqs at the default settings — see
[BENCH.md](../BENCH.md).

`image-webp` cannot help here: `decode_frame` and `read_image` are rgb(a) only,
and its public surface has no yuv entry point (checked against 0.2.4). libwebp
does, through `WebPDecodeYUVInto`, which is why [04](04-webp-decoder.md) landed
first.

## change, as implemented

**no argument was added.** decoding to yuv is what the plugin does for a lossy
webp file with no alpha channel, whatever the caller passes:

- `output_format` in `src/formats/webp.rs` reads the container's chunk headers
  and returns `YUV420P8` for a file that is
  - lossy (`VP8` chunk, not `VP8L`),
  - without alpha (no `VP8X` alpha flag, no `ALPH` chunk),
  - and probed as `Rgb8`.
- everything else keeps the interleaved layout the `image` path produced:
  lossless webp (rgb by definition), any file with an alpha channel (its alpha
  plane is read from the interleaved buffer), a grayscale lossy webp, a webp
  with no `.webp` extension, an animated container, and a container the walk
  cannot read in full.
- `Pixels` in `src/decoder.rs` is either `Interleaved { color_type, buffer }`
  or `Planar(Vec<Vec<u8>>)`, so `src/source.rs` matches on the decode result
  rather than on the format.
- `write_decoded_planes` copies the decoder's planes into the frame; the
  decoder's chroma is `ceil(size / 2)` while a VapourSynth plane is
  `size / 2`, so only the frame's own rows and columns are copied.
- `ReadAlpha` on a yuv frame fills an opaque gray alpha plane (`255`), the same
  value a source without an alpha channel gets.

the decision to skip the opt-in argument is deliberate: the plugin measures
whether the caller wants rgb (see the bytes and conversion points above) and
every graph that needs rgb from a webp can say so once with a resize, while a
graph that wants yuv no longer pays for a round trip it did not ask for. a
lossy webp file is a yuv file; handing out yuv is the honest output.

### matrix and range

`src/color.rs` tags a yuv frame `_Matrix = 5` (`bt470bg`) and `_Range = 0`
(limited), and leaves rgb and gray frames on `_Matrix = RGB` / `_Range = full`
as before. that is not a heuristic:

- vp8 defines the bt.601 coefficients for the limited range, and libwebp's own
  yuv to rgb conversion uses exactly that pair, so the tags describe the planes
  the plugin hands out.
- ffmpeg's webp decoder reports the same pair. bestsource, which wraps it,
  prints `_Matrix=5`, `_Range=0` for these files, and its planes are
  byte-identical to the plugin's (below).

ffmpeg's *transcode* heuristic (bt.601 below 720p, bt.709 above) is about
guessing a matrix for a stream that does not say, which is not this case.

### why the plugin does not convert

a graph that wants rgb can convert these planes itself, and that is cheaper than
any of the three ways the plugin could hand out rgb, on a 3672x5274 page:

| where the conversion happens | shape of the kernel | cost per frame | pool holds |
| --- | --- | --- | --- |
| libwebp, inside the decoder (the pre-03 path) | hand written simd in libwebp | +110 ms (185 + 8 against 265 + 38) | 55.4 MiB |
| `zimg` in the graph, `Bicubic` to `RGB24` | hand written integer simd | +29 ms serial, +10 ms at the default lookahead | 27.7 MiB |
| `zimg` in the graph, `Point` to `RGB24` | hand written integer simd | +22 ms | 27.7 MiB |
| our own code, scalar rust, nearest chroma | `u8` integer, auto vectorized | +55 ms | 27.7 MiB |
| our own code, scalar rust, bilinear chroma | `u8` integer, auto vectorized | +131 ms | 27.7 MiB |
| `zenyuv` 0.1.3, nearest chroma | `f32` per pixel, dispatched | +182 ms | 27.7 MiB |
| `zenyuv` 0.1.3, bilinear chroma | `f32` per pixel, dispatched | +371 ms | 27.7 MiB |

all rows are measured with `target/bench/yuvspeed`, probes that run the
conversion in the shape a frame write would need it (interleaved destination,
then the planar deinterleave into the frame).

[`zenyuv`](https://crates.io/crates/zenyuv) was tried because its name and
keywords promise simd and because it is licenced MIT OR Apache-2.0, so vendoring
it would be fine. it loses by 2.5x to the plain scalar loop. two reasons, both
worth knowing before the name sells it again:

- 0.1.3 keeps `mod decode;` private. using the decode at all means vendoring
  `decode.rs`, `decode_generic.rs` and `avx2_decode.rs` (~620 lines) or waiting
  for a release that exports them, and the numbers above are from a local copy
  with `pub mod decode` patched in.
- the simd in that copy is nominal. the only hand written avx2 kernel is
  `yuv444_to_rgb_avx2`, and nothing calls it (the comment above the dispatch
  says its `mulhrs_epi16` overflows when `y_coeff >= 1.0`, which is the normal
  case). every 4:2:0, 4:2:2 and 4:0:0 call goes to `decode_generic`, which is
  per pixel `f32` arithmetic. `cargo asm` is not needed to see it: `f32` with
  four bilinear chroma taps per pixel costs 18 ns/px where integer nearest costs
  2.8 ns/px.

so simd alone is not what makes `zimg` fast here. what makes it fast is being a
hand written integer kernel for exactly this conversion, which is why our scalar
loop is 2.5x off its `Point` and 4.5x off its `Bicubic` rather than 10x off. to
close that gap the plugin would need intrinsics or a simd dependency (`libyuv`
is the obvious one, and it would have to be added to the native licence set), and
the conversion would still run on the requesting thread inside `get_frame`, where
`zimg` runs in the graph and can overlap with the pool. so the plugin decodes to
yuv, and the graph converts if it needs rgb.

## results

sandbox webp set, 35 files, `tests/bench-imgseqs-vs-bestsource.vpy --reps 3
--extra --prefetch 16`, best pass of three. the middle row is
`target/bench/vs_imageseqs-rgb.dll`, a build of the libwebp decoder before this
change, so the two rows separate the decoder from the format:

| build | frames | `prefetch=0` | `prefetch=16` | bestsource |
| --- | --- | --- | --- | --- |
| `image-webp`, `RGB24` (before [04](04-webp-decoder.md)) | 5.66 s | 19.97 s | 4.12 s | 2.15 s |
| libwebp, `RGB24` (04 only) | 4.24 s | 15.60 s | 2.62 s | 1.78 s |
| libwebp, `YUV420P8` (04 + 03) | 3.39 s | 11.94 s | 1.87 s | 1.88 s |

the third row is one recorded run, `target/bench/sandbox-webp-yuv.txt`, so its
bestsource column is the 1.88 s measured beside it; the middle row is another
run of the same script with `--plugin target/bench/vs_imageseqs-rgb.dll`, where
bestsource came in at 1.78 s. the first row is the run recorded in
[BENCH.md](../BENCH.md) before either change. bestsource varies a few percent
between runs on this machine, which is why the verdict below is quoted from the
third row's own numbers: the plugin is now 1.14x *ahead* of bestsource
including the open, where it was 1.29x behind.

the three pipelines at `prefetch=0`, eight files of 3672x5274, so the frames
are the same size in every row:

| pipeline | wall | cpu |
| --- | --- | --- |
| `RGB24` from libwebp (04 only) | 411-434 ms/frame | 410-434 ms/frame |
| `YUV420P8` (this change) | 309-310 ms/frame | 307-309 ms/frame |
| `YUV420P8` + `Bicubic` to `RGB24` | 334-352 ms/frame | 332-344 ms/frame |

the second row is 1.33-1.40x faster than the first, and a graph that still
wants `RGB24` is 1.23x faster through the yuv path plus a resize than through
the decoder's own conversion. manga set (2903x4128, eight files, `prefetch=0`):
180-192 ms/frame before, 124-127 ms/frame after.

lookahead depth on the first 16 sandbox files, wall and process CPU per
delivered frame, with the value [BENCH.md](../BENCH.md) recorded before this
change in brackets:

| prefetch | wall | cpu | cores busy |
| --- | --- | --- | --- |
| 0 | 316.2 ms (571.3) | 312.5 ms (562.5) | 0.99 (0.98) |
| 2 | 160.7 ms (263.0) | 331.1 ms (576.2) | 2.06 (2.19) |
| 4 | 91.2 ms (160.7) | 351.6 ms (648.4) | 3.85 (4.04) |
| 8 | 65.7 ms (124.0) | 343.8 ms (758.8) | 5.23 (6.12) |
| 16 | 54.2 ms (145.6) | 419.9 ms (806.6) | 7.75 (5.54) |

half the bytes per frame also halves what the budget holds, so a deep
`prefetch` no longer turns around: 16 workers are 5.8x the serial row where
they used to be 3.9x, and cpu per delivered frame is 1.34x one serial frame
against 1.43x.

## validation

- **planes**: for every file checked, all three planes are byte-identical to
  bestsource/ffmpeg (`max 0`, mean delta `0.00`). this holds for the odd sized
  `3312x4717` file too, once ffmpeg's crop of the last row is accounted for.
- **round trip**: the yuv planes converted back with `Bicubic` land on the
  `image-webp` `RGB24` output with a mean absolute difference of `0.00` per
  channel and a per-channel maximum of 17-54 (libwebp's own chroma upsampling
  against zimg's bicubic, which differ at edges).
- **format**: `sandbox/webp` is 35/35 `YUV420P8`,
  `I:\Manga\KamiKatsu\source\v06` is 163/163, and png/jpeg/jxl/heic pages are
  unchanged.
- **properties**: `YUV420P8 matrix=5 range=0`; lossless/alpha webp and every
  other format `matrix=0 range=1`; alpha clips are `Gray8` on a yuv frame, have
  no `_Matrix`, and are filled with `255`.
- **ogsov**: `target/bench/ogsov-verdicts.py` runs the webp set through
  `GPUUpload` and `ogsov.AnalyzeVk` from the plugin's yuv frames and from a
  pre-03 `RGB24` build of the plugin. `OGSOVIsColor` is the same for all 35
  frames on both routes, p000 to p002 colour and the rest not, and the plane
  means differ by at most `0.07` of a level (`0.006` on average, worst frame the
  odd page and its restored border).
- `cargo test --locked` (45 tests, including plane sizes, the container walk
  and the yuv to rgb reconstruction), `cargo clippy -D warnings`,
  `tests/readalpha.vpy`.

## caveats

- **odd sizes.** a VapourSynth plane is `floor(size / 2)` where the decoder's
  is `ceil(size / 2)`, so for a 3312x4717 or 2903x4128 file the last chroma row
  and column are dropped. that is the frame layout, not a decode choice, and
  the same files are the reason `std.BlankClip` refuses an odd width for a
  subsampled format and `resize` to rgb refuses outright (`Resize error 1027:
  image dimensions must be divisible by subsampling factor`, for every matrix
  argument spelled out or omitted, and whether the clip is variable or not).
  the way through is to crop the odd row and column, convert, and restore the
  size with an `RGB24` border, which is not bound by the subsampling rule:
  `Crop` → `resize` → `AddBorders`. that costs what the conversion costs, 125 to
  153 ms per frame on a 2903x4128 page at `prefetch=0` and 26 to 49 ms at
  `prefetch=8`. `std.Crop` and `std.AddBorders` both refuse a variable clip
  (`Crop: constant format and dimensions needed`, `AddBorders: input needs to be
  constant format`), and `mismatch` makes the clip variable, so a folder that
  mixes formats or sizes has to be grouped in python by format *and* size, one
  graph per group. `resize` itself is happy with a variable clip, so a mixed
  folder converts frame by frame with the one line conversion and only stops on
  a `4:2:0` frame whose dimension is odd; `std.CropAbs` (variable input, pinned
  size) and `std.FrameEval` (a chain per frame) are the two in graph ways past
  that frame. the added border is black, so the dropped row and column come back
  black unless the graph stacks the last real column and row back on instead. an
  opt-in argument that asks the plugin for `RGB24` again was considered and
  left out, see the change section above.
- **the format depends on the input**, so a folder that mixes a lossy webp
  with a lossless or alpha one is no longer one constant `RGB24` sequence: the
  sequence check reports `frame 1 ('b-lossless.webp') has 32x32 RGB24, expected
  frame 0 ('a-lossy.webp') to be 32x32 YUV420P8`, and such a folder needs
  `mismatch=True`, which yields a variable format clip with
  `['YUV420P8', 'RGB24', 'RGB24']` frames. converting that variable clip is not
  a problem in itself: one `resize` returns `RGB24` frames at the pages' own
  sizes, and the `470bg`/limited arguments do nothing to the `RGB24` and `Gray8`
  frames (measured means identical to converting each file on its own, and
  `Gray8` 128 comes back as 128 in all three planes, so no level shift either).
- **chroma is not reversible.** the yuv path cannot be turned back into the rgb
  frame the old path produced byte for byte, so the two are compared by
  difference and means rather than by equality.

## acceptance (met)

- webp set: median frame and total roughly halve; `frame.format.name` is
  `YUV420P8`; `_Matrix`/`_Range` are present and match the documented choice.
- jpeg, png, jxl, avif and heic sets: unchanged, they never reach this code.
- a decoded frame matches ffmpeg's yuv output for the same file, byte for byte
  rather than within rounding.
