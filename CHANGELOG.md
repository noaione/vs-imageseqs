# changelog

## unreleased

### fixed
- a frame request on an AVIF whose item holds no coded frame no longer hangs
  forever; it fails with a decode error naming the file
  - the item decoder now runs at low latency, which is what lets "the decoder has
    all of the item and has no picture" be told apart from "the picture is still
    being decoded"

### performance
- AVIF decoding is 15.6% faster on the 35-page sandbox set, and 43% faster on
  `sandbox/hitokage-sample`: a still image is one frame, so the item is decoded
  directly instead of through a frame-delay pipeline it cannot use

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
