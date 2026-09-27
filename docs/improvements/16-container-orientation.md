# 16 — Honor AVIF and HEIF container orientation consistently

- status: implemented
- touches: `src/formats/avif.rs`, `src/formats/heif.rs`, `src/decoder.rs`,
  `tests/make-orientation-fixtures.py` and 13 fixtures under `tests/fixtures/`
  (new), `tests/readalpha.vpy`, `README.md`, `AGENTS.md`, `CHANGELOG.md`,
  `docs/BENCH.md`, `docs/IMPLEMENTATION.md`, `docs/HANDOFF.md` and the index row
- depends on: [09](09-exif-orientation.md), whose `Transform` and
  `apply_rotation` this reuses, and [12](12-heif-avif-yuv-output.md), whose
  native decoders are the two paths that had to agree.
- expected: an avif or heif that states its orientation as `irot`/`imir` is
  handed out the way the file describes it, `apply_rotation=False` hands the
  stored picture back, and `ImgSeqOrientation` reports the code
- result: both containers state their orientation as two transformative item
  properties, which the plugin now reads and maps onto one of the eight exif
  codes — every combination of the two has an exact exif equivalent. An avif is
  handed out as its stored picture and the writer applies the transform, because
  `dav1d` returns the coded item; a heif keeps `libheif`'s own application for
  `apply_rotation=True` and hands `libheif`'s displayed picture back through the
  inverse transform when rotation is off, which is the shape
  [11](11-jxl-direct.md) gave the other decoder that applies a transform itself.
  The validator passes 423 checks (93 new, 13 fixtures), `cargo test` 139 (8 new),
  and all 120 `frame-parity.py` lines are byte identical because no file in the
  sandbox states a container transform. Reading the properties costs 0.2 ms per
  35 avif files and 1.3 ms per 35 heic files at clip creation, where the heic
  figure is the second open the container walk needs.
- risk: medium — it changes what a rotated avif or heif hands out and what
  `ImgSeqOrientation` reports for it, and it moves the width and height of every
  file whose container states a transform

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

## what it did, measured

Both halves of the defect are reproduced, and they are opposite halves. A heic
whose container states a quarter turn clockwise (`orientation-heic-rot-90.heic`,
`irot` 3) was handed out at the displayed size with the rotated pixels under
*both* settings, so `apply_rotation=False` could not reach the stored picture and
`ImgSeqOrientation` said 1 while the file said 6. An avif with the same statement
(`irot` 1, a quarter turn anticlockwise) was handed out as the stored picture
under both settings, because `dav1d` hands the coded item over and nothing
applied the property. The reference decoder agreed with neither of the two
reports for the avif and only half of the heic's:

| file | ImageMagick (libheif) | plugin before | plugin after, rotation on / off |
| --- | --- | --- | --- |
| `orientation-heic-rot-90.heic` | 3x4, `15,11,7,16,12,8,…` | 3x4, same pixels, code 1, both settings | 3x4 same / 4x3 stored |
| `orientation-avif-irot-1.avif` | 3x4, `10,14,18,9,13,17,…` | 4x3, stored, code 1, both settings | 3x4 same / 4x3 stored |
| `orientation-avif-irot-1-imir-0.avif` | 3x4, `7,11,15,8,12,16,…` | 4x3, stored, code 1 | 3x4 same / 4x3 stored |

**the transform had to be read from the boxes.** The plan prefers the library's
own answer over a second parser, so that was checked first: `libheif-rs` 3.0.0
exposes no rotation or mirror getter at all (`ImageHandle` has `width`/`height`
and `ispe_width`/`ispe_height` and nothing else geometric), the C functions that
would answer — `heif_item_get_property_transform_rotation_ccw`,
`heif_item_get_property_transform_mirror`,
`heif_item_get_transformation_properties` — all take a `heif_context` the
wrapper keeps private and does not re-export, and the only size-based inference
available tells a quarter turn apart from nothing else: a mirror and a half turn
leave the size alone, and no size says which way a quarter turn went. So the
plugin reads the two properties with the ISO base media file format item
metadata walker it already had for avif, through
`avif::container_orientation`, which is the one parser rather than a second one;
[17](17-avif-container-robustness.md) is where that walker's own bounds handling
is queued.

**the mapping, and why the property can always be filled.** `irot` states a
rotation in units of 90 degrees anticlockwise and `imir` a mirror whose axis
exchanges the top and bottom (0) or the left and right (1). MIAF
(ISO/IEC 23000-22 section 7.3.6.7) fixes the order they are applied in — clean
aperture, then rotation, then mirror — so the mirror is taken in the already
rotated frame, which is what makes some pairs share a code and others swap it:

| `irot` | no `imir` | axis 0 | axis 1 |
| --- | --- | --- | --- |
| 0 | 1 | 4 | 2 |
| 1 (90 ccw) | 8 | 5 | 7 |
| 2 (180) | 3 | 2 | 4 |
| 3 (270 ccw) | 6 | 7 | 5 |

Every combination is one of the eight exif codes, so `ImgSeqOrientation` is
filled for a container-only transform rather than left at 1. The mapping is unit
tested against the operations themselves rather than against the table: all
twelve pairs are applied to a labelled grid in the MIAF order and compared with
the grid the mapped code's `Transform` produces. That test caught a real bug —
the first version mapped a half turn with a mirror to the wrong axis, because
rotating half a turn and then exchanging the top and bottom is the same picture
as exchanging the left and right of the stored one.

A container transform is normative for both formats and an exif tag beside one is
informational, so nothing here reads an exif tag for an avif or heif and nothing
applies both. The two fixtures with an exif-like code and a container transform
are checked to agree, not to compose.

**who applies it.** `dav1d` returns the coded item, so the avif path keeps the
stored size and hands the writer the transform, exactly like the exif path of
[09](09-exif-orientation.md). `libheif` applies the container's own transforms as
it decodes and cannot be asked not to through the wrapper's size bookkeeping
without also losing the clean aperture, so the heif path keeps its displayed size
and identity for rotation on, and hands the displayed picture back through the
inverse transform for rotation off — the same shape [11](11-jxl-direct.md) uses
for jxl, which is the other decoder here that applies a file's transform itself.

**fixtures.** Thirteen, all encoded from one hand-written source
(`orientation-container.png`, the same four by three grid of twelve distinct
samples as the exif fixtures) by `avifenc` and `heif-enc`, with the exact
commands in `tests/make-orientation-fixtures.py`'s header: four rotations, both
mirrors, two mirror-and-rotation combinations, the identity-matrix file that
keeps the rgb path, a monochrome one, and three heic rotations. Each was checked
against ImageMagick's own rendering of the same file before it was committed,
which is what makes the expectations the container's and not this plugin's.

Validation:

- `tests/readalpha.vpy`: **423 checks pass** (93 new), against 330 before. Each
  fixture is checked for its format, its displayed size, the exact samples of the
  displayed picture, its `ImgSeqOrientation`, and then all four again for
  `apply_rotation=False`, plus the `mismatch=False` rejection of a rotated page
  beside an upright one and their agreement when rotation is off.
- `cargo test --locked`: **139 passed** (was 131). The eight new ones are the
  twelve-pair mapping, the reserved bits, the "no transform is orientation 1"
  case, the ten avif fixtures' probe fields, the stored picture a rotated avif
  decodes to, the unrotated file that must not move, and the two heic ones.
- `target/bench/frame-parity.py`: **120 of 120 data lines byte identical** — no
  file in the sandbox states a container transform, so nothing that used to be
  handed out differently changed. The 13 new fixtures are outside the parity sets,
  which select `alpha-*` under `tests/fixtures/`.
- probe cost, best of five rounds per build, per 35-file clip:

  | set | before | after |
  | --- | --- | --- |
  | avif | 1.7–1.9 ms | 2.0–2.1 ms |
  | heic | 6.2–6.7 ms | 7.5–8.0 ms |

  the avif figure is the two extra property payloads in a walk that already ran;
  the heic figure is the second open the container walk needs, which is 0.037 ms
  per file against the 0.15 ms per file [08](08-color-metadata.md) accepted for
  the same reason. The decode pass is untouched: `ab-sets.py` measured heic
  6175.0 → 6197.4 ms (1.004) with the sets this change cannot reach swinging
  ±20% run to run in the same batch, which is the noise floor of the machine
  rather than the change.

## deviations from the plan

- **the transform is read by the plugin, not by libheif.** The plan says not to
  add a second container parser "where the existing library can expose the
  information". The library cannot: the wrapper has no geometric getter and
  refuses its context, so the only routes are the raw C functions through a
  second `libheif-sys` context or the walker the plugin already has. The walker
  is reused rather than duplicated, and `avif::container_orientation` carries the
  reason where the call is made.
- **`apply_rotation=False` on a heif undoes the rotation and the mirror, not a
  clean aperture.** `libheif` applies `clap` together with `irot` and `imir`, and
  the plugin models orientation rather than cropping, so a heic whose clean
  aperture actually crops is handed back rotated-to-stored but still cropped. No
  file here has a cropping `clap` — the five in the corpus state the full
  aperture — so this is a limitation recorded rather than a case that was tested.
- **no heic mirror fixture.** `heif-enc` writes `irot` only; it has no mirror
  option. Mirroring is covered on the avif path, where `avifenc --imir` states
  it, and the heic path's own difference is only who applies the transform, which
  the three rotation fixtures cover.
- **the walker lives in `formats/avif.rs` and serves both containers.** A
  dedicated ISO base media file format module would be tidier; the extraction is
  mechanical and was left out of this change to keep the diff about orientation.

## validation

this is the plan's own list, as written before the change; the measured answers
are in "what it did, measured" above.

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
- Use asymmetric labeled pixels with rotation, mirroring and combinations;
  include non-square AVIF/HEIC, alpha, and both `apply_rotation` values. Assert
  dimensions, exact plane contents, orientation properties, and `mismatch=False`
  validation when a sequence mixes stored/display sizes. Use lossless fixtures
  where exact comparison is expected. Preserve all existing PNG/JXL/WebP tests.

## left over

- **a heic with a mirror, and a heic with a real clean aperture.** The first
  needs a writer that states `imir` (or a container rewritten by hand); the second
  needs the plugin to model cropping at all, which is a separate change and not
  an orientation one.
- **a container whose item boxes the walker cannot read falls back to reporting
  orientation 1** while `libheif` still applies whatever transform it states. The
  probe could say "unknown" instead, which the property has no value for; a file
  whose `meta` box this walker refuses is one
  [17](17-avif-container-robustness.md) would report as malformed instead.
- **the alpha item's own orientation is not read.** A heif alpha plane is
  transformed by `libheif` together with the colour, so the two agree; an avif
  alpha item is a second coded item with its own properties, and none of the
  fixtures states one with a transform.
- **`ImgSeqOrientation` is now one property for three sources.** It reports the
  exif code a file's orientation corresponds to, which is exact for all twelve
  container combinations and for the eight exif codes, but a file that states a
  container transform *and* a different exif tag has only the container one
  reported, because MIAF makes that the normative one.
