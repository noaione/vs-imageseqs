# changelog

## unreleased
### changed

- The plugin no longer asks `image` for a jpeg decoder. That feature was merely
  `image`'s own link to `zune-jpeg`, which this tree depends on directly for
  `src/formats/jpeg.rs`, so the flag put a second adapter in front of the same
  decoder: a jpeg this reader declines, `image` declined too. `Cargo.lock` loses
  those two edges from `image` and keeps the package, which the direct reader
  still uses. No frame, format or property changes. The `jpeg` flag on the
  `tiff` dependency is a different one and is still needed, for a strip that
  holds a jpeg.
- Creating a clip over a long list is faster. Describing 170 files went from a
  median of 78 ms to 40 ms, over seven interleaved pairs. Every format module
  answered "is this mine?" by opening the file, so one file was opened once per
  module -- about fifteen times -- and the decode then asked all of it again
  with a gate of its own. The content decides that question in one place now,
  `identify::route`, and the probe and the decode each ask it once: sixteen
  opens down to one in the decode, and fifteen down to two in the probe. A
  renamed file also routes the same way in both halves now, which it did not --
  webp, heif, jxl and jpeg 2000 answered the decode from the extension while
  the probe answered from the bytes, so a file whose name lied about it could
  be described as one format and decoded as another. An animated webp under a
  name that does not say `webp` keeps the single-frame read it had, because
  libwebp's simple entry points read one image and refuse a container of them
- An OpenEXR whose first part holds no colour channel now decodes. The probe and
  the decode were asking different questions: the probe took the first part that
  states `R`, `G` and `B`, and the decode took the first part it could read at
  all -- which in a file whose first part is a depth pass is a layer with no
  colour in it -- so a file the probe accepted failed at the frame request with
  "the file states no `\"R\"` channel". Both now select by the same rule. Every
  other OpenEXR is byte for byte identical, and the two changes that come with
  it are small: the channels are written straight into the frame instead of
  into a plane a second pass interleaves, so the `debug` log's timings count
  that work with the open rather than separately.
- A TIFF that states an orientation and carries an embedded ICC profile now
  reports both. The adapter had been building its description with the two
  hardcoded away, so a file that states orientation 6 -- a quarter turn clockwise
  -- was handed out stored, and `ImgSeqOrientation` said 1 where the file said 6.
  The profile is now read too, so `ImgSeqHasICC` and the opt-in `ICCProfile`
  property are right for a TIFF. A BigTIFF is no longer declined either: the
  adapter's own four-byte signature check accepted only the classic version word,
  so the crate's BigTIFF support could not be reached. Files that state neither
  tag, and classic TIFFs, are byte for byte identical.

- A PAM that states a `MAXVAL` above a byte but names no `TUPLTYPE` is now read at
  the width its `MAXVAL` states instead of a byte a sample. Such a file was read
  without an error and with the wrong samples: a raster of `[1023, 512]` under a
  `MAXVAL` of 1023 came back as `[1, 64]`, because the bytes were taken one at a
  time and rescaled as eight bit. It now reads as `[65535, 32800]`, which is the
  same picture at the sixteen bit container the plugin hands out. A file that
  names its tuple, or whose `MAXVAL` fits a byte, is unchanged.

- A TIFF compressed with zstd now decodes. Its decompressor is not one of the
  `tiff` crate's default features, so it is named by hand here, and libzstd is a
  native dependency the plugin links as a result -- see `LICENSES/zstd-COPYING.txt`.
- A Radiance HDR whose resolution line states an orientation other than the
  common `-Y ... +X` now decodes instead of being refused. The `image` decoder
  accepted only that one spelling and answered "does not support the format
  features Orientation ..." for every other, so a file that stored its scanlines
  bottom to top, or its pixels right to left, or stated the axes the other way
  round, could not be read at all. Four files that hold the same picture four
  different ways now come out as that one picture. Every file that already read
  is byte for byte identical.
- An AVIF or HEIF whose samples are stored as r,g,b is now read by `libheif`
  rather than by the `image` decoder, which is one library for every such
  container instead of two. The frames are byte for byte identical. One
  property moves: a file that carries no alpha channel now reports
  `ImgSeqOriginalColorType=rgb8` where the `image` decoder reported `rgba8`,
  because its avif hook always named four channels whatever the file held.
- An AVIF whose coded item is written as several extents is read by the plugin
  itself instead of being handed to the `image` decoder. The extents are one
  payload split across the container, and joining them is a concatenation in
  the order the file lists them, so the file and the one it was cut from are now
  the same picture through the same reader. A file written that way used to
  come out as `RGB24` where the file it was cut from came out as the `YUV420P8`
  the container states.
- An AVIF whose coded item is a grid of tiles now decodes instead of being
  refused. The plugin's own walk does not join a grid, so the file goes to
  `libheif`, which does, rather than to the `image` decoder, which could not:
  it has no monochrome avif, and a grid's cells are monochrome, so it answered
  `Invalid argument`. A `avifenc -g 2x2` file reads back as the joined
  256x256 `Gray8` picture `avifdec` reads from it.

### fixed

- Windows builds enable libheif's built-in dav1d decoder, so animated AVIF
  sequences decode instead of failing with `NoMatchingDecoderInstalled`.
  libheif's default features stay disabled, and no additional codec is selected.

### performance

- PNG decoding hands each decoded row to the frame it belongs in instead of
  building the whole picture in a buffer the plugin then copies, which is one
  pass over the image rather than two: 1.2x on the decoded side of every PNG
  set measured, and a frame is byte for byte identical either way
  - this reaches every colour type and bit depth, `tRNS` on grey, rgb and
    palette, and an odd width; an interlaced or animated file, and one the
    caller asked to rotate, still goes through the `image` decoder
  - a grey page is a copy per row and a colour page a single walk over the row
    that fills all three planes, so `Read` no longer pays a second pass at all
  - a palette page is expanded by the plugin rather than by the decoder, which
    is one pass over one byte per pixel where `Transformations::EXPAND` is a
    pass over three: a further 1.10x on the pages made of them
  - every png set measured is now ahead of Pillow's own decode column, from
    0.89x to 0.69x of its time
- x86-64 wheels carry one plugin library per microarchitecture level, and
  VapourSynth loads the widest the machine's CPU supports: 6% to 11% faster on
  the PNG and JPEG sets measured, almost all of it in the write into the frame
  - the libraries are `vs_imageseqs.dll`, `vs_imageseqs.avx2.dll` for AVX2 and
    `vs_imageseqs.avx512.dll` for AVX-512, and a unix wheel names the same
    three with a `lib` prefix and a `.so` extension
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
