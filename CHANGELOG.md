# changelog

## unreleased

### changed

- **The `image` crate is gone**. Every format this plugin reads has a reader of its
  own under `src/formats/`. A file whose bytes and extension both name no format
  here is refused with `no reader here knows its format` rather than sent to it.
  The plugin is about 435 KB smaller. Several of those readers are ports of that
  crate's, so its licence texts and notices stay where they were
- A bitmap with the older `BITMAPCOREHEADER` is read instead of being refused.
  The three places it differs from the header this plugin already read are handled
  directly: its dimensions are signed sixteen bit values, it has no compression
  field so only the uncompressed form exists, and a palette entry is three bytes
  rather than four. A core header has no colour count either, so a palette page of
  one carries an entry for every index its depth names. It cannot be stored
  top-down, and one that says so is refused by name
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
- A gray tiff of one, two or four bits a sample is read rather than refused.
  The decoder hands such a page over packed -- a 16x8 bilevel page arrives as
  sixteen bytes where a hundred and twenty-eight samples belong -- so the
  samples are unpacked here and widened by `value * 255 / (2**bits - 1)`,
  which is libtiff's own widening and is what leaves a bilevel page black and
  white rather than nearly black. The page is handed out as `Gray8` whatever
  width it states, `PhotometricInterpretation` still decides which end is
  black, and a page of one, two or four bits that states a predictor is still
  refused, by name
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
- A Targa colour map whose entries are fifteen or sixteen bits is read rather
  than refused. The entries are five bits a channel, widened by the same
  round-to-nearest table a direct sixteen bit image uses. A sixteen bit entry's
  attribute bit at bit fifteen is the entry's alpha, and a fifteen bit entry
  states none, so such an entry is opaque
- A bare device-independent bitmap is read: a `.dib` holds no `BM` file header at
  all, so its bytes start at the DIB header. The name selects the bitmap reader
  and a strict probe confirms the file is one -- a header size the reader walks,
  a depth it reads and a compression code it takes -- so a file renamed `.dib`
  that is not a bitmap is still refused
- A Windows cursor is read, which its type word used to be mistaken for a bit
  depth and refused for. A cursor's directory is the icon's with two fields
  moved -- an icon states its colour planes and its depth where a cursor states
  the hot spot -- so it is selected on area alone, and the payload and the AND
  mask rule are the icon's: the alpha clip is the payload's own alpha multiplied
  by the mask
- A gray tiff whose second sample is alpha is read, at eight, sixteen and
  thirty-two bits. The pinned decoder names such a page `Multiband` whatever its
  depth, so the shape is read from the directory instead: two samples and an
  `ExtraSamples` value that names alpha. The second sample becomes the alpha
  clip, and `ImgSeqOriginalColorType` is `La8`, `La16` or `La32F`
- A flat grayscale OpenEXR is read: `Y` is the colour plane and `A` is the
  alpha clip, which is the same rule the r,g,b channel set already followed, and
  `ImgSeqOriginalColorType` is `L32F` or `La32F`. A layer that states neither
  set is still declined rather than described wrongly

### fixed

- A signed JPEG 2000 page, or one whose components state two widths, is refused
  with a message that names the component, its width and what is wrong with it,
  where a single sentence used to cover both cases
- The musllinux wheel no longer crashes on a TIFF (or an AVIF with no picture)
  read on a VapourSynth worker thread. musl gives such a thread 128 KiB of
  stack, and the format dispatcher had grown a 100 KiB frame because every
  format's decoder was inlined into it. Each format's `decode` and `stream`
  entry point is now a call of its own, so the dispatcher's frame is small and
  only the format being read uses its own

- Windows builds enable libheif's built-in dav1d decoder, so animated AVIF
  sequences decode instead of failing with `NoMatchingDecoderInstalled`.
  libheif's default features stay disabled, and no additional codec is selected.

- An animated png's delays are placed exactly. A frame that states a delay whose
  denominator a thousand does not divide -- a third of a second, say -- had its
  delay rounded to whole milliseconds before it was added to the timeline, so a
  long animation drifted; the timeline now runs at the lowest common denominator
  of the delays the file states

- An animation whose length is not a whole number of output frames keeps its
  last frame. The clip was given the complete output ticks the source covers, so
  a 600 ms animation at 24 fps was 14 frames and dropped the picture shown at
  583 ms; the count is now the number of output sample instants before the
  segment ends, which is what the sampling rule states, and that clip is 15

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

- A netpbm, a targa and a bitmap are read a row at a time rather than into a
  buffer the size of the file: a 3000x3000 image of each holds 48 MiB rather
  than 74 MiB while it decodes, and decodes 7 to 14% faster

- A netpbm whose header is longer than the window it was read through is read.
  A comment is legal anywhere in the preamble and may be arbitrarily long, so a
  `P6` with a 70,000-byte comment failed to identify with "the header states no
  width"; the header is now read as far as the parse needs, to a megabyte

- A DirectDraw surface whose width or height is not a multiple of four is read
  instead of refused. Its last block of a row and of a column is in the file in
  full, and the pixels that hang over the edge are dropped: a 7x24 DXT1 or DXT5
  texture decodes to 7x24, where it failed to identify before

- A planar tiff is handed to the frame as the planes it already is rather than
  interleaved and separated again: a 3000x3000 eight bit page decodes in 43 ms
  rather than 139 ms, which is the chunky spelling's own time

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
