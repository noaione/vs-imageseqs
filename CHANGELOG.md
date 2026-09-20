# changelog

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
