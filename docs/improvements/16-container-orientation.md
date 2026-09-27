# 16 — Honor AVIF and HEIF container orientation consistently

Status: proposed; code-path gap confirmed, AVIF symptom reproduced on
2026-09-27. Priority: high. Extends [09](09-exif-orientation.md) and
[12](12-heif-avif-yuv-output.md).

## problem and evidence

`decoder::probe(path, apply_rotation)` returns early from the AVIF and HEIF
probes without passing `apply_rotation`. Both probes report
`Orientation::NoTransforms` and `Transform::IDENTITY`. The AVIF property reader
does not interpret `irot` or `imir`. The HEIF path decodes with default libheif
options, which its own comments identify as applying container transformations.
Thus the two native paths cannot consistently provide both display orientation
and the stored orientation selected by `apply_rotation=False`.

The local `target/investigation/irot.avif` still produced a 3x2 frame with
`ImgSeqOrientation=1` under the default rotation setting. Promote a verified
generator for this sample into the fixture suite before implementing the fix;
pixel placement and the declared rotation must both be asserted.

## intended change

- Establish the stored dimensions, container transform and any Exif orientation
  separately. Specify precedence and composition using container semantics and
  reference-decoder output; do not apply Exif and container transforms twice.
- Pass the rotation policy through both probes and decode paths. Either request
  untransformed libheif pixels and apply the shared transform, or explicitly
  account for transformations already performed by the native decoder.
- Make probe dimensions, frame allocation and decoded-plane checks agree for
  both policy settings. Keep color and alpha transformed together.
- Define how a container-only transform maps to `ImgSeqOrientation`. Document
  that choice before broadening the property's existing Exif description.
- Cover the native YUV/gray paths and RGB fallbacks. Check the existing
  subsampled-plane transform behavior and chroma-location metadata for rotations
  and mirrors; do not silently change sampling geometry to force a match.

The pinned [libheif decoding options](https://github.com/strukturag/libheif/blob/v1.23.1/libheif/api/libheif/heif_decoding.h)
are the starting point for deciding who applies the transform. Availability in
the Rust wrapper must also be checked. Do not add a second container parser
where the existing library can expose the information.

## validation

Use asymmetric labeled pixels with rotation, mirroring and combinations;
include non-square AVIF/HEIC, alpha, and both `apply_rotation` values. Assert
dimensions, exact plane contents, orientation properties, and `mismatch=False`
validation when a sequence mixes stored/display sizes. Use lossless fixtures
where exact comparison is expected. Preserve all existing PNG/JXL/WebP tests.

No orientation behavior is changed by this plan.
