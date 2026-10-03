# 27 - direct still decoders and alternatives to image-rs

status: proposed, research only. part of [26](26-remove-image-rs.md). The current
backend column describes `Cargo.lock`; alternative versions below were inspected
separately from their published sources on 2026-10-03. Direct zune-jpeg is the
planned JPEG integration. Other zune codecs are priority candidates, with
coverage and benchmark gates still open. No replacement was implemented or
benchmarked in this candidate research.

## current backends and proposed ownership

Some image-rs codecs are adapters over separate crates. Others live inside the
`image` crate and cannot be made direct dependencies just by changing an import.
The removal plan must cover both groups, including the existing direct modules.

| format | current path / actual backend | proposed replacement or research choice | parity cases that block removal |
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
| QOI | thin image-rs adapter over `qoi 0.4.1` | compare `zune-qoi` with direct qoi's header and `decode_to_buf`; measure any manual candidate too | RGB/RGBA, bounds, run/index operations, truncated streams and the current output properties |
| BMP | decoder internal to image-rs | evaluate `zune-bmp` first; compare a scoped project decoder or legally extracted module where coverage requires it | palette/packed depths, top-down/bottom-up rows, bitfields, RLE, embedded image variants and alpha rules actually accepted today |
| ICO | decoder internal to image-rs using its PNG/BMP decoders | project directory selection plus direct PNG/BMP payload readers; zune-bmp needs a separate DIB compatibility decision | existing entry selection/ties, PNG payload, DIB payload, doubled DIB height, AND mask and alpha precedence |
| DDS | internal DDS headers and DXT decoder in image-rs | evaluate `ddsfile` + `bcdec_rs`, or scoped extraction of the existing DDS/DXT implementation | DXT1/3/5, accepted DX10 forms, edge blocks, alpha rounding, first surface/mip and invalid headers |
| farbfeld | decoder internal to image-rs (`ff` feature) | compare `zune-farbfeld` with a small project header/row reader; resolve the inspected version's buffer-unit blocker first | big-endian RGBA16, exact samples, opaque/color-only handling and truncation |
| HDR | RGBE/RLE decoder internal to image-rs | evaluate `zune-hdr` against a project reader/extracted reference, retaining float output and full exponent behavior | scanline and legacy RLE, exponent conversion, axis signs/order and header/error behavior |
| PNM | PBM/PGM/PPM/PAM parser internal to image-rs | evaluate `zune-ppm` for covered subtypes; compare project parsing/row reads and fill all compatibility gaps | ASCII/binary forms, comments, PBM polarity/packing, `MAXVAL` expansion, tuple types and PAM alpha |
| TGA | raw/RLE and palette decoder internal to image-rs | manually written project decoder, benchmarked against image-rs and any extracted reference | palette offsets, gray/RGB/alpha, RLE across rows, packed pixels and both origin axes |

For internal codecs, enumerate the old decoder's accepted forms and output
before choosing the replacement. The cases above are an audit list, not a claim
that every legal subtype already works. Keep previously accepted files working
and previously refused files bounded and safe. New subtype support belongs in
an explicitly scoped follow-up.

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
structures; [bcdec_rs](https://docs.rs/bcdec_rs/latest/bcdec_rs/) supplies block
decompression. A header crate is not a complete decoder. Audit current licenses,
maintenance, API versions and DXT interpolation parity before choosing this pair.
Avoid adopting an umbrella crate that brings `image` back transitively.

**zune family:** prioritize the individual codecs below alongside the planned
direct zune-jpeg integration. Header parsing is a promising integration boundary,
not evidence that a probe or playback is already faster. These crates share
`zune-core`; do not adopt the umbrella `zune-image` ownership/processing layer
merely to get their decoders.

## zune candidates and header-only probes

The published source versions inspected are BMP/QOI/HDR/farbfeld 0.5.2 and PPM
0.5.1. Their manifests depend on `zune-core ^0.5.1`, compatible with the existing
locked 0.5.3, and not on image-rs. These are researched versions, not new lockfile
entries or assurances about future releases.

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
