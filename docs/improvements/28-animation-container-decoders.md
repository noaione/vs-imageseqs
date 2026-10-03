# 28 - animation discovery and container fallbacks without image-rs

status: proposed, research only. part of [26](26-remove-image-rs.md). No timeline,
compositor or container parser has been changed.

## the remaining full-decode discovery pass

`src/animation/frames.rs` uses image-rs for GIF and animated WebP. Its
`FrameSource::durations` consumes every fully decoded/composited `Frame` to
collect delays and check canvas sizes, then drops the pictures. Playback builds
a new frame iterator and decodes those presentations again. A one-picture GIF
also pays that discovery pass before being declined as an animation and read
through the generic still path.

This is a concrete instance of the user's double-decode concern. It is separate
from the bounded replay required for requests that go backward beyond
`AnimationSource`'s presentation window. It also decodes frames the clip's
constant-rate sampling may never select. Removing image-rs is an opportunity to
read the timeline without rendering it, not permission to keep all decoded
frames indefinitely.

## GIF plan

Use the already underlying `gif 0.14.2` directly. Its inspected `DecodeOptions`
has `skip_frame_decoding(true)`, which skips LZW pixel decoding while permitting
frame metadata traversal. Build the timeline from frame delays and logical
screen/subframe rectangles in that pass, also checking palettes, extensions,
transparency, ICC presence and bounds. Confirm that the scan consumes the whole
stream safely without producing decoded canvases. See the
[GIF decoder API](https://docs.rs/gif/latest/gif/struct.Decoder.html) and the
versioned [source](https://docs.rs/crate/gif/0.14.2/source/src/reader/mod.rs).

During playback, decode indexed/RGBA subframes and compose a logical canvas in
project code. The low-level `gif` frame is a subframe, not image-rs's displayed
canvas. Replacing the import alone loses essential behavior:

- Honor offsets, local/global palettes, transparent indices and interlacing.
- Preserve keep/background/previous disposal and its order relative to
  publishing a presentation. Preserve the existing transparent-canvas behavior
  rather than substituting a GIF background color without a separate decision.
- Bound saved previous-canvas or previous-rectangle storage and account for it
  alongside the presentation window. Do not mutate a cached presentation while
  composing its successor.
- Keep still one-picture behavior, including partial canvases, and make alpha
  correct for both the color-only and `ReadAlpha` requests.

Reuse `Presentation`, `Rate`, `SegmentInfo` and the project's `AnimationDecoder`
trait rather than recreating image-rs `Frame` and `Delay`. Keep sequential state
and restart/checkpoint as necessary for backward reads. Skipping an output tick
does not mean skipping a subframe that a later displayed picture depends on.

## animated WebP plan

Still WebP pixels already use libwebp. Investigate linking its demux component
and using `WebPDemuxGetFrame` for ANMF rectangles, delays, blend/dispose flags
and frame count without decoding pixels. Read EXIF/ICCP through container chunks
and preserve metadata behavior. Compare two playback integrations:

| candidate | benefit | work and constraints |
| --- | --- | --- |
| libwebp `WebPAnimDecoder` | supplies composed RGBA canvases, avoiding a new WebP compositor | library owns the returned buffer, which must be copied or safely consumed before the next call; replay/reset and RGBA ordering must be checked |
| demux + existing still decoder + project compositor | more control over demand, temporary storage and canvas ownership | must implement blend/dispose and offsets correctly; frame fragments need validation and lossy YUV subframes do not make the composed RGB canvas a YUV clip |
| direct `image-webp 0.2.4` | keeps the current underlying animation decoder for a parity-first migration | audit allocations, compositing and metadata scan costs against native demux/animation, keeping it only if measurements justify it |

The [WebP container API](https://developers.google.com/speed/webp/docs/container-api)
documents demux metadata and composed-animation decoding. The
[image-webp decoder API](https://docs.rs/image-webp/latest/image_webp/struct.WebPDecoder.html)
offers a second direct candidate; pin any adopted version and inspect its
constructor and frame-read behavior before assuming metadata access is cheap.

Animated WebP currently yields composed RGBA buffers to the existing output
writer. Preserve that output contract. Keep lossy opaque **still** WebP on its
native YUV route. Check one-frame animation containers explicitly rather than
letting them fall between the demux and still readers.

`WebPAnimDecoder`/demux are not provided merely by naming libwebp's decode
archive. Future implementation must inspect `build.rs`, vcpkg, manylinux and
Homebrew linkage for the demux library and its transitive inputs, then update
notices, exact licenses and wheel checks as required. This research changes none
of those files.

## other animations

APNG already composes directly over `png`, including 16-bit channels. Keep its
blend/dispose precision and audit the `decoder::probe` and still-poster routes
that still use image-rs. JXL already scans headers and decodes presentations
from checkpoints. AVIF/HEIF track adapters already read sample tables and clean
apertures in project code. They need shared-type migration, not an unmeasured
rewrite of their decoders.

Preserve one play regardless of loop count, exact constant-rate sampling,
zero/absent delay as one output tick, default `24/1`, checked arithmetic, and
the bounded presentation window. Count decoder replay and cache hits separately
from the avoidable discovery decode. Do not combine this work with the deferred
JXL decoder-continuation experiment described in [BENCH.md](../BENCH.md).

## AVIF still fallbacks

`formats/avif.rs` deliberately uses `Meta::native_eligible` to keep probe and
decode consistent. The direct reader does not cover every container image-rs
can decode. Monochrome AVIF still uses the generic decode and is corrected to
gray; containers with unsupported item layouts retain fallback behavior.
The image-rs AVIF constructor itself decodes the picture and linked alpha, so
a fallback constructor can render during probing, or eagerly decode unwanted
alpha during a color-only request.

First make a route inventory for the existing monochrome and split-extent
fixtures, then add currently accepted grid, extent, construction-method and
matrix cases that are absent from the fixtures. For each, record whether the
current probe is container-only or constructs the image-rs decoder. Do not
assume that every unsupported item arrangement is handled by either backend.

Evaluate a project-owned compatibility adapter using the existing
mp4parse/dav1d behavior, or direct libheif support where available. Extending the
existing bounded item walker is another choice, but grids, extent assembly,
construction methods and RGB conversion are separate obligations. None may be
treated as completed because a simple primary item decodes.

Preserve native YUV output only on the routes that currently expose it. For a
fallback RGB route, preserve conversion, matrix/range interpretation, stored
word and alpha behavior. A different RGB conversion can change samples even
when the source is lossless. Retain monochrome depth corrections and container
`irot`/`imir` orientation. Keep path-qualified errors, checked file/idat bounds
and the low-latency no-picture behavior from [15](15-avif-decoder-progress.md)
and [17](17-avif-container-robustness.md).

## HEIF RGB fallbacks

`formats/heif.rs::handles` accepts non-RGB output. RGB pages are still decoded
through `libheif_rs::integration::image`. The inspected integration constructs
a primary handle and describes it without pixel decoding, then requests RGB
and copies libheif's interleaved rows into image-rs's caller buffer. The plugin
subsequently writes that buffer into VapourSynth planes.

Replace that hook with direct libheif calls in the existing format module. Use
its RGB planes/interleaved output as supported, avoiding an unnecessary owned
copy when lifetimes permit. Preserve the fallback's stored word depth, row
stride, color conversion and alpha, alongside the existing native gray/YUV
paths. Container transforms may already be applied by libheif: use the project's
current transform/inverse-transform policy so orientation is applied exactly
once and `apply_rotation=False` still returns the stored picture.

Audit ICC and color metadata availability on each route rather than assuming
that the old image integration exported everything the direct handle can read.
Changes to public properties beyond parity need explicit scope. Only after all
HEIF routes are direct can the Cargo `image` feature and process-global hook
registration be removed. See the [libheif-rs API](https://docs.rs/libheif-rs/3.0.0/libheif_rs/).

## correctness and performance gate

Extend the release validator with mixed-size canvases, partial rectangles,
palette/transparency changes, every disposal/blend operation, background/previous
restoration, interlaced GIF, opaque/translucent WebP, one-presentation files,
loop counts and zero/absent delays. Check sampling boundaries at integer and
rational rates, both output clips, and reverse/shuffled requests exceeding the
presentation window. Exercise alpha-first/color-first requests, lookahead and
multiple clips without corrupting composed state or duplicating shared work.

Benchmark the current image-rs baseline and each candidate using [26](26-remove-image-rs.md)
and [BENCH.md](../BENCH.md). Include long/high-resolution animations as well as
the tiny committed fixtures. Measure discovery/open time separately from
playback and confirm metadata scanning performs no pixel decompression. Record
CPU, first-frame latency, all playback orders, peak memory, saved compositor
state and decoded presentation counts. Keeping the entire animation to remove a
second decode is not an acceptable unbounded tradeoff.

The original research-session validator stopped on `animation.avif` because
Windows libheif had no matching AV1 decoder. The separate build fix in
[30](30-windows-avif-sequence-decoder.md) enables its existing dav1d backend and
the full release validator now passes. This does not implement an image-rs
migration. Take a fresh baseline from the repaired build before a future
migration can be accepted.
Investigate and fix significant speed or memory regressions, and prove a
measured improvement over image-rs before claiming one.
