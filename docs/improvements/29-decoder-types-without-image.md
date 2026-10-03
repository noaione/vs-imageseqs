# 29 - decoder types without image-rs intermediaries

status: proposed, research only. part of [26](26-remove-image-rs.md). This stage
should preserve every pixel, frame property and argument; it is not itself a
measured speed optimization.

## the dependency surface actually used

The plugin already owns `ImageInfo`, `DecodedImage`, `Pixels`, `Demand`,
`DecodeTimings`, `RowStream`, `RowSink`, `PixelFormat`, `Transform`, `Cicp`,
`ImgSeqError`, `Rate`, `Presentation`, `SegmentTable` and `AnimationSource`.
They cover most of the needed integration. The remaining image-rs surface is:

| image-rs item | current consumers | proposed disposition |
| --- | --- | --- |
| `ColorType` | `ImageInfo`, interleaved `Pixels`, pixel dispatch, format adapters and APNG | a small project-owned decoded layout; map codec enums once inside each adapter |
| `ExtendedColorType` | `ImageInfo.original_color_type`, tests and `color.rs` property serialization | small source-description representation with explicit stable property labels |
| `metadata::Orientation` | `ImageInfo`, `Transform`, inverse orientation, JXL and ISO container mapping, tests | project-owned eight-way orientation code plus existing `Transform` |
| `ImageReader`, `ImageDecoder` | generic probe/decode and GIF/WebP identification | project format identification and direct adapter calls, retaining the existing result types |
| `ImageFormat` | expected GIF/WebP identity checks | small format-kind enum or per-format signature checks shared with dispatch |
| `AnimationDecoder`, `Frame`, frame iterator / `ImageResult` | `animation/frames.rs` | existing project animation trait and `DecodedImage` over owned buffers, with project `Result` |
| `Delay` and its ratio accessor | indirect through `Frame::delay().numer_denom_ms()` | map container delays directly into existing `Rate` / `Presentation` |
| `WebPEncoder` | lossless fixture construction in `formats/webp.rs` tests | existing native libwebp lossless encode API or a reviewed direct dev-only encoder |
| libheif decoder hooks | `decoder.rs::register_decoder_hooks` and Cargo's libheif `image` feature | remove after [28](28-animation-container-decoders.md) replaces RGB/fallback decode |

Production code does not directly use `DynamicImage`, generic image resizing,
`GenericImageView` or image-rs color transforms. Do not design replacements for
APIs the plugin does not call. Its current image buffer wrapper is primarily the
animation `Frame`, which becomes raw ownership at `into_buffer().into_raw()`.

## reuse the output representation and separate the input layout

Keep `PixelFormat` as the **VapourSynth output format**. It already names gray,
RGB and subsampled YUV at their actual nominal depths. It cannot alone describe
an RGBA16 interleaved decoder buffer: the color clip is three planar channels
while the buffer has four channels stored as sixteen-bit words.

Use a compact decoded-layout enum for the actual variants currently consumed:
L8, LA8, RGB8, RGBA8, L16, LA16, RGB16, RGBA16, RGB32F and RGBA32F, or an
equivalent validated channel/sample description. Its helpers should state
sample word size, channel order/count and inline-alpha position. Use checked
size arithmetic and explicit byte order at the adapter boundary. Keep
`Pixels::Planar` and `Pixels::Stream` for native planes and rows rather than
forcing them through an interleaved type.

Distinguish four facts that image-rs types currently help carry:

- Source encoding/type, including palette, packed gray or CMYK where reported.
- Decoded buffer layout, including inline alpha and stored word size.
- Output `PixelFormat`, including nominal nine-to-fifteen-bit depth and YUV.
- File alpha presence versus the call's `Demand::alpha` and produced alpha data.

The zune candidates in [27](27-direct-still-decoders.md) already share
`zune-core::ColorSpace`, `BitDepth` and `DecodingResult`. Reuse those existing
types inside the zune adapters instead of duplicating a JPEG/BMP/QOI/PNM/HDR/
farbfeld enum for each codec. Map them once into the project boundary used by
all the other backends. `ColorSpace` and `BitDepth` are non-exhaustive and include
unsupported/unknown cases: reject or explicitly handle those, including BGR,
BGRA and arbitrary channel counts. A zune sample-word type still does not state
the file's nominal depth, the public source label or the frame's output format.
Using a shared backend enum must not reintroduce an umbrella image buffer or
change `ImgSeqOriginalColorType` to zune's debug spelling.

A color-only AVIF decode may omit an independently coded alpha item without
making the file's source metadata say it never had alpha. A nominal ten-bit JXL
may still arrive in sixteen-bit words that the existing writer shifts down.
Do not collapse those distinctions when removing enums.

## preserve original color-type labels

`src/color.rs` currently writes `ImgSeqOriginalColorType` using the debug name
of `ExtendedColorType`. That string is public behavior. Preserve labels such as
`L1`, `L8`, `La8`, `Rgb8`, `Rgba16` and `Cmyk8` exactly where the current probe
reports them. Capture every label from the existing format/fixture corpus and
the accepted-subtype audit before deleting the old type.

Use a source-type enum with an explicit label serializer, or a validated
description and compatibility-label mapping. Do not rely on the debug name of
a newly chosen codec enum, which can differ by backend or upstream version.
Do not assume the original color type is identical to the decoded layout:
palettes, low-bit gray and CMYK conversions are the important counterexamples.
Existing native probes sometimes derive it from their decoded type; preserve
that current property behavior in this stage instead of silently redefining it.

Duplicate only the source cases necessary for compatibility, plus a deliberate
unsupported/unknown policy. A new decoder must not describe an unsupported
encoding as RGB simply because that is a convenient default.

## orientation and metadata

Keep an eight-way file orientation with explicit EXIF codes 1–8 and a checked
conversion from external values. Reuse `Transform` to apply it. Keep the file
code separately when `apply_rotation=False` chooses `Transform::IDENTITY`, so
`ImgSeqOrientation` still reports the source and orientations 5–8 still swap
output dimensions when applied. Avoid a new dependency merely for eight enum
variants.

JPEG/PNG/WebP direct readers will need the EXIF orientation extraction their
image-rs decoders supplied. Evaluate a small shared bounded TIFF/EXIF tag
reader or a focused EXIF crate at implementation time. Preserve missing,
invalid, truncated and endian behavior without rendering the image. Reuse the
ISO item walker for HEIF/AVIF `irot`/`imir`; do not create a second container
parser or give EXIF precedence over those normative item properties. JXL maps
its own orientation into the same local type.

Keep `Cicp`, existing VapourSynth color enums, ICC `Arc<[u8]>`, optional ICC
retention, chroma-position mapping and project errors. There is no need for an
intermediate image-rs color enum once each container has reported its codes.
ICC export remains raw bytes without a transform and reaches both `ReadAlpha`
clips; unspecified/unrecognized color codes still leave properties unset.

## identification, results and lifetime boundaries

Map backend errors with `image_error` / `ImgSeqError` and keep path-qualified
messages. Replace `ImageResult<Frame>` with the project's `Result<DecodedImage>`
and the animation decoder trait already used by APNG/JXL/HEIF. Do not clone a
whole image-rs error hierarchy or create a new umbrella image trait solely to
rename `ImageDecoder`.

Format identification must preserve accepted extension aliases, uppercase
extensions, signature guessing and behavior for mismatched/absent extensions.
Current direct adapters often choose by extension while `ImageReader` can
guess from content. Audit these actual routes before centralizing them. Give
probe and decode one consistent format identity, with explicit subtype routing,
so one cannot promise native YUV while the other falls back to RGB. Test files
whose content changes after probing and preserve bounded failure behavior.

Backend enum mappings belong inside `src/formats/` and `src/animation/`.
The shared frame/cache/prefetch code should see only project representations.
Use native typed state with valid ownership/`Send` requirements rather than
copying the erased image-rs iterator and its manual `unsafe impl Send` into the
new design. A mutex is not proof that any foreign object may move between
threads. Streams and borrowed native planes must remain valid until the writer
has consumed them, and animation buffers must not change under cached frames.

## migration and checks

First introduce local representations and conversions around the image-rs paths
that remain, then migrate adapters one by one. Remove temporary conversions and
the image-rs test encoder only after [27](27-direct-still-decoders.md) and
[28](28-animation-container-decoders.md) provide all replacements. If any code
is copied from upstream, record its license/provenance and preserve notices.

Require exhaustive mapping checks, source-label parity, every orientation code
and dimension transform, word-depth shifts, alpha demand, float bit behavior,
and metadata parity on actual frames. Run relevant Rust checks and the release
validator before and after implementation. `rg` must find no remaining imports,
qualified calls or documentation links that require image-rs types when removal
is complete, and normal/build/dev dependency graphs must exclude it, including
the libheif integration feature.

Use [26](26-remove-image-rs.md)'s baseline protocol and [BENCH.md](../BENCH.md)
even for the type migration. Compare byte-identical outputs and stable properties
against the image-rs baseline. This stage should be performance-neutral; later
decoder/ownership changes must demonstrate their claimed speed and memory
improvements. Investigate and fix significant regressions before landing. No
implementation or dependency removal is authorized by this plan.
