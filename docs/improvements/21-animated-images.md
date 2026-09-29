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
| AVIF and HEIF/HEIC image sequences | current `libheif-rs 3.0.0` exposes `has_sequence`, aggregate sequence timing, track IDs, track timescale and ordered `decode_next_image`; decoded images expose a duration field | The embedded libheif 1.23.1 build reports the wrong AVIF sample durations, while system libheif 1.23.5 reports the expected values. HEIC track decoding returns coded pixels without applying the sequence's presentation crop. |

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

The bundled libheif sequence decoder also decodes a linked alpha track whenever
it decodes a visual frame. Its current decoding options have no switch to skip
that work. This means the planned demand rule for avoiding alpha decode on a
color-only read cannot currently be met through `Track::decode_next_image()`.

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

## verified backend checks (2026-09-29)

Direct probes against the generated AVIF and HEIC sequences compared the
embedded library used by this checkout with the installed system library. The
embedded `libheif-rs 3.0.0` build reports libheif 1.23.1; the system probe used
libheif 1.23.5.

- `heif-info -d`, `avifdec --info` and the system libheif probe agree that the
  AVIF visual and alpha tracks have four samples at a timescale of 1000, with
  `stts` deltas of 80/170/110/240 and a total duration of 600. The embedded
  1.23.1 build reports 80 ticks for all four frames, in both native and RGB
  decode modes, despite returning the right aggregate track duration. The
  upstream fix explains the bug: the visual decoder assigned a duration using
  the sample being fed into the decoder, which can run ahead of the frame being
  returned. The fix looks up timing by the output position. System libheif
  1.23.5 returns the expected values for this fixture. The fixture's all-80
  failure pattern differs from the upstream regression example, so the exact
  mismatch pattern is specific to this input; the faulty lookup and corrected
  1.23.5 result are independently confirmed. `ffprobe` also reports AVIF
  presentation timestamps of 0, 0.08, 0.25 and 0.36 seconds, with a 16x12
  stream and four frames.
- Updating only `Cargo.lock` cannot currently bring in that correction. The
  locked `libheif-sys 5.3.1+1.23.1` embeds libheif 1.23.1, and the repository's
  vcpkg override also pins 1.23.1. As of this check, the latest published
  `libheif-sys` release still embeds 1.23.1. The coordinated implementation
  needs an updated/fixed embedded dependency or a checked timing parser for
  visual-track samples.
- HEIC track timing is 150 ticks per frame at a timescale of 1000 in both
  versions. The visual track and primary image handle report a presentation
  size of 16x12, but `decode_next_image()` returns 64x64 frames in both
  versions. `heif-info -d` shows the 16x12 `clap` on the primary image item,
  with offsets that place the aperture at x=0,y=0 in the 64x64 coded image; the
  visual and alpha tracks each have four samples at 150 ticks. A normal
  primary-image decode applies the item transformation and returns 16x12,
  while ignoring transformations returns 64x64. The first track frame matches
  the top-left 16x12 crop of the primary-image decode byte for byte. All four
  track frames report the same coded size, but only the first frame was compared
  pixel-for-pixel. `ffprobe` independently reports four 64x64 coded frames at
  150 ms intervals; it does not apply the 16x12 presentation crop. The decoder
  must apply the sequence presentation crop to each output, and the
  implementation must read crop metadata from the relevant item or track sample
  description rather than assume every crop starts at 0,0.
- The HEIC fixture contains a linked alpha track. In libheif's sequence decoder,
  decoding each visual frame recursively decodes that auxiliary track and
  attaches its alpha channel. The exposed decode options contain no way to
  suppress it, so the current wrapper cannot honor color-only demand for this
  format while using `decode_next_image()`.

## progress

Stages 1 and 2 have started against the plan above.

- **stage 1, the timeline, is implemented** in `src/animation.rs`: a compact
  `Segment` per input path, a `SegmentTable` that resolves an output frame to a
  path and a presentation, and checked `i128` rational arithmetic throughout.
  The rules unit tests pin down are that a still is one frame at any rate, that
  `floor(duration * fps)` output ticks are covered with a floor of one, that a
  picture is first shown at the first tick at or after it, that a zero or absent
  delay is held for one output tick, and that a tick starting exactly where a
  segment ends belongs to the next segment.
- **stage 2 has landed for GIF, APNG, animated WebP and JPEG XL.**
  `src/animation/apng.rs` composes APNG frames at the file's own depth,
  including 16-bit, because the `image` compositor's 16-bit arm is
  `unreachable!` and it refuses every 16-bit colour type.
  `src/animation/frames.rs` replays the two formats `image` already composites
  over the full logical canvas, so no disposal or blending code is duplicated
  for them. `src/animation/jxl.rs` uses the format's own scan and seek targets:
  a header-only pass reads the timing without rendering, and each presentation
  is decoded from its own checkpoint, so it is the one adapter that does not
  replay on a backward request. Its timeline keeps the codestream's own
  timescale rather than being converted to milliseconds first.
- **the prefetch pool is indexed by output frame**, not by file: `Prepare`
  gained `frames`, `estimate(index)` and `produce(index)`, and the segment table
  is what says which presentation a frame index names. The pool is otherwise
  unchanged, so the lookahead window, byte budget and generation rules still
  apply and one animation's presentations are serialised on the worker that
  reaches that file first.
- **the AVIF and HEIF/HEIC sequence tracks have landed too**, in
  `src/animation/sequence.rs` and `src/animation/heif.rs`. The container's own
  sample table is what places every presentation, because the embedded libheif
  1.23.1 build reports the wrong per-sample duration and no published
  `libheif-sys` release carries the fix; the same table is also how the sample
  count is known at all. The presentation crop is applied per plane, so a track
  whose coded pictures are 64x64 and whose aperture is 16x12 hands out the
  aperture. A track decodes as whatever colour its samples are: the fixtures are
  full resolution rgb, which libheif hands over as planar rgb with alpha.
- **one demand exception is deliberate.** A colour-only read of a HEIF sequence
  still decodes its linked alpha track, because `libheif-rs 3.0.0` exposes no
  control over it; the alternative is hand-writing a track decoder for one
  format, which this plan does not ask for. It is recorded here and in the
  changelog rather than left implicit.
- **one correctness bug this work surfaced and fixed**: a source used to keep
  only the presentation it decoded last, so a lookahead worker that moved the
  cursor ahead of the consumer made the consumer's next forward request replay
  the wrong picture. `AnimationSource` now keeps a bounded window of decoded
  presentations, which both fixes that and is the memory bound the plan asks
  for.
- **stage 3 is partly done**: the changelog records the behavior, and the
  validator still passes unchanged; the animation checks it needs are not
  written yet.

## remaining implementation checks

Every check below has been answered; what each answer is, and what it cost, is
recorded here rather than deleted, because the next reader of this plan is
likely to ask the same questions.

- **AVIF timing: resolved by reading the container, not the decoder.** The
  system libheif 1.23.5 probe returns 80/170/110/240 ms for the fixture, but no
  published `libheif-sys` carries that version: the newest is still
  `5.3.1+1.23.1`, so the embedded decoder cannot be updated to a fixed one. The
  checked `stts` reader the plan asked to keep as a fallback is therefore the
  implementation, in `src/animation/sequence.rs`, and it is what the fixture is
  placed by. Re-run the probe after any `libheif-sys` update: if a fixed release
  lands, the reader can stay, because it is also how the sample count is known.
- **The presentation crop is read from the container and applied per plane.**
  The fixture's aperture is a `clap` item property on the primary item, with a
  16x12 aperture over a 64x64 coded picture; the track's own sample description
  carries no aperture, and its record states the presentation size rather than
  the coded one, so the coded size comes from the item's `ispe`. Both placements
  are read, the track's own first, and a non-zero offset is applied by cropping
  each plane; a 4:2:0 aperture that lands off a chroma sample is refused rather
  than rounded.
- **The HEIF alpha question is decided, not deferred.** A colour-only read of a
  HEIF or HEIC sequence still decodes its linked alpha track. `libheif-rs 3.0.0`
  exposes no control over that and the fixture's visual track reports alpha of
  its own, so honouring the demand rule would mean hand-writing a track decoder
  for one container, which this plan does not ask for. The exception is recorded
  in the changelog and the README rather than left implicit.
- **The sample count comes from the same table as the timing.** `TrackTiming`
  is built from `stts`, so the count is known before a pixel is decoded, and the
  expansion is bounded before it allocates. JPEG XL's scan-only path was already
  the model for this and is implemented in `src/animation/jxl.rs`.
- **16-bit APNG composition is implemented and tested.** `src/animation/apng.rs`
  composes from `png 0.18.1` at the file's own depth, including RGBA16, with
  blend and disposal; the validator checks that a 16-bit APNG is `RGB48`, that
  it is seven frames at 24 fps, and that it switches picture at the third.
- **Benchmarking is done, and recorded in [BENCH.md](../BENCH.md).** A still
  image pays nothing for the new pool indexing: two builds measured in one batch
  over all nine sandbox sets, at three prefetch depths, land within 2.3% of each
  other in both directions, and the serial rows are the tightest. Sequential and
  backward playback were measured per decoder with the clip rebuilt for every
  pass, because VapourSynth caches a node's frames and would otherwise answer
  the second walk itself: on the four-presentation fixtures every order costs
  the same, and on a 150-presentation corpus a backward walk costs 1.31x to
  2.38x a forward one, which is the bounded window keeping the replay bounded
  rather than quadratic. One piece of headroom is measured and left in place:
  the JPEG XL adapter builds a decoder per presentation, and the obvious fix —
  keeping one and walking it forward — fails on a codestream whose frames are
  not uniform, so the conservative version is what shipped.

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
`heif-info`, and JPEG XL decodes back to a four-frame APNG through `djxl`. The
AVIF timing and HEIC crop behavior were also checked directly against libheif
1.23.1 and 1.23.5, as described above. These fixtures do not change the
still-image implementation.

Do not add a loop-count, duration override, or alternate animation filter
argument until a concrete caller needs it. The supported formats land together;
these checks close backend and performance details, not the format rollout
scope.
