# 51 — a colour page at fourteen or sixteen bits keeps its planes

- status: implemented, with no fixture on this machine. The arms are in and a
  unit test holds the table; the file that would exercise them end to end cannot
  be written by any encoder here, and the page says why rather than pretending
  otherwise.
- touches: `src/pixel.rs` (`PixelFormat` and its six matches),
  `src/formats/heif.rs` (`yuv_format`, `color_space_of`), tests in both,
  `AGENTS.md`, `CHANGELOG.md`
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

**there is no fixture, and this is what stops it.** `C:\Tools\heic\heif-enc.exe`
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

The rest of the acceptance is the existing one: `cargo test --locked`, clippy,
`tests/readalpha.vpy` (1034 checks) and `tests/pngwrite.vpy` (311 checks) all
pass, and nothing about them changed, because no fixture in the tree states a
depth this patch adds.

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

- **a fixture.** Any encoder built for sixteen bits writes one, and
  `tests/make-alpha-fixtures.py` is where the command belongs: its header already
  documents the `heif-enc` line for `heic-yuv420p12.heic`, so a `-b 14` pass over
  a sixteen bit source would slot beside it. Until then the arms are exercised by
  the table test alone, which is why they are documented as such.
- **nine bits stays out.** VapourSynth names `YUV420P9`, but no encoder here
  writes a nine bit stream and no file in the corpus states one, so the arm that
  would serve it is guesswork rather than a mapping.
- **rgb at fourteen and sixteen already worked**, through `Rgb14`/`Rgb16`, which
  is why this gap only ever bit a colour page whose container states a matrix.
