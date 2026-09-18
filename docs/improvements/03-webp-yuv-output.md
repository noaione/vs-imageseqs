# 03 — yuv output for lossy webp

- status: proposed, depends on [04](04-webp-decoder.md)
- touches: decoder path, `src/pixel.rs`, `src/source.rs`, `src/color.rs`
- expected: webp ~25-30 ms/frame instead of 88.8, i.e. ahead of bestsource on
  the total
- risk: medium, it changes what the plugin outputs for some inputs

## problem

lossy webp stores yuv 4:2:0. the plugin decodes it to rgb, writes `RGB24`, and
the rest of the graph in a typical manga or video pipeline converts it back to
yuv. that costs three things per frame:

- a colour conversion inside the decoder that nothing asked for,
- 36 MB per frame instead of 18 MB, for every allocation, copy and page fault,
- half as many frames in the 192 MiB budget, so fewer useful decodes.

bestsource keeps its decoder's format (`YUV420P8` on this set) and is 4.5x
faster per frame than imgseqs at the default settings — see
[BENCH.md](../BENCH.md).

`image-webp` cannot help here: `decode_frame` and `read_image` are rgb(a) only,
and its public surface has no yuv entry point (checked against 0.2.4). libwebp
does, through `WebPDecodeYUV`/`WebPDecodeYUVInto`, which is one more reason to
land [04](04-webp-decoder.md) first.

## change

an opt-in argument, name to be decided (`yuv`, `raw`, `native_format`), that:

- decodes lossy webp to yuv planes and writes `YUV420P8`,
- leaves everything else on the current path — png, jpeg, jxl, avif, heif,
  `Gray8` and `RGB16` webp and *lossless* webp all decode to rgb or gray in
  their decoders, and lossless webp is rgb by definition,
- sets `_Matrix` and `_Range` for the yuv output. the plugin only sets
  `_Matrix = RGB` for rgb output today, so that code path stays as it is.

the matrix is the part that needs a decision before any code:

- lossy webp does not record one. ffmpeg's heuristic (bt.601 below 720p,
  bt.709 above) is what bestsource inherits, so matching it keeps the two
  plugins comparable.
- alternatives: always bt.709, or expose `matrix`/`range` arguments and let the
  script say. for a plugin that already takes explicit arguments, exposing them
  with ffmpeg's default heuristic seems most honest, and it is a documentation
  question more than a code one.
- range: webp is generally full range, which is also what an image source
  should assume.

## expected

a 2903x4128 frame becomes 2903 * 4128 * 1.5 = 18 MB:

| effect | before | after |
| --- | --- | --- |
| copy into the frame | 48 ms | ~24 ms |
| frames per 192 MiB budget | 5.6 | 11 |
| decode | 265 ms with a yuv to rgb conversion | less, and no conversion |

with [01](01-lookahead-scheduling.md) and the budget question settled, the webp
set should land near 25-30 ms/frame. bestsource needs 15 ms/frame plus 5.95 s
of indexing on that set, so imgseqs would be ahead on the total even without a
faster decoder.

## risks

- the output format depends on the input, which is why this must be opt-in.
  graphs that expect `RGB24` need `yuv=False`, and the README has to say so.
- a wrong matrix shows as a colour shift. validation should compare planes
  against ffmpeg's decode of the same file, not against the rgb path.
- chroma subsampling is not reversible: yuv output cannot be turned back into
  the rgb frame the current path produces, so the two paths are not comparable
  byte for byte.
- `tests/readalpha.vpy` covers rgb and gray output. a yuv path needs its own
  checks (plane dimensions, subsampling, properties, and that `ReadAlpha` still
  behaves).

## acceptance

- webp set: median frame and total roughly halve; `frame.format.name` is
  `YUV420P8`; `_Matrix` and `_Range` are present and match the documented
  choice.
- jpeg, png and jxl sets: unchanged with the argument off; with it on, either
  ignored (documented) or rejected with a clear error.
- a decoded frame from the yuv path must match ffmpeg's yuv output for the same
  file within rounding, which is the check that the matrix assumption is right.
