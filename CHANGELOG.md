# changelog

## unreleased

### changed

- **The `image` crate is gone**. Every format this plugin reads has a reader of its
  own under `src/formats/`. A file whose bytes and extension both name no format
  here is refused with `no reader here knows its format` rather than sent to it.
  The plugin is about 435 KB smaller. Several of those readers are ports of that
  crate's, so its licence texts and notices stay where they were
- Support cmyk tiff is read again, and a cmyk tiff with an alpha channel is read for
  the first time. Both are handed out as rgb by the same formula of
  `(maximum - ink) * (maximum - k) / maximum` in `f32` with the same
  truncation, so a file reads as it always did -- which is why a black-only
  pixel of 128 comes out 126 rather than 127. `ImgSeqOriginalColorType` is
  `Cmyk8` or `Cmyk16`, and a fifth sample becomes the alpha clip
- A palette tiff is now properly supported (partially)
- A ycbcr tiff is read for the first time, as the file's own planes:
  `YUV444P8`, `YUV422P8` and `YUV420P8` for the three samplings the format
  defines. `_Matrix` comes from `YCbCrCoefficients` and `_Range` from
  `ReferenceBlackWhite`, defaulting to bt.601 at full range, and a page whose
  coefficients name no matrix VapourSynth has a code for is converted to rgb
  with the coefficients it states. Eight bit samples, uncompressed, and
  libtiff's own `tiff2rgba` gives the same samples for every fixture
- Whether a file plays a timeline is decided by its bytes now, like its format
  already was, so an animated gif, webp, avif or heic under a name that does
  not say so plays its timeline instead of a single still. The dispatch is one
  `identify::route` call rather than five extension checks, so the file is read
  once instead of up to five times
- `identify::route` decides whose file this is once, instead of every format
  module opening the file to ask -- about fifteen times -- and the decode asking
  all of it again with a gate of its own. The probe saves that route and every
  module takes it, so a file is opened once to identify it and not again: over
  the fixture set, clip creation costs 3% more per file than the released
  build's extension check did
- A tiff that states an orientation, carries an ICC profile or is a BigTIFF now
  reports all three: the adapter had hardcoded the first two away and its
  signature check accepted only the classic version word
- A PAM whose `MAXVAL` is above a byte but which names no `TUPLTYPE` is read at
  the width its `MAXVAL` states, instead of coming back rescaled as eight bit
- A tiff compressed with zstd decodes, which links libzstd -- see
  `LICENSES/zstd-COPYING.txt`
- A radiance hdr that states an orientation other than the common `-Y ... +X`
  decodes rather than being refused
- An OpenEXR whose first part holds no colour channel decodes: the probe and the
  decode now select the part by the same rule
- An avif whose coded item is written as several extents, or is a grid of
  tiles, is read rather than refused or misread: the extents are joined here
  and a grid goes to `libheif`, which joins it
- An avif or heif whose samples are stored as r,g,b is read by `libheif`. One
  property moves: a file that carries no alpha now reports
  `ImgSeqOriginalColorType=rgb8` rather than the `rgba8` that adapter always
  named
- A png is identified by its bytes on the probe side too, so a file whose name
  lies about it gets the same description and the same `cICP` properties as one
  whose name does not
- Support one frame gif properly (read as still image).
- A webp is described from its own chunks rather than by `image`, so
  `ImgSeqOrientation` and `ImgSeqHasICC` come from the file, and a webp cut
  short is refused rather than described

### fixed

- Windows builds enable libheif's built-in dav1d decoder, so animated AVIF
  sequences decode instead of failing with `NoMatchingDecoderInstalled`.
  libheif's default features stay disabled, and no additional codec is selected.

- An animated png's delays are placed exactly. A frame that states a delay whose
  denominator a thousand does not divide -- a third of a second, say -- had its
  delay rounded to whole milliseconds before it was added to the timeline, so a
  long animation drifted; the timeline now runs at the lowest common denominator
  of the delays the file states

### performance

- Creating a clip over a long list reads the front of each file instead of the
  whole file, where the container states what a description needs there. Over
  seven interleaved pairs a set: 35 webp files of 146 MiB went from a median of
  99 ms to 6 ms, 35 jpeg xl of 174 MiB from 157 ms to 5 ms, 35 jpeg 2000 of
  251 MiB from 159 ms to 4 ms, and 35 avif plus 35 heic of 362 MiB from 84 and
  170 ms to 6 and 13 ms
- Describing an animated png reads the frame delays its `fcTL` chunks state
  rather than rendering every frame to reach them: five 1024x1024 files of eight
  frames each went from a median of 49 ms to 3 ms, and 35 still pngs of 65.8 MiB
  are unchanged over the same protocol

- PNG decoding hands each decoded row to the frame it belongs in instead of
  building the whole picture in a buffer the plugin then copies, which is one
  pass over the image rather than two: 1.2x on the decoded side of every PNG set
  measured, and a frame is byte for byte identical either way
  - a palette page is expanded by the plugin rather than by the decoder, which
    is one pass over one byte per pixel where `Transformations::EXPAND` is a
    pass over three: a further 1.10x on the pages made of them
  - every png set measured is now ahead of Pillow's own decode column, from
    0.89x to 0.69x of its time
  - an interlaced or animated file, and one the caller asked to rotate, still
    goes through a whole-picture decode
- x86-64 wheels carry one plugin library per microarchitecture level, and
  VapourSynth loads the widest the machine's CPU supports: 6% to 11% faster on
  the PNG and JPEG sets measured
  - all three decode identically, and the plain library still passes no
    `-C target-cpu`, so a machine that could load the plugin before still can
  - the Windows wheel is 10.4 MiB instead of 3.8 MiB, which is what the three
    builds cost

### build

- the Linux release includes a musllinux wheel beside the manylinux one, for
  hosts whose C library is musl rather than glibc, where the manylinux wheel
  cannot load at all
  - the musl wheel carries the same three plugin libraries, so an AVX2 or
    AVX-512 host is served there the same way
  - auditwheel's musl policy promises the host only libc and libz, so the wheel
    bundles the C++ runtime the embedded libheif needs instead of leaving the
    plugin needing a package Alpine does not install by default
  - it is accompanied by `linux-musl-relink-source.tar.gz`, the corresponding
    source archive the manylinux wheel already has

## [0.2.1] - 2026-09-30

### build

- fix wrong version number in the plugin manifest itself.
- macOS arm64 wheels and standalone plugin ZIPs include dav1d and libde265, so
  users do not need to install those runtime libraries separately; they target
  macOS 11 or later

## [0.2.0] - 2026-09-30

### added

- animated GIF, APNG, WebP, JPEG XL, AVIF and HEIF/HEIC inputs now play their
  displayed timeline instead of contributing one frame
  - each output frame shows the picture the file displays at that instant, sampled
    onto the clip's own `fpsnum`/`fpsden`, so a fractional rate and a delay that
    does not divide it both land where the file says
  - a picture with no delay is held for one output tick rather than dropped, and a
    file's loop count is ignored: each listed path plays once
  - a 16-bit APNG stays 16-bit, and a HEIF or AVIF sequence is cropped to the
    picture it presents rather than handed out as coded
  - still images keep their frame count, format, pixels and metadata whether or
    not they share a list with animations
  - a colour-only read of a HEIF sequence still decodes its alpha track, because
    the library gives no way to skip it

### fixed

- a frame request on an AVIF whose item holds no coded frame no longer hangs
  forever; it fails with a decode error naming the file
  - the item decoder now runs at low latency, which is what lets "the decoder has
    all of the item and has no picture" be told apart from "the picture is still
    being decoded"
- an AVIF or HEIF/HEIC that states its orientation as the `irot` and `imir` item
  properties is now handed out the way the file describes it
  - `ImgSeqOrientation` reports the equivalent EXIF code instead of `1` for such
    a file, and `apply_rotation=False` hands the stored picture back at the
    stored size, which neither container could do before
  - a container transform is the normative statement for these two formats, so an
    EXIF tag beside one stays informational and is not applied on top of it
- an AVIF whose item is split over several extents now decodes instead of failing
  every frame request
  - the container reader does not join extents, so such a file is described as the
    format the fallback decoder produces rather than as the YUV its samples are;
    that decoder does join them
- a malformed AVIF container is refused instead of being read past its own bounds
  - an extent the file does not hold is no longer allocated before the read that
    fails on it, an `iloc` field wider than an address no longer shifts a value
    out of its type, and a box with an extended size header is read with the whole
    header rather than the last eight bytes of it being dropped
- a colour-only read of an AVIF no longer fails on a file whose alpha item is
  broken
  - the alpha item is a coded item of its own and is not part of what `Read` hands
    out, so it is not read; `ReadAlpha` still reports the problem on the same file

### performance

- AVIF decoding is 15.6% faster on the 35-page sandbox set, and 43% faster on
  `sandbox/hitokage-sample`: a still image is one frame, so the item is decoded
  directly instead of through a frame-delay pipeline it cannot use
- reading a container's orientation costs 0.006 ms per AVIF file and 0.037 ms per
  HEIF file at clip creation, and nothing per frame
- bounding an AVIF item's extent costs one file metadata call per file at clip
  creation, 0.005 ms per file, and nothing per frame
- a read that hands out no alpha clip no longer decodes an AVIF's alpha item,
  which is a coded item of its own: 11–20% off a colour-only read of an
  alpha-bearing page, with the colour planes byte identical
- a probe no longer keeps a file's embedded ICC profile unless `icc_profile=True`
  asks for it, which is 36 MiB off a 35-file clip whose files each carry a 1 MiB
  profile; `ImgSeqHasICC` is unchanged
- a still image reads as fast as it did before, on every sandbox set and at every
  lookahead depth, so animations cost the still path nothing

### build

- improvement to linux wheel distribution, manylinux now has dropped down to glibc 2.28
  - linux build now bundles `libdav1d.so.7` and `libde265.so.0` to avoid runtime dependency issues
- change the plugin layout to become `plugins/imageseqs/` with a `manifest.vs` file, and the plugin loader path to `$ORIGIN/lib`

## [0.1.0] - 2026-09-20

the first release of `vapoursynth-imageseqs`, a native image-sequence source
for VapourSynth.

### added

- `Read` for ordered image sequences and `ReadAlpha` for separate color and
  alpha clips.
- support for PNG, JPEG, WebP, AVIF, HEIF/HEIC, JPEG XL, JPEG 2000, DDS,
  farbfeld, TIFF, EXR, HDR, PNM, QOI, TGA, BMP, GIF, and ICO.
- native YUV output for compatible WebP, HEIF/HEIC, AVIF, and JPEG 2000
  sources.
- nominal 9–15-bit formats, including 10-bit and 12-bit gray, RGB, alpha,
  and YUV output.
- EXIF and codestream orientation handling through `apply_rotation`.
- CICP and container color metadata as VapourSynth frame properties.
- optional embedded ICC profile export through the binary `ICCProfile` frame
  property.
- variable-size and variable-format sequences through `mismatch`.
- background prefetching with a bounded memory budget.
- `debug` timing logs for probing, decoding, frame writing, and properties.

### performance

- native libwebp decoding and direct lossy WebP YUV output.
- direct HEIF and AVIF plane output where the container metadata permits it.
- JPEG XL and JPEG 2000 header-only probing before frame requests.
- frame construction moved into the prefetch workers to reduce request-thread
  work.
