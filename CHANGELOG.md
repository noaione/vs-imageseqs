# changelog

## unreleased

### added

- `ImgSeqAnimationIndex` names the displayed picture's position within its own file, and is written on frames from an animated file
- `core.imgseqs.PNGWrite(clip, output_path=...)` writes the frames a graph asks for as separate PNGs: a request writes that frame before it returns, and a frame nobody requests is never written, so creating the node writes nothing
  - automatically convert YUV clip to RGB following the frame's own `_Matrix` and `_Range`. Matrcies are converted for non-constant-luminance ones, and a constant-luminance, SMPTE 2085 or chromaticity-derived frame is refused by name. The conversion is vectorized where the processor has one to run it on: AVX2 or AVX-512 on x86-64, and NEON on aarch64, which every aarch64 has.
  - a float clip is a value in `[0, 1]`, which becomes a sixteen bit PNG or the word `depth` names: a value outside the range is clamped and a NaN is written as the smallest sample
  - `depth` names the word the PNG stores. 8 gives up precision by rounding, 16 widens an eight or ten bit frame exactly, and 1, 2 or 4 pack a gray frame, which writes back the picture a low bit reader expanded. A frame of nine to fifteen bits is stored as a sixteen bit PNG whose high bits hold the source precision, with an `sBIT` chunk saying how many are meaningful, so the widening is exact rather than a scale
  - `output_path` is a filename for a one frame clip or a `%d`/`%06d` template, numbered by the writer's own frame index plus `start_number` rather than any index a trim or a splice moved. `compression` is 0 to 9 and lossless at every level, `always_save` writes a frame again instead of skipping one this instance already published, and `overwrite` is the separate permission to replace a destination that is already there
  - a write goes to a temporary file beside the destination and is published with a rename or a linking create, so a failure leaves no partial file and two writers cannot both claim a path. `ImgSeqPNGWritePath`, `ImgSeqPNGWriteSaved` and `ImgSeqPNGWritePerformed` report what happened, and `cICP` and `icc_profile` carry the colour the frame states rather than inventing one
  - a consumer that keeps several frames in flight (`frames(prefetch=n)`, open `get_frame_async` futures, `vspipe`) gets several encodes at once, and **`zlib-rs` compresses 1.46x faster than flate2's default fallback into 7.5% smaller files**, which at the default level is Pillow's own size for the same picture

### changed

- **The `image` crate is gone.** Every format this plugin reads has a reader of its own under `src/formats/`, and a file whose bytes and extension both name no format here is refused with `no reader here knows its format`. The plugin is about 435 KB smaller, and several of those readers are ports of that crate's, so its licence texts and notices stay
  - `webp`: a still is decoded by `wpd`, with libwebp gone from the link, and described from its own chunks, so `ImgSeqOrientation` and `ImgSeqHasICC` come from the file. The still path is **1.2x to 1.3x faster**, the Windows library 134 KB smaller, a corrupt file is refused with the decoder's own words, and building on x86 or x86-64 still needs `nasm` on `PATH`
  - `tiff`: a WebP- or JPEG-compressed strip is decoded here, and a compression code this reader does not take is refused by name at the probe
  - `tiff`: one, two or four bit gray, gray plus alpha, cmyk (with alpha for the first time), palette and ycbcr pages are all read now, and a ycbcr page comes out as its own yuv planes with `_Matrix` and `_Range` from its tags
  - `tiff`: a page that states an orientation, carries an ICC profile or is a BigTIFF now reports all three
  - `bmp`/`ico`: the older `BITMAPCOREHEADER` and a bare `.dib` are read, and a Windows cursor is read instead of having its type word mistaken for a bit depth
  - `tga`: a colour map whose entries are fifteen or sixteen bits is read rather than refused, widening by the same round-to-nearest table a direct sixteen bit image uses
  - `hdr`: a radiance picture whose resolution line states an orientation other than the common `-Y ... +X` decodes
  - `exr`: a first part with no colour channel decodes, and a flat grayscale layer is read with `Y` as colour and `A` as the alpha clip
  - `jp2`: a file that states what its components mean is read as what it states -- gray or r,g,b beside an opacity component -- and a palette is expanded here rather than refused
  - `png`: a file is identified by its bytes on the probe side too, so a name that lies about it gets the same description and the same `cICP` properties
  - `gif`: a one frame gif is read as a still image
- Whether a file plays a timeline is decided by its bytes now, like its format already was, so a gif, webp, avif or heic under a name that does not say so plays its timeline. `identify::route` decides that and whose file it is in one call, so a file is opened once to identify it rather than about fifteen times, at a cost of 3% more per file at clip creation over the fixture set
- An avif or heif is read by the library its container states, through one walk the probe and the decode share
  - An item written as several extents is joined here, a grid of tiles goes to `libheif`, and an r,g,b page is read by `libheif` too; one with no alpha now reports `ImgSeqOriginalColorType=rgb8`
  - A sequence honours the `ctts` composition offsets and an edit list that ends its media early, clamping a sample composed before zero and refusing by name an edit list that starts partway into the media
  - A twelve bit subsampled page is handed out as `YUV420P12` or `YUV422P12`
- A PAM whose `MAXVAL` is above a byte but which names no `TUPLTYPE` is read at the width its `MAXVAL` states

### fixed

- `ImgSeqIndex` reports the path's position in `files` rather than the output frame number, which an animation pushed out of step for every file after it
- A signed JPEG 2000 page, or one whose components state two widths, is refused with a message that names the component, its width and what is wrong with it
- The musllinux wheel no longer crashes on a TIFF (or an AVIF with no picture) read on a VapourSynth worker thread: each format's `decode` and `stream` is a call of its own, so the dispatcher no longer grows a 100 KiB frame past musl's 128 KiB stack
- Windows builds enable libheif's built-in dav1d decoder, so animated AVIF sequences decode instead of failing with `NoMatchingDecoderInstalled`; libheif's default features stay disabled
- An animated png's delays are placed exactly, at the lowest common denominator of the delays the file states, rather than rounded to whole milliseconds. An animation whose length is not a whole number of output frames keeps its last frame, so 600 ms at 24 fps is 15 frames rather than 14

### performance

- Creating a clip over a long list reads the front of each file where the container states what a description needs there: 35 webp files of 146 MiB went from 99 ms to 6 ms, 35 jpeg xl from 157 ms to 5 ms, 35 jpeg 2000 from 159 ms to 4 ms, and 35 avif plus 35 heic of 362 MiB from 84 and 170 ms to 6 and 13 ms
- Describing an animated png reads the delays its `fcTL` chunks state instead of rendering every frame: five 1024x1024 files of eight frames each went from 49 ms to 3 ms
- PNG decoding hands each row to the frame it belongs in rather than buffering the whole picture, and a palette page is expanded here rather than by the decoder. Every png set measured is now ahead of Pillow's own decode column, from 0.89x to 0.69x of its time
- A netpbm, a targa and a bitmap are read a row at a time rather than into a buffer the size of the file, holding 48 MiB rather than 74 MiB for a 3000x3000 image and decoding 7 to 14% faster
- A planar tiff is handed to the frame as the planes it already is, which is 43 ms rather than 139 ms on a 3000x3000 eight bit page
- A DirectDraw surface whose width or height is not a multiple of four is read with the pixels that hang over the edge dropped, and a netpbm whose header is longer than the readable window is read to a megabyte

### build

- the Linux release includes a musllinux wheel beside the manylinux one, for hosts whose C library is musl rather than glibc, where the manylinux wheel cannot load at all
  - it carries the same three plugin libraries, so an AVX2 or AVX-512 host is served there the same way
  - auditwheel's musl policy promises the host only libc and libz, so it bundles the C++ runtime the embedded libheif needs instead of needing a package Alpine does not install by default
  - it is accompanied by `linux-musl-relink-source.tar.gz`, the corresponding source archive the manylinux wheel already has
- x86-64 wheels carry one plugin library per microarchitecture level (`avx2` and `avx512`) and VapourSynth loads the widest the CPU supports, which is 6% to 11% faster on the PNG and JPEG sets measured at the cost of a 10.4 MiB Windows wheel instead of 3.8 MiB

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
