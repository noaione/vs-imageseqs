# 27 - direct still decoders and alternatives to image-rs

status: **selected, not implemented.** part of [26](26-remove-image-rs.md). Every
still format this plugin supports now has a chosen decoder; see
[the selection](#the-selection) below. The candidate research of 2026-10-03 was
reviewed on 2026-10-04 against the candidates' published sources and against a
run of each candidate over a purpose-built corpus, and every zune still
candidate was rejected for a defect reproducible from that corpus. Nothing has
been implemented, no dependency in `Cargo.toml` has changed, and the animation
and container fallbacks are [28](28-animation-container-decoders.md).

## the selection

One row per still format, decided on 2026-10-04. `probe` is what comes out of
the file without decoding it, `decode` is where the samples come from, and
`writes` is how the pixels reach a frame. The routes are the ones
[`src/decoder.rs`](../../src/decoder.rs) already has: a `RowStream` hands every
row to the frame it belongs in, `Pixels::Planar` and `Pixels::Interleaved`
buffer the picture first, and a probe-only module describes a file the decode
path then reads with its own reader.

| format | decoder | probe | decode | writes | state |
| --- | --- | --- | --- | --- | --- |
| png | `png 0.18.1` | `png 0.18.1`, plus the `cICP` chunk | `png 0.18.1` | `RowStream` where walkable, buffered otherwise | already implemented in [`formats/png.rs`](../../src/formats/png.rs); only the shared types remain |
| jpeg | `zune-jpeg 0.5.15` | one `decode_headers`, which answers dimensions, colour space, ICC and exif together | `zune-jpeg 0.5.15` `decode_into` | `Pixels::Interleaved`, one interleave to split | verified: all 35 files of `sandbox/jpeg` decode byte-identically through this path; the exif orientation needs a small parse of the `exif()` bytes |
| webp | libwebp, unchanged | container chunks read first-party | libwebp | `Pixels::Planar` for a lossy yuv page | implemented in [`formats/webp.rs`](../../src/formats/webp.rs) |
| avif | `dav1d 0.11.1`, unchanged | the item metadata walker | `dav1d` | `Pixels::Planar` | implemented in [`formats/avif.rs`](../../src/formats/avif.rs) |
| heif/heic | `libheif-rs 3.0.0` | `libheif`, unchanged | `libheif`, unchanged | `Pixels::Planar` | gray and yuv pages are implemented in [`formats/heif.rs`](../../src/formats/heif.rs); an rgb page is still routed to the `image` integration (`handles` excludes `ColorFamily::RGB`), and `color_space_of` already maps `Rgb8..Rgb16` to `Rgb(C444)`, so the rgb page needs a `handles`/`image_info` change and the integration deleted |
| jpeg xl | `jxl 0.7.4`, unchanged | codestream header | `jxl` | `Pixels::Planar` | implemented in [`formats/jxl.rs`](../../src/formats/jxl.rs) |
| jpeg 2000 | `jpeg2k 0.10.1`, unchanged | container boxes and codestream header | vendored OpenJPEG | `Pixels::Planar` | implemented in [`formats/jp2.rs`](../../src/formats/jp2.rs) |
| tiff | `tiff 0.11.3` | `Decoder::new`, `dimensions`, `colortype`, tags | `read_image_to_buffer`, or `read_image_bytes` per chunk | `Pixels::Interleaved`, one pass | promote the crate from `image`'s adapter to a direct dependency |
| exr | `exr 1.74.2` | header read, no pixels | `SpecificChannels` for the channels a frame has | `Pixels::Planar` | promote the crate to a direct dependency |
| qoi | `qoi 0.4.1` | `decode_header`, 14 bytes | `Decoder::decode_to_buf` | `Pixels::Interleaved` | **landed** in [`formats/qoi.rs`](../../src/formats/qoi.rs), and `image`'s qoi feature is off |
| bmp | image-rs's BMP decoder, ported | header | the port | `Pixels::Interleaved` | no candidate crate passes: `zune-bmp` fails every palette and RLE file in the corpus |
| ico | ported directory, then the BMP port or `png` | directory | the payload reader | the payload's route | `zune-bmp` has no DIB entry point and answers `probe_bmp=false` on every `.ico` |
| dds | ported header and DXT | header | the port | `Pixels::Interleaved` | `ddsfile` adds a proc-macro to the build graph and `bcdec_rs` changes samples and panics on a truncated block |
| farbfeld | written here | header | here | `Pixels::Interleaved` | the candidate's `decode_into` is unusable; the format is 8 bytes of header and BE `u16` rows |
| hdr | written here | header | here | `Pixels::Interleaved` | the candidate gets the exponent and the orientations wrong; the ported decoder is 687 lines, so writing it is the cheaper half |
| pnm | ported | header | the port | `Pixels::Interleaved` | the candidate has no `decode_into`, no `MAXVAL` rescale and no P1 to P4 |
| tga | ported | header | the port | `Pixels::Interleaved` | the plan's own choice was a reader written here; the ported decoder is 456 lines and its header parser 156 |

Where the table says *ported*, the decoder inside `image` is the starting
point and not a licence to copy blindly. Three things make a port the right
answer rather than a rewrite: those decoders already write into a
caller-provided buffer, so the frame path is the only part that changes;
`image` is MIT OR Apache-2.0 with no per-file header, so a port keeps the
crate's `LICENSE-*` and a `THIRD_PARTY_NOTICES` entry naming the files it came
from; and a port is what keeps the samples identical, including the places
where the format and the decoder disagree (the parity rules below). A rewrite
would have to rediscover each of those by measurement.

The `image`-specific types a port needs — `ColorType`, `ExtendedColorType`,
`ImageResult` and friends — are [29](29-decoder-types-without-image.md)'s job,
and so is the `ImageDecoder` trait shape. The audit lists the symbols each
file touches: 12 for BMP, 11 for ICO and TGA's decoder, 12 for DDS, 10 for
PNM and 8 for DXT, none of which is `ImageBuffer`, `DynamicImage` or `Pixel`.

`gif` and `apng` are not rows here because their still cases share the
animation plan's decoders: `gif 0.14.2` and `png 0.18.1` respectively.
[28](28-animation-container-decoders.md) owns both, and it owns the container
cases of webp, avif and heif that still reach `image`.

### why the candidates were rejected

The five zune still decoders were built and run over a corpus of real files,
alongside an image-rs 0.25.10 baseline over the same files. The harness and the
corpus are under `target/cand/`, and every line below is reproducible from them.

| candidate | what the run showed | verdict |
| --- | --- | --- |
| `zune-bmp 0.5.2` | 1, 4 and 8 bit palettes and RLE4/RLE8 answer `ERROR Position overrun` on files the baseline decodes; the crate's own docs say the entry point needs a `BM` file header, so an ICO payload is out of reach (`probe_bmp` answers false on every `.ico` in the corpus); a 32-bit DIB is forced opaque | rejected |
| `zune-qoi 0.5.2` | the decode loop is driven by the sink, so a `QOI_OP_RUN` never completes a run: `rgba.qoi` returns the wrong samples and then reports the last-bytes check | rejected |
| `zune-ppm 0.5.1` | P1, P2, P3 and P4 are refused outright (`Unsupported PPM version`), and a PAM whose `MAXVAL` is 31 does not match the baseline's rescale (`sha256=e907df53` against `11963c49`) | rejected |
| `zune-hdr 0.5.2` | exponent bytes 96 and 160 both decode to the same samples as 128, which is the `unsigned_abs() & 31` shift; only `-Y h +X w` is accepted of the sixteen Radiance orientations, where the baseline accepts every one | rejected |
| `zune-farbfeld 0.5.2` | `output_buffer_size()` returns bytes and `decode_into` slices that many `u16`, so a correctly sized sink is rejected and an oversized one runs past the samples: the corpus file ends in `Not enough bytes, expected 2 but found 0` | rejected |
| `ddsfile 0.6.0` + `bcdec_rs 0.2.0` | the two together decode DXT1/3/5 into a project buffer with no transitive `image`, but `bcdec_rs` expands a 5-bit channel by bit replication where image-rs truncates (213 against 214 on the first DXT3 block), which changes samples in an otherwise parity-only migration, and it panics on a truncated DXT3 block (`range end index 2 out of range for slice of length 0`) | rejected |
| `tiff 0.11.3` | not a candidate: it is the crate the `image` adapter already uses, and its chunk APIs answer per strip and per tile | selected |
| `exr 1.74.2` | not a candidate: it is the crate the `image` adapter already uses, and `SpecificChannels` names the channels a frame holds | selected |
| `qoi 0.4.1` | samples byte-identical to the baseline for both an RGB and an RGBA file | selected |
| `zune-jpeg 0.5.15` | the crate image-rs already decodes through, run over the real 35-file `sandbox/jpeg` corpus: every file is byte-identical to the baseline, and one `decode_headers` answers the 436-byte exif the baseline opens three extra decoders for | selected |

The corpus the runs used, and the two dumps:

- `target/cand/make-corpus.py` and `make-corpus-extra.py` write 56 files:
  six BMP depths, RLE4/RLE8, bitfields, BMP V4, four ICOs, six TGAs, every
  PNM subtype including a PAM with an odd `MAXVAL`, DXT1/3/5 at two sizes,
  farbfeld, HDR at three exponent bytes, QOI in both forms, and nine TIFF and
  EXR shapes.
- `target/cand/imgbaseline` is the image-rs 0.25.10 harness and
  `target/cand` is the candidate harness; both print
  `<name> <probe> | <decode>` so the two dumps diff line by line. They are
  `baseline-dump.txt` and `candidate-dump.txt` beside them.
- The two source audits behind the rows above are `target/cand/zune-audit.md`
  (68 kB) and `target/cand/image-internal-audit.md` (74 kB); both quote file
  paths and line numbers from the published crates.
- The jpeg row was checked against real pages rather than the synthetic
  corpus: `jpeg-baseline.txt` and `jpeg-candidate.txt` hold the same
  `sha256` for all 35 files of `sandbox/jpeg`.
`target/cand/compare.py` counts the matching hashes in two dumps, which is how
each row above was decided. The whole `target/cand/` tree is scratch work
under an ignored directory: it is evidence for this selection, not a build
input.

The image-rs decoders themselves were read as well, because the ones that are
ported have to be reproduced rather than the format. Two findings shape what a
ported module has to do:

- Every one of the seven decoders inside `image` already writes into a
  caller-provided buffer and none allocates a picture of its own, so a
  ported module can keep the plugin's own storage and no copy is lost by
  leaving `image`. What a port has to reproduce is each decoder's accepted
  subtypes and its alpha rule, which the audit lists per format.
- `Limits` never reach those decoders: `set_limits` is the trait default and
  the plugin opens decoders through `into_decoder`, which never calls
  `reserve`. A ported module therefore has to bound its own allocations, and
  `src/formats/avif.rs`'s metadata walker is the pattern to follow.

### parity rules the ported readers inherit

These are the image-rs behaviours a replacement must keep, taken from the
audit. They are listed because each of them is a place where the obvious
reading of the format is not what this plugin hands out today.

- BMP: a 32-bit `BI_RGB` bitmap drops its fourth byte, including in a V4 or V5
  header; alpha is handed out only when a bitfield alpha mask is non-zero.
- ICO: entries are scored by `(bits_per_pixel, width * height)`, so bit depth
  dominates area; a tie goes to the last entry that attains the best score.
  The payload is sniffed as PNG or as a bare DIB, a DIB's height is doubled
  and halved again, and the AND mask can only clear alpha.
- DDS: only DXT1/3/5 and their DX10 equivalents are accepted, the width and
  height must be multiples of four, and the mipmaps, other cube faces and
  other volume slices are ignored without an error.
- HDR: `Rgb32F` always, native-endian, with `value * 2^(exponent - 128) / 256`
  and a zero exponent byte meaning black.
- PNM: `P1` to `P7`, an ASCII raster whose comments are refused, and samples
  rescaled through `f32` whenever `MAXVAL` is not one less than a power of
  two.
- TGA: a 32-bit image with zero attribute bits is `Rgba8` and carries its
  fourth byte as alpha, which is the opposite of BMP's rule.
- QOI: the header's colourspace flag is not a `_Transfer` property, and
  nothing about it may become one in a parity migration.

### recommended implementation order

Cheapest first, by what a slice touches rather than by how common the format
is. Items 1 and 2 are a dependency promotion, item 3 is a small reader, and
items 4 to 8 are ports of code that already works; item 9 is the three
dependency changes, which wait for the module shape to settle.

1. **jpeg onto direct `zune-jpeg`** — the only slice that removes a repeated
   read the current code makes per file. One header pass replaces the four
   decoders `image` builds (pixels, ICC, exif and XMP), and `decode_into`
   removes the `image` pixel buffer. The exif orientation is the one thing
   `image` holds and zune-jpeg does not, so it needs a small parser over the
   `exif()` bytes; [09](09-exif-orientation.md)'s `Orientation` mapping is the
   shape to reproduce. Gate: the 35-file `sandbox/jpeg` corpus, byte-identical
   samples and properties, and the orientation fixtures in `tests/`
   (`orientation-6.png` has a jpeg sibling in `sandbox/level-check`).
2. **qoi directly** — **done**, in `f329f79`. `qoi 0.4.1` was already in
   `Cargo.lock` as the `image` adapter and already matched the baseline byte for
   byte, so this was a dependency promotion plus a module: [`src/formats/qoi.rs`](../../src/formats/qoi.rs)
   reads the fourteen byte header for the probe and runs the crate's decoder into
   the frame's buffer. `image`'s own `qoi` feature is off, which is what proves
   the crate no longer reads one. There were no committed qoi fixtures at all, so
   [`tests/make-qoi-fixtures.py`](../../tests/make-qoi-fixtures.py) wrote three: a
   three channel file, a four channel one with varying alpha, and one whose
   colours flag is set, which exists only to pin that the flag stays informative
   and never becomes a `_Transfer` property.
3. **farbfeld, written here** — 50 to 80 lines, one format, no dependency,
   and the smallest module that exercises a ported reader's shape
   ([`decoder::RowStream`](../../src/decoder.rs) or
   `Pixels::Interleaved`).
4. **ico and bmp, porting both together** — the ICO directory is 383 lines
   and the BMP decoder it needs is 1399, the largest of the set; the DIB path
   lives in the BMP file, so the two land as one slice. Port the accepted
   subtypes the corpus and `sandbox/` show and refuse the rest explicitly, so
   an unported subtype is an error rather than a wrong picture. The corpus has
   a three-entry icon, a PNG payload, a DIB payload, and the six BMP depths
   and compression forms.
5. **tga, porting its header and decoder** — 156 and 456 lines, and the
   format the plan already promised would be read here rather than taken from
   a crate. The corpus has all six raw/RLE and palette/truecolour shapes, and
   the 15-bit and attribute-bit rules are the parts that are not obvious.
6. **dds, porting its header and its DXT** — 345 and 350 lines, which keeps
   image-rs's exact 5-to-8 bit expansion and so its samples, and which the
   corpus can check against the baseline directly. Both candidate crates are
   rejected: `ddsfile` brings a proc-macro into the build graph, and
   `bcdec_rs` changes samples and panics on a truncated block.
7. **pnm, porting the parser** — 985 lines plus a 366-line header and a
   20-line autobreak helper, the widest coverage list of the set and the only
   one whose ASCII raster refuses comments the header accepts.
8. **hdr, written here** — 380 to 500 lines, both RLE schemes and the
   sixteen orientations the candidate refuses; the decoder in `image` is 687
   lines, so the RLE and RGBE parts are most of the work either way.
9. **tiff, exr and the heif rgb page** — the three dependency changes, once
    the ported modules have settled the shape. `tiff` and `exr` are already in
    `Cargo.lock` behind `image`, so the lock file gains a direct edge, not a
    new crate; the heif rgb page needs `libheif-rs` alone.

Steps 1 to 4 are individually reversible and each has a corpus on this
machine already. Items 1 and 3 are the ones that should not start without
committed fixtures: the synthetic BMP corpus was generated for this research
and no committed fixture exercises a palette or an RLE bitmap.
## the research audit of 2026-10-03

Some image-rs codecs are adapters over separate crates. Others live inside the
`image` crate and cannot be made direct dependencies just by changing an import.
The removal plan must cover both groups, including the existing direct modules.

This table is the audit the selection was made from; the decision for each row
is in [the selection](#the-selection) above. The last column is still the
checklist a replacement has to answer.

| format | current path / actual backend | the choice the audit led to | parity cases that block removal |
| --- | --- | --- | --- |
| PNG | image-rs metadata and fallback over `png 0.18.1`; `formats/png.rs` already streams eligible still rows | use the existing `png` dependency for all probing and decode paths | Adam7, 1/2/4/8-bit palettes and gray expansion, `tRNS`, 16-bit byte order, ICC, EXIF, `cICP`, APNG poster and one-presentation routing |
| JPEG | image-rs over `zune-jpeg 0.5.15` and `zune-core 0.5.3` | use direct zune-jpeg as planned; libjpeg-turbo remains a benchmark comparator | baseline/progressive, gray/RGB, CMYK/YCCK conversion, EXIF, multipart ICC, truncated inputs, decoder numerical differences |
| GIF | image-rs over `gif 0.14.2`, including its own canvas compositor | direct `gif` plus project composition, covered in [28](28-animation-container-decoders.md) | one-picture canvas offsets, palette, transparency and metadata, as well as animation disposal |
| WebP | native libwebp for still pixels; image-rs over `image-webp 0.2.4` for metadata and animation | keep the native still decoder and add direct container metadata/demux | lossless RGBA, lossy YUV, EXIF/ICC chunks, animation flag and one-presentation cases |
| AVIF | project container probe/dav1d for eligible color items; image-rs `avif-native` uses mp4parse + dav1d on fallback routes | retain dav1d and replace fallback integration under [28](28-animation-container-decoders.md) | monochrome, linked alpha, unspecified matrix RGB, split extents, grids and every currently accepted non-native case |
| HEIF/HEIC | direct libheif gray/YUV paths, image-rs hook for RGB/fallback pages | use `libheif-rs 3.0.0` directly for those remaining pages | RGB at stored word depth, unknown matrix, container transforms, unusual primary items and alpha |
| JPEG XL | direct `jxl 0.7.4`, still and animation | retain it and replace shared image-rs types | nominal 9–15-bit depths, gray/alpha, float paths, orientation and animation checkpoints |
| JPEG 2000 | direct `jpeg2k 0.10.1` with vendored OpenJPEG | retain it and replace shared image-rs types | JP2/codestream identification, signed/deep samples and existing alpha/metadata behavior |
| TIFF | image-rs adapter over `tiff 0.11.3` | direct tiff using its typed/byte-buffer and strip/tile APIs; compare libtiff only if a measured gap justifies native linkage | first IFD, planar/chunky storage, palette/CMYK, bit expansion, float, endian, compression, orientation, ICC and associated alpha |
| EXR | image-rs adapter over `exr 1.74.2` | direct exr with project-owned channel/plane storage | selected RGB layer, half-to-f32, alpha, display/data windows, offsets, largest level, missing channels and compression coverage |
| QOI | thin image-rs adapter over `qoi 0.4.1` | direct `qoi 0.4.1`; `zune-qoi` is rejected | RGB/RGBA, bounds, run/index operations, truncated streams and the current output properties |
| BMP | decoder internal to image-rs | port it; `zune-bmp` is rejected | palette/packed depths, top-down/bottom-up rows, bitfields, RLE, embedded image variants and alpha rules actually accepted today |
| ICO | decoder internal to image-rs using its PNG/BMP decoders | port the directory over the BMP port and `png`; `zune-bmp` has no DIB entry point | existing entry selection/ties, PNG payload, DIB payload, doubled DIB height, AND mask and alpha precedence |
| DDS | internal DDS headers and DXT decoder in image-rs | port it; `ddsfile` + `bcdec_rs` is rejected | DXT1/3/5, accepted DX10 forms, edge blocks, alpha rounding, first surface/mip and invalid headers |
| farbfeld | decoder internal to image-rs (`ff` feature) | write it here; `zune-farbfeld`'s decode path is broken | big-endian RGBA16, exact samples, opaque/color-only handling and truncation |
| HDR | RGBE/RLE decoder internal to image-rs | write it here; `zune-hdr` is rejected | scanline and legacy RLE, exponent conversion, axis signs/order and header/error behavior |
| PNM | PBM/PGM/PPM/PAM parser internal to image-rs | port it; `zune-ppm` covers P5 to P7 only | ASCII/binary forms, comments, PBM polarity/packing, `MAXVAL` expansion, tuple types and PAM alpha |
| TGA | raw/RLE and palette decoder internal to image-rs | port it | palette offsets, gray/RGB/alpha, RLE across rows, packed pixels and both origin axes |

For a ported decoder, the audit's per-format list of accepted forms and output
types is what the port has to keep. The cases above are an audit list, not a
claim that every legal subtype already works. Keep previously accepted files
working and previously refused files bounded and safe. New subtype support
belongs in an explicitly scoped follow-up.

## evaluate the same decoder before changing algorithms

A direct call to the same library is the lowest-risk first candidate. It keeps
the codec's implementation and SIMD behavior while allowing the plugin to own
metadata extraction and output storage. That can remove overhead, but it does
not make decompression itself faster by definition.

- `png` already offers row, interlaced-row and whole-frame reads. Keep [22](22-png-decode-path.md)'s
  fast path, implement direct metadata mapping, then replace its image-rs
  refusals with a correct direct buffered or Adam7 path. Buffered completion is
  acceptable when writing directly is not yet safe. Preserve `EXPAND` behavior
  and avoid stripping sixteen-bit samples. See the [png reader API](https://docs.rs/png/0.18.1/png/struct.Reader.html).
- Direct `zune-jpeg` offers `decode_headers`, metadata accessors and
  `decode_into`. The current image-rs adapter builds separate header decoders
  for ICC and EXIF and another decoder for pixels. Investigate one header pass
  for all probe facts and reuse of decode-time state. Avoid retaining the entire
  compressed corpus or an open decoder per file just to save a parse. The
  [zune-jpeg API](https://docs.rs/zune-jpeg/0.5.15/zune_jpeg/struct.JpegDecoder.html)
  does not by itself promise a VapourSynth-strided planar RGB sink.
- `tiff` exposes caller-buffer and chunk APIs as well as typed results. Its
  current image-rs adapter decodes into `DecodingResult`, handles conversions,
  then copies into the plugin's byte buffer. Start by avoiding that redundant
  ownership layer. Check complete planar output lengths and limits: some tiff
  APIs can return only the first sample plane, and subsampled coding units have
  documented limitations. See the [tiff decoder API](https://docs.rs/tiff/0.11.3/tiff/decoder/struct.Decoder.html).
- The inspected EXR adapter builds an f32 image and then copies its bytes to the
  generic buffer. Explore the [exr API](https://docs.rs/exr/1.74.2/exr/) for
  project-owned channel storage and chunk placement. Prove callback lifetimes,
  channel selection, clipping, initialization outside the data window and
  Rayon behavior before claiming a direct frame write.
- The existing QOI backend is already a thin adapter with `decode_to_buf`.
  Keep direct `qoi` as a comparator for zune-qoi rather than assuming a shared
  crate family must be faster. Its header
  colorspace flag must not silently introduce new `_Transfer` properties in
  what is meant to be a parity migration.

## alternative decoder candidates

**JPEG:** compare direct zune-jpeg with
[libjpeg-turbo](https://www.libjpeg-turbo.org/Documentation/Documentation).
The latter exposes grayscale/packed pixels and planar YUV APIs with explicit
strides, but RGB clips still need RGB planes. Returning JPEG YUV would change
the current public output contract and needs its own decision, not a hidden
speed optimization. Planar YUV decoding can also use internal copies for MCU
alignment. Check numerical/IDCT and chroma-upsample differences against exact
sample parity before choosing it. See the [TurboJPEG header](https://github.com/libjpeg-turbo/libjpeg-turbo/blob/main/src/turbojpeg.h).

**PNG:** retain and complete the direct Rust `png` path. Exclude libpng from
the candidate list as requested by the user; its expected disadvantage is a
scope decision, not a locally measured comparison. No libpng benchmark is
planned here. The Rust migration must still be compared with the current
image-rs/streamed baseline, including palettes and Adam7.

**TIFF/EXR:** the existing lower-level crates are the first candidates. Native
libtiff or OpenEXR are later research options if direct Rust integration fails
the measured performance or coverage gate. They are not selected dependencies;
their bindings, threading, Windows build and packaging need a separate audit.

**DDS:** [ddsfile](https://docs.rs/ddsfile/latest/ddsfile/) supplies container
structures and [bcdec_rs](https://docs.rs/bcdec_rs/latest/bcdec_rs/) supplies
block decompression, and the pair was rejected: `bcdec_rs` expands a 5-bit
channel by bit replication where the current decoder truncates, so it changes
samples, and it panics on a truncated block. `ddsfile` also pulls a proc-macro
into the build graph. A first-party header and DXT is the decision; see
[the selection](#the-selection).

**zune family:** the individual codecs below were the priority candidates
alongside the planned direct zune-jpeg integration. They were all built and run
on 2026-10-04 and none of them passed; see [the selection](#the-selection) for
what each one did. Header parsing is a promising integration boundary, not
evidence that a probe or playback is already faster. These crates share
`zune-core`; do not adopt the umbrella `zune-image` ownership/processing layer
merely to get their decoders.

## zune candidates and header-only probes

The published source versions inspected are BMP/QOI/HDR/farbfeld 0.5.2 and PPM
0.5.1. Their manifests depend on `zune-core ^0.5.1`, compatible with the existing
locked 0.5.3, and not on image-rs. They are rejected as decoders; the table is
kept as the record of what each one offers and what the run showed.

| format and candidate | probe API | pixel API and comparison obligations |
| --- | --- | --- |
| BMP — [zune-bmp 0.5.2](https://docs.rs/zune-bmp/0.5.2/zune_bmp/struct.BmpDecoder.html) | `BmpDecoder::decode_headers`, `dimensions`, `colorspace`, `depth`, `icc_profile` | `decode_into(&mut [u8])`; compare palettes, RLE4/8, masks, BGR/RGB ordering and alpha with image-rs and any project implementation |
| QOI — [zune-qoi 0.5.2](https://docs.rs/zune-qoi/0.5.2/zune_qoi/struct.QoiDecoder.html) | `QoiDecoder::decode_headers`, `dimensions`, `colorspace`, `bit_depth` | `decode_into(&mut [u8])`; compare image-rs, direct `qoi 0.4.1` and any manual candidate, preserving RGB/RGBA and header semantics |
| PPM/PGM/PAM — [zune-ppm 0.5.1](https://docs.rs/zune-ppm/0.5.1/zune_ppm/struct.PPMDecoder.html) | `PPMDecoder::decode_headers`, `dimensions`, `colorspace`, `bit_depth` | `decode()` returns `DecodingResult` with owned U8/U16/F32 vectors; no public `decode_into` in this version. Compare a manual row reader, complete subtype coverage and `MAXVAL` handling |
| HDR — [zune-hdr 0.5.2](https://docs.rs/zune-hdr/0.5.2/zune_hdr/struct.HdrDecoder.html) | `HdrDecoder::decode_headers`, `dimensions`, `get_colorspace`, `metadata` | `decode_into(&mut [f32])`; compare the full float range, axis handling and scanline/legacy RLE before timing a compatible manual or crate path |
| farbfeld — [zune-farbfeld 0.5.2](https://docs.rs/zune-farbfeld/0.5.2/zune_farbfeld/struct.FarbFeldDecoder.html) | `FarbFeldDecoder::decode_headers`, `dimensions`, `colorspace`, `bit_depth` | nominally `decode_into(&mut [u16])`, native endian; the source-level buffer-unit issue below blocks accepting this version. Compare a verified version with a manual RGBA16 row reader |

The inspected header functions parse metadata without invoking their pixel
decode functions. Use `BufReader<File>` through `zune-core`'s `std` reader
support where possible, so a small probe does not first read the whole file
into a `Vec`. `ZCursor` is appropriate when compressed bytes are already owned;
its availability is not a reason to retain one full file per sequence entry.
For crates without their own `std` switch, audit feature unification through a
direct zune-core dependency. QOI's defaults include `std` and logging; enable
only the needed reader features, and verify options/limits rather than adopting
upstream defaults unexamined.

Measure bytes read, seeks, allocations and time for the **whole probe**, not
only `decode_headers`. BMP headers may seek to and retain an ICC profile after
the pixel payload without decoding pixels. Bound that access and preserve
`ImgSeqHasICC` while retaining profile bytes only on demand. A variable-length
HDR/PNM header needs limits too. Dimensions, byte counts and sample counts must
be checked independently before backend allocations, and malformed/truncated
headers must remain errors. A contiguous `decode_into` buffer is not a
VapourSynth-strided planar sink; record the remaining placement pass.

### compatibility findings before selection

- **BMP/ICO:** the public BMP entry point checks a `BM` file header. It does
  not expose image-rs's `new_without_file_header` equivalent for an ICO DIB.
  Do not assume it replaces the ICO payload reader unchanged. Audit a bounded
  DIB adapter or project DIB reader, doubled heights and AND masks separately.
  Embedded PNG/JPEG compression is documented as unsupported. The overview
  says embedded profiles are ignored, but the inspected V5 header parser and
  `icc_profile` accessor do extract them: verify actual parity instead of
  relying on that inconsistent overview. Source:
  [BMP decoder](https://docs.rs/crate/zune-bmp/0.5.2/source/src/decoder.rs).
- **PNM:** source dispatch accepts P5, P6, P7 and float-map forms, but not
  P1/P2/P3/P4. The [published coverage table](https://docs.rs/zune-ppm/0.5.1/zune_ppm/)
  is therefore not complete PBM/PGM/PPM support. A hybrid or manual reader must
  keep the currently accepted ASCII and packed PBM forms. The inspected pixel
  path returns raw samples; it does not reproduce image-rs's `MAXVAL` scaling
  and rounding. Capture the original maximum separately and compare cases
  such as 1, 15, 255, 1023, 4095 and 65535, PAM tuple validation and alpha.
  PFM support must not silently add a new public format in a parity migration.
  Source: [PPM decoder](https://docs.rs/crate/zune-ppm/0.5.1/source/src/decoder.rs).
- **HDR:** source accepts the `-Y/+X` and `+X/-Y` axis-token pairs, not every
  orientation. Verify output order for each form accepted by the baseline.
  Its RGBE conversion masks the absolute exponent with 31 and uses a wrapping
  u32 shift, suggesting incorrect conversion outside that exponent range.
  Include exponent bytes 96 and 160 (unbiased exponents -32 and +32) in the
  reproduction corpus; the inspected expression folds both to a zero shift.
  Resolve this with a verified upstream version or a scoped project reader
  before acceptance. Compare exact RGB32F samples, including zero exponent,
  extremes, small scanlines and old/new RLE. Source:
  [HDR decoder](https://docs.rs/crate/zune-hdr/0.5.2/source/src/decoder.rs).
- **farbfeld:** `output_buffer_size` computes eight bytes per pixel, while
  `decode_into` treats that result as a count of `u16` elements. `decode()`
  allocates four elements per pixel and passes them to that check. This appears
  to reject a normal nonempty image, starting with a one-pixel RGBA16 fixture;
  providing an oversized sink would instead
  make it read twice the required sample count. Verify or resolve upstream
  before using this version, rather than hiding the mismatch with padding.
  Source: [farbfeld decoder](https://docs.rs/crate/zune-farbfeld/0.5.2/source/src/decoder.rs).

These findings are source inspection, not executed candidate tests or benchmark
results. Adoption requires reproductions and a verified compatible version.
The five inspected manifests license their code under MIT OR Apache-2.0 OR
Zlib; BMP/QOI/farbfeld declare Rust 1.87 and HDR/PPM 1.77, within this project's
1.94 requirement. Recheck the exact selected version, features, notices and
Windows/Linux/macOS behavior at implementation time.

## manual readers must compete too

TGA is planned as a manually written project decoder. Other manual readers are
candidates where a direct row path is useful or a crate leaves a coverage gap,
especially PNM, farbfeld and ICO DIB. Writing fewer lines or avoiding a generic
buffer is not evidence that those readers are faster.

For every manually implemented format, compare the existing image-rs baseline,
the relevant direct crate candidates that pass correctness, and the manual
reader on the same subtype corpus. QOI additionally keeps direct `qoi` as a
comparator. TGA must at least compare with its existing image-rs decoder; a
scoped extracted reference may be another comparator, without requiring a
zune TGA decoder as a comparator. Where no crate currently passes coverage, record
that exclusion and still compare the manual reader with image-rs. Time the
complete hybrid adapter if a manual path fills only a dependency's gaps.

Keep header-only/open costs separate from decode and planar placement. Compare
end-to-end wall time, CPU, first-frame latency, allocations/copies and peak
memory for `Read` and both `ReadAlpha` outputs, with identical samples,
properties, limits and output layouts. Use [26](26-remove-image-rs.md)'s paired
baseline protocol and [BENCH.md](../BENCH.md), including probe I/O with cold and
warm file caches, small files and large pages. Do not claim an improvement over
image-rs until the measured gain exceeds repeat-run variation. Investigate and
fix significant speed or memory regressions before accepting a reader.

Extraction remains a reference option, but imports of image-rs traits, buffers,
errors and utilities must be replaced. Preserve upstream notices, provenance
and licenses for copied code. A new implementation creates a maintenance
obligation for every accepted subtype. A correct uncompressed RGB fixture does
not prove a complete BMP, ICO or TGA reader.

For every candidate record header-only capability, ownership/copies, output
layout and stride, depth/endian/alpha semantics, orientation and ICC access,
thread budget, malformed-input behavior, license, MSRV and all release targets.
Default to decode-only features; enabling a wrapper's default encoders or
unrelated formats works against the dependency-removal goal.

## integration plan

The reusable boundary is already here: `ImageInfo`, `DecodedImage`, `Pixels`,
`Demand`, `RowStream` and `RowSink`. Feed them directly from the chosen backend
and use [29](29-decoder-types-without-image.md)'s small layout types. Keep codec
enums inside the format module and make one explicit mapping at that boundary.
Do not replace image-rs with another generic image ownership/conversion layer.

Probe should describe the selected decode route and preserve ICC presence while
retaining bytes only when requested. Decode should verify file dimensions,
layout and relevant identity again. Compressed bytes, temporary rows and backend
buffers must have bounded lifetimes and count toward the memory investigation.
Initialization cannot be skipped unless every byte subsequently exposed in all
color and alpha planes is written, including canvas gaps and partial tiles.

`Read` may avoid extracting a separately coded alpha item when `Demand::COLOR`
allows it. An inline alpha channel in PNG/QOI/TGA does not mean decompression of
that channel can always be skipped. Preserve straight/associated alpha behavior
and avoid color transforms merely because an ICC profile is present.

## checks before each migration lands

Use [26](26-remove-image-rs.md)'s baseline protocol and [BENCH.md](../BENCH.md).
Run baseline and candidate on identical output formats, samples and properties,
including both `ReadAlpha` clips and different request orders. Check probe
counts, allocation/copy costs, open + frames, CPU and peak memory. Any proposed
speed improvement over image-rs must be measured, not inferred from fewer APIs.

Run the relevant Rust tests, clippy, formatting and release validator before and
after implementation. Extend fixtures for that format's coverage gaps, bounds,
truncation and changed-file detection. Validate packaging when native linkage
changes, with exact licenses and required source/relinking material. Investigate
and fix significant throughput or memory regressions. Disable one image-rs
format feature only after its replacement passes; the final removal still waits
for all formats and [28](28-animation-container-decoders.md)'s fallback cases.
