# 51 — a colour page at fourteen or sixteen bits keeps its planes

- status: implemented, and the fixtures arrived on 2026-11-11. Eight pages,
  `heic-yuv{400,420,422,444}p{14,16}.heic`, are the ramp these arms were added
  for, and `tests/readalpha.vpy`'s `test_heic_depth_arms` reads all eight. Three
  of them exercise a `P14` arm end to end; the rest fall where the rules put
  them, which is the table below.
- touches: `src/pixel.rs` (`PixelFormat` and its six matches),
  `src/formats/heif.rs` (`yuv_format`, `color_space_of`), tests in both,
  `AGENTS.md`, `CHANGELOG.md`, `tests/fixtures/heic-yuv*p{14,16}.heic` (eight
  pages) and `tests/readalpha.vpy`
- expected: a heif page whose container states a 4:2:0, 4:2:2 or 4:4:4 matrix at
  fourteen or sixteen bits is handed out as its own yuv planes, at its own
  depth, instead of being converted to r,g,b first.
- risk: low, and entirely in the direction of "fewer conversions". The new arms
  are only reached by a file whose container states 14 or 16 bits, which no file
  this tree can build does — so a mistake here cannot change what any existing
  file decodes to, and the table test is what holds the mapping.

## problem

`heif.rs`'s `yuv_format` armed eight, ten and twelve bits at every chroma, and
sixteen bits at 4:4:4 alone. `src/pixel.rs`'s `PixelFormat` had no `Yuv420P16`,
`Yuv422P16` or any `P14` at all. So a colour heif page that states fourteen or
sixteen bits — legal in the container, and what a multi-depth x265 writes —
fell through to `None`, which means the reader asked `libheif` for r,g,b and
converted the picture. That is a conversion, a rounding, and a loss of the
`_Matrix`, `_Range` and `_ChromaLocation` a yuv frame carries, for a file that
already holds yuv planes.

VapourSynth names every one of these: `YUV420P14`, `YUV422P14`, `YUV444P14`,
`YUV420P16` and `YUV422P16` all exist in the core this plugin builds against,
and `query_format` asks the core for a format from family, depth and
subsampling, so the plugin only had to name the right one.

## evidence
**the fixtures, and the encoder that writes them.** `heif-enc --bit-depth 14`
answered `Unsupported bit depth: Bit depth not supported by x265` for the
`libx265-217.dll` that came with the tool, which is built for eight, ten and
twelve bits. The maintainer has since built a sixteen bit x265 (`encoders.txt`
in that directory reports `x265 HEVC encoder (4.3+1-e9b8812)`), and the eight
pages it wrote are in `tests/fixtures`: eight by six ramps that state bt.709
primaries and transfer at full range.

| page | the container states | the plugin hands out |
| --- | --- | --- |
| `heic-yuv400p14.heic` | monochrome, 14 bits | `Gray14` |
| `heic-yuv400p16.heic` | monochrome, 15 bits | `Gray15` |
| `heic-yuv420p14.heic` | yuv 4:2:0, 14 bits | `YUV420P14` |
| `heic-yuv422p14.heic` | yuv 4:2:2, 14 bits | `YUV422P14` |
| `heic-yuv444p14.heic` | yuv 4:4:4, 14 bits | `YUV444P14` |
| `heic-yuv420p16.heic` | yuv 4:2:0, 15 bits | `RGB45` |
| `heic-yuv422p16.heic` | yuv 4:2:2, 15 bits | `RGB45` |
| `heic-yuv444p16.heic` | yuv 4:4:4, 15 bits | `RGB45` |

Two things the table says that the names do not. The `p16` pages **state
fifteen bits**, which is libheif capping a page rather than the encoder:
HEVC can carry sixteen bits, but the `hvcC` box of the file format cannot
signal that depth, so libheif refuses to hand one over and states fifteen --
its README says so under the codec notes, and the pages show it twice: the
container says fifteen, and their samples reach 34976, past the 32767 a
fifteen bit frame holds. VapourSynth names no fifteen bit yuv format, so
those pages are handed out as the r,g,b the plugin builds, at the depth the
container states, which is the same rule that serves any depth yuv has no
name for. And the `P14` pages are the yuv frames these arms were for, with
`_Matrix` 1 beside them because the container states bt.709, which is the
half of the rule that makes a yuv
frame useful at all.

`tests/readalpha.vpy`'s `test_heic_depth_arms` reads every page: the format,
the 8x6 size, each plane's row length, that every sample fits the depth the
container states, the `_Primaries`/`_Transfer`/`_Matrix`/`_Range` codes, and an
alpha clip that is opaque at the frame's own maximum. It pins no sample values:
the samples are this encoder's conversion of the source ramp rather than a
formula, and the twelve bit page beside it already pins the decoder's values.
with `--bit-depth 14` and `--bit-depth 16` answers:

```
Could not encode HEIF/AVIF file: Encoder plugin generated an error:
Unsupported bit depth: Bit depth not supported by x265
```

`libx265-217.dll` is built for eight, ten and twelve bits, so the deepest colour
heif this machine can make is `heif-yuv420p12.heic`, which is already in
`tests/fixtures`. `avifenc` cannot help either: AV1 profiles stop at twelve bits,
so `avif.rs`'s own table correctly stops there too and is untouched by this
change.

What that leaves is a table, held by two tests:

- `pixel::tests::the_yuv_formats_name_their_depth_and_subsampling` walks every
  planar yuv format, requires its name to be `YUV<chroma>P<depth>`, and checks
  the depth, the subsampling, the plane count, the word size, the integer
  maximum and the gray alpha format it hands out.
- `heif::tests::the_subsampling_libheif_reports_maps_onto_a_format` now requires
  the six new arms and keeps `None` for the depths with no format of their own
  (nine and fifteen).

The rest of the acceptance is the existing one: `cargo test --locked` (378),
clippy, `tests/readalpha.vpy` and `tests/pngwrite.vpy` (1134 and 311 checks),
and `tests/readalpha.vpy`'s `test_heic_depth_arms` is what reads the eight
pages end to end.

## the change

`PixelFormat` gains `Yuv420P14`, `Yuv422P14`, `Yuv444P14`, `Yuv420P16` and
`Yuv422P16`; `alpha_format`, `color_family`, `bits_per_sample`, `sub_sampling`
and `name` each gain the arms, and `bytes_per_sample` and `plane_count` are
already wider than the new formats. `heif.rs` gains the six `yuv_format` arms and
the `color_space_of` arms that ask `libheif` for those chroma, so the planes are
handed over unconverted — which is the whole point of the request.

`src/convert.rs` needs nothing: it reads the frame's own `bits_per_sample` and
the writer quantises to the depth the frame names.

## what is left

- **sixteen bits cannot arrive through libheif at all.** `YUV420P16` and
  `YUV422P16` are reached only by a container that states sixteen bits, and
  libheif caps an HEVC page at fifteen because `hvcC` cannot signal sixteen
  (see the evidence above). So those two arms, and the `Gray16`/`Rgb16` a heic
  would map to, are reachable only through a decoder that reports sixteen --
  an AV1 container can state twelve at most, so nothing here does. The table
  tests are the whole of their evidence until one exists.
- **nine bits stays out.** VapourSynth names `YUV420P9`, but no encoder here
  writes a nine bit stream and no file in the corpus states one, so the arm that
  would serve it is guesswork rather than a mapping.
- **rgb at fourteen and sixteen already worked**, through `Rgb14`/`Rgb16`, which
  is why this gap only ever bit a colour page whose container states a matrix.
