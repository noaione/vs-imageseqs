# 21 — Animated image sources

- status: proposed — decoder/API survey complete; implementation plan only
- touches: `src/source.rs`, `src/decoder.rs`, `src/clip.rs`, `src/prefetch.rs`,
  `src/formats/`, `tests/readalpha.vpy`, animation fixtures, README and
  `AGENTS.md`
- depends on: [18](18-demand-aware-decoding.md)'s per-call decode demand and the
  existing frame builder/prefetch path
- expected: an animated image in `files` contributes its displayed pictures to
  the same constant-frame-rate clip, with compositing, timing and alpha
  handled consistently; a still image continues to contribute one frame
- risk: medium to high — a single input path may expand to many frames, frame
  count and random-access behavior change for animated inputs, and format
  compositing can depend on earlier frames

## problem and evidence

The public contract and its implementation currently make one path equal one
output frame. `SequenceArgs::read` probes each path into one `ImageInfo`, takes
the number of images as `num_frames`, and `Prefetcher` indexes those same
`ImageInfo` records for each requested frame. This is a useful random-access
model for still images, but it cannot describe multiple presentations from one
file.

The existing format paths confirm that this is not just a missing decoder call:

- `src/formats/webp.rs::bitstream_header` returns `None` for `ANIM` and `ANMF`,
  and its tests assert that animated WebP does not enter the still-image path.
- `src/formats/jxl.rs` reads `BasicInfo` and decodes one image; it does not
  enumerate animation frames or frame durations.
- the generic decoder stores dimensions, format, orientation and color
  metadata on one `ImageInfo`, and `DecodedImage` represents one full image.
- `Read` and `ReadAlpha` share each decoded picture through one `FrameBuilder`,
  so an animation must preserve the existing one-decode-for-both-clips rule.

The frame API already has a constant `fpsnum/fpsden`; it does not expose
per-frame durations. To preserve an animation's playback time, the source needs
to map the animation's presentation timeline onto that output rate. Merely
enumerating encoded subframes would show partial rectangles, lose disposal and
blend behavior, and discard the timing the file provides.

## proposed behavior

- Keep `Read(files=[...])` and `ReadAlpha(files=[...])` as the entry points.
  Each still file contributes one frame. Each supported animation contributes
  its displayed timeline, in file-list order.
- Keep the output clip constant-rate at the requested `fpsnum/fpsden`. Map each
  animation's frame delays onto that rate with rational arithmetic: hold a
  displayed picture across output ticks when needed, and omit pictures that
  fall between ticks. Use the decoder/container's normalized delay values and
  document how a zero or missing delay is handled.
- Play each listed animation once. Do not repeat it based on its loop count;
  loop metadata describes repeat behavior for a player, while this source treats
  each input path as one finite timeline segment. A later API can add explicit
  repetition if callers need it.
- Emit full logical-canvas presentations after the format's blend and disposal
  operations. Preserve transparency for `ReadAlpha`, including clearing pixels
  when a frame's disposal requires it. The alpha clip remains opaque for a
  presentation with no alpha, matching still-image behavior.
- Keep animated frame dimensions and format stable within a source. The
  existing `mismatch` validation continues to decide whether segments with
  different dimensions or formats can share a clip.
- Continue to attach file-level metadata from the input animation to each
  output presentation. Do not invent per-frame color metadata or orientation;
  format-specific per-frame metadata can be added only if a supported format
  actually carries it and the property model can represent it.

The expansion and sampling rules need unit tests for exact boundaries. Define
the duration of an animation segment as the sum of its normalized presentation
delays, and its output frame count as the number of output sample instants in
that duration. A still path remains exactly one output frame regardless of
clip fps. Use checked integer/rational math for durations and total frame count;
reject a sequence whose expanded frame count exceeds VapourSynth's `i32` limit.

## decoder/API survey

The requested one-shot delivery is feasible with the current dependency set;
the formats do not need to be introduced as separate releases. The survey found
decoder support for this coordinated first implementation:

| format | API available in this checkout | consequence |
| --- | --- | --- |
| GIF | `image 0.25.10` implements `AnimationDecoder`; its iterator composites frames and disposal onto the logical canvas | Use the existing Rust decoder; retain its RGBA8 output, which matches GIF's sample depth |
| APNG | `image 0.25.10` exposes `PngDecoder::is_apng` and `PngDecoder::apng`; the APNG iterator composites blend/disposal and handles an excluded default-image thumbnail | Do not use this adapter for output because its APNG compositor is RGBA8-only; use `png 0.18` frame controls and compose at the source bit depth so 16-bit APNG remains 16-bit |
| animated WebP | `image 0.25.10` implements `AnimationDecoder`; the bundled `image-webp 0.2.4` decodes full-canvas frames with delays and alpha blending | Keep the current libwebp still-image route; recognize animation before that route and use the animation decoder, or add the libwebp demux/animation API only if measurement requires it |
| JPEG XL | current `jxl 0.7.4` exposes frame-only scanning, visible-frame durations and seek targets; its default coalescing composes visible presentations | Extend `src/formats/jxl.rs`; index presentation timing without retaining frame pixels and seek from the crate's frame checkpoints |
| AVIF and HEIF/HEIC image sequences | current `libheif-rs 3.0.0` exposes `has_sequence`, aggregate sequence timing, track IDs, track timescale and ordered `decode_next_image`; decoded images expose a duration field | It reads both generated sequence formats and decodes their visual tracks, but runtime checks found that AVIF's per-image duration field does not preserve varying `stts` entries. Use a checked sample-timing source for AVIF. |

The image crate's shared animation trait is a good fit for GIF and WebP, but its
APNG implementation explicitly composites through RGBA8 and says 16-bit
compositing is unsupported. The plugin currently preserves 16-bit still PNG,
so an APNG implementation must use the lower-level PNG frame API and compose
16-bit samples instead of silently narrowing them. The existing PNG `cICP`
reader remains the source of PNG color metadata.

JPEG XL has the strongest random-access API in the group: `scan_frames_only`
collects visible-frame timing and seek information, and `start_new_frame` starts
from the relevant checkpoint. The image crate's GIF/APNG/WebP interfaces are
stream iterators, while libheif tracks also expose only `decode_next_image` in
the wrapper used here. For those sources, deterministic random access therefore
needs replay from the start after a backward request unless a bounded checkpoint
cache is added.

HEIF/AVIF sequence decoding differs from the existing primary-image item path.
The current `src/formats/avif.rs` decodes one AV1 item with dav1d, while the
locked `libheif-rs` exposes ordered visual-track decoding. A visual sequence
track can supply color and its linked alpha track, but the crate wrapper does
not expose a timing-only sample index or sample count. `HeifContext` provides an
aggregate sequence duration without decoding images.

References checked against the locked versions and upstream API documentation:
[image 0.25.10 `AnimationDecoder`](https://docs.rs/image/0.25.10/image/trait.AnimationDecoder.html),
[image 0.25.10 `PngDecoder`](https://docs.rs/image/0.25.10/image/codecs/png/struct.PngDecoder.html),
[jxl 0.7.4 decoder API](https://docs.rs/jxl/0.7.4/jxl/api/decoder/struct.JxlDecoder.html),
[libheif sequence reading](https://github.com/strukturag/libheif/wiki/Reading-and-Writing-Sequences),
and the [WebP animation decoder API](https://developers.google.com/speed/webp/docs/container-api).

## proposed implementation direction

Deliver GIF, APNG, animated WebP, animated JPEG XL, AVIF image sequences and
HEIF/HEIC image-sequence tracks together behind one animation-source interface.
Do not split these into format-by-format releases. Multi-page TIFF is not part
of this animation contract: TIFF pages are document pages and have no standard
frame-delay timeline. ICO entries are likewise alternatives, not timed frames.
If a caller needs those as page sequences, that should be specified separately.

Use a compact segment table rather than expanding one descriptor for every
output tick. Each listed still has a duration of one output tick; each listed
animation has one traversal of its presentation timeline, and the next file
starts immediately after that segment. Ignore format loop/repetition counts so
the finite VapourSynth clip never repeats an input path implicitly. Represent
durations and sampling instants as checked rational/integer values, not floating
point milliseconds. Output frame `n` samples the segment presentation active at
the segment-relative time `(n - segment_start) / fps`; the output count is the
number of sample instants before the segment ends. Normalize zero or missing
per-frame delays according to each format's
decoder/spec behavior and cover those rules with fixtures.

Keep one decoder/compositor cursor and one last presentation buffer per active
animation source. A forward request advances the cursor and composites any
intervening source frames; a repeated sample reuses the last presentation; a
backward request resets and replays from the beginning. JPEG XL uses its
indexed seek targets instead. This bounds decoded-pixel memory independently
of animation length and preserves deterministic arbitrary requests. Adapt
prefetching so requests within one animation are decoded in timeline order on
one worker, while different files can still decode concurrently. `Read` and
`ReadAlpha` continue to share the same completed VapourSynth frame payload.

This trades some cost for backward random seeks on GIF/APNG/WebP/AVIF/HEIF for
a simple memory bound and a fast sequential path. Measure both before adding
per-animation checkpoints or an LRU of full canvases. Never collect every
decoded presentation during clip creation or hold all animation frames in RAM.

## implementation stages

### 1. Describe timelines without decoding full pictures

Add an internal frame-source model that separates an input path from an output
frame. A sequence entry retains shared file-level metadata and identifies a
still image or an animation segment. Resolve output frame numbers through the
compact segment table to an input path and target presentation time, rather than
duplicating metadata or profile buffers for every held output tick.

Add duration normalization and timeline mapping tests first. Cover exact fps
matches, delays shorter and longer than one tick, fractional frame rates,
multiple animation/still segments, zero/missing delays, checked overflow and
loop metadata ignored. This stage must leave still-only frame counts, formats,
pixels and timing unchanged.

### 2. Implement all animation adapters in one change

Implement GIF, APNG, WebP, JPEG XL and HEIF/AVIF track adapters together behind
the shared animation-source interface. Each adapter reports canvas dimensions,
source timing, format metadata and alpha behavior, then returns deterministic
full-canvas presentations. Preserve format depth, especially 16-bit APNG and
the existing 10/12-bit AVIF and HEIF paths. Still files continue to use their
current decoders and contribute one frame.

### 3. Document and validate the public contract

Document that animated inputs expand to one finite timeline segment, use the
clip's output fps, and play once. Update the API description and changelog when
implementation lands. Add tiny committed fixtures for frame delays, alpha,
partial-frame blending, disposal-to-background and restore-previous behavior,
plus still/animation ordering. Extend the VapourSynth validator to check frame
count, frame pixels/alpha, random requests, `mismatch`, and preservation of
still-image behavior.

## acceptance checks

- A `files` list mixing stills and supported animations produces the documented
  expanded frame count and ordering at several fractional and integer frame
  rates.
- Every output frame matches the format's fully composited logical canvas;
  alpha is correct before and after disposal operations.
- Sequential, reverse and shuffled requests produce the same frame hashes.
- `ReadAlpha` and `Read` share decoded presentation work, and a color-only read
  does not request a separately coded alpha item where the format permits it.
- Existing still-image fixtures retain their current frame count, frame bytes,
  output formats, metadata and `mismatch` validation.
- Peak memory stays within the chosen bounded policy for a long animation, and
  sequential throughput is measured against the decoder's direct sequential
  path. Include the per-frame replay and prefetch-worker cost in the report.
- `cargo test --locked`, `tests/readalpha.vpy`, and the relevant read benchmark
  pass without errors or warnings.

## remaining implementation checks

- Read AVIF `stts` entries or use another timing API: `libheif-rs` 3.0.0's
  `Image::duration()` returned 80 ticks for all four AVIF frames, while
  `avifdec` and the file's `stts` box report 80/170/110/240. Aggregate sequence
  duration is correct at 600/1000 seconds, but it cannot locate presentation
  boundaries by itself.
- Account for HEIF track clean-aperture cropping: this 16x12 HEIC track reports
  16x12 through `Track::image_resolution()`, while `decode_next_image()` returns
  a 64x64 raster and the file describes a crop to 16x12. Track output must be
  cropped/normalized to the logical canvas before entering the shared frame
  builder.
- Decide how to count source frames without decoding every full-resolution
  frame. The `libheif-rs` wrapper exposes total duration and track IDs, but no
  sample count or per-sample timing table; ISO BMFF sample tables can provide
  that metadata without decoding coded pictures. JPEG XL's scan-only path
  already returns visible frame timings and seek targets without rendering.
- Implement and test 16-bit APNG composition and color metadata. The generated
  16-bit APNG decodes through `png 0.18.1` as RGBA16 and exposes frame timing,
  blend/disposal controls and subframe rectangles; the lower-level decoder
  returns raw subframes, so composition remains our responsibility.
- Benchmark sequential playback and backward seeks for every sequential-only
  decoder under the proposed ordered prefetch scheduling.

## generated fixtures

`tests/make-animation-fixtures.py` generates a four-frame, 16x12 animation in
`tests/fixtures/animation.{gif,png,webp,jxl,avif,heic}`, plus a two-frame 4x3
16-bit RGBA APNG at `tests/fixtures/animation-rgba16.png`. The image frames have
transparency, moving regions and distinct color markers; GIF and APNG carry
disposal/blend metadata. GIF, APNG, WebP, JPEG XL and AVIF use delays of 80, 170,
110 and 240 ms. The HEIC sequence has four 150 ms frames because the installed
`heif-enc` command-line encoder accepts one duration for the whole sequence.
The script requires Pillow, `cjxl`, `avifenc` and `heif-enc` on `PATH`; it keeps
intermediate PNGs in a temporary directory.

The installed `image 0.25.10` animation iterators returned all four frames and
the expected delays for GIF, APNG and WebP. JPEG XL's `scan_frames_only` path
reported the expected four delays without rendering: 80/170/110/240 ms. The
generated AVIF reports four frames and a 600 ms timeline through `avifdec`, the
HEIC sequence reports the same frame count and total duration through
`heif-info`, and JPEG XL decodes back to a four-frame APNG through `djxl`. These
fixtures establish real encoded inputs for the remaining sequence API and
timing checks; they do not change the still-image implementation.

Do not add a loop-count, duration override, or alternate animation filter
argument until a concrete caller needs it. The supported formats land together;
these checks close backend and performance details, not the format rollout
scope.
