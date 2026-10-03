# 28 - animation discovery and container fallbacks without image-rs

status: **selected, not implemented.** part of [26](26-remove-image-rs.md). Every
animated format and every container fallback now has a chosen decoder; see
[the selection](#the-selection) below. Two of the three candidates for
animated WebP were rejected, the container fallbacks turn out to be one library
rather than two rewrites, and the two compositors that are still to be written
already have reference implementations that agree with the plugin's output
byte for byte. No timeline, compositor or container parser has been changed.

## the selection

One row per animated format and per fallback, decided on 2026-10-04. `discover`
is where the timeline comes from, `pixels` is where the composited pictures
come from, and `state` says what already exists.

| format | discover | pixels | state |
| --- | --- | --- | --- |
| GIF | `gif 0.14.2` with `skip_frame_decoding(true)` | the same crate's LZW, composited here | **decided**: `gif` is already a dependency of `image`; the compositor is new |
| animated WebP | a first-party RIFF walk over `ANIM`/`ANMF` | libwebp, per sub-rectangle, composited here | **decided**; `WebPDemux` and `WebPAnimDecoder` are rejected |
| APNG | `png 0.18.1` | the same crate, already composed here in [`animation/apng.rs`](../../src/animation/apng.rs) | landed; only the shared types remain |
| JPEG XL | the `jxl 0.7.4` codestream header | `jxl` from each presentation's own checkpoint | landed in [`animation/jxl.rs`](../../src/animation/jxl.rs) |
| AVIF sequence | the sample table, already read here | `libheif`'s track cursor | landed in [`animation/heif.rs`](../../src/animation/heif.rs) and [`animation/sequence.rs`](../../src/animation/sequence.rs) |
| HEIF sequence | the same | the same | landed |
| AVIF still fallback | the container walker | `libheif`, not `image` | **decided**: one library for every refused container |
| HEIF RGB page | `libheif` | `libheif`'s planar rgb | **decided**: [`formats/heif.rs`](../../src/formats/heif.rs) already has the entry point |

Four of the eight rows are landed and need no decoder decision: APNG, JXL and
the two sequence adapters already read their own timelines and presentations in
project code. What is left for them is the shared types from
[29](29-decoder-types-without-image.md) plus an audit of the routes that still
reach `image`. The rest is the work: one compositor for GIF, one for WebP, the
two fallbacks onto `libheif`, and the split-extent join that `libheif` refuses.

### what the runs showed

**GIF: `gif 0.14.2` is the decoder, and its timeline pass is cheap and exact.**
`DecodeOptions::skip_frame_decoding(true)` with `next_frame_info()` walks the
whole stream without running LZW over it, and reports the logical screen, the
global palette, the background index, the loop count and, per frame, the
sub-rectangle, the delay and the disposal method. Run against
`tests/fixtures/animation.gif`, the three passes agree frame for frame:
`skip_frame_decoding(true)` reports the same four frames as a full decode, and
the delays are 8, 17, 11 and 24 centiseconds, which is the 80/170/110/240 ms
the baseline's `image` frames report as `80/1`, `170/1`, `110/1`, `240/1`.
The fixture's per-frame rectangles and disposal methods are the interesting
part and they are reported too: `16x12 at 0,0 Keep`, `11x10 at 0,0 Background`,
`16x12 at 0,0 Previous`, `16x10 at 0,0 Keep`, while the baseline's `image`
frames are always the full 16x12 canvas, because `image` has already composited
them. That is the work this plan has to do: `gif`'s frame is a sub-rectangle,
not a canvas.

**Animated WebP: a first-party RIFF walk for the timeline, libwebp for the
pixels.** Both candidates were run. `WebPDemux` agrees with a first-party walk
of the same file on everything the timeline needs -- canvas 16x12, four frames,
the same offsets and the same 80/170/110/240 ms delays -- so the walk is
sufficient and no new native dependency is needed for it. `bcdec_rs`'s lesson
from [27](27-direct-still-decoders.md) applies here too: a first-party reader
that the corpus can check against two independent implementations is cheaper
than a library that has to be linked and packaged. `WebPAnimDecoder` was
**rejected** because it hands over composed BGRA canvases from a buffer the
library owns, which is the opposite of the demand-aware, plugin-owned storage
the rest of this reader has.

**The two container fallbacks are one library, not two rewrites.** This is the
finding that changes the plan's own shape. `libheif` reads every container the
plugin's own walker refuses and hands back planar rgb at the stored depth:

| input | plugin's own walker | `image` 0.25.10 | `libheif 1.23.1` |
| --- | --- | --- | --- |
| `avif-split-extents.avif` (item split over extents) | refuses: `data_range` reads one extent | decodes (mp4parse joins them) | refuses: `Unknown OBU type 0 of size 264` |
| `grid2x2.avif` (a 2x2 grid of tiles, made by `avifenc -g 2x2`) | refuses: `native_eligible` excludes a grid | refuses: `Invalid argument` | **decodes: planar r/g/b 256x256** |
| `cicp-rgb8.avif` (rgb, `cICP` 9/18/0) | does not own it | decodes | **decodes: planar r/g/b, and refuses `monochrome` as an unsupported conversion** |

So the fallback for a container this reader refuses is `libheif` rather than a
compatibility adapter over mp4parse, and the `image` integration can go. The
one case `libheif` cannot do is the split-extent item, which is a bounded
change in the existing walker: join the extents' bytes before handing them to
`dav1d`, which is what `Meta::native_eligible` already decides. Grids become a
new capability rather than a replacement: `avifenc`-made grid files are refused
by *both* decoders today, so nothing regresses if they stay refused and
something is gained if the rgb route reaches them through `libheif`.

**A grid file is refused by every decoder today, and that is the baseline.**
`grid2x2.avif` through the built plugin:

```text
grid2x2.avif: Error: failed to create decoder for image 'target/cand-anim/avif-grid/grid2x2.avif':
              Format error decoding Avif: Invalid argument
```

which is `image`'s own message, reached through the fallback. `libavif`'s
`avifdec` decodes the same file to 256x256, so the file is valid and the
refusal is the readers'.

### the animated baseline an implementation has to reproduce

`target/cand-anim/animation-parity.py` prints one hash per output frame of both
clips for the eight committed animated fixtures. On the built plugin all six
multi-frame fixtures agree frame for frame with each other -- `animation.gif`,
`animation.webp`, `animation.png`, `animation.jxl`, `animation.avif` and
`animation.heic` are all `RGB24 16x12`, 14 frames, and the first six frames hash
identically -- which makes them their own oracle: a GIF compositor, a WebP
compositor and the four landed formats must land on the same pictures. The
record is `target/cand-anim/animation-baseline.txt`, and
`target/cand-anim/routes-baseline.txt` is the per-file route report.

The two formats that need a compositor written here already have an **external**
oracle, and both pass today.

**GIF.** `target/cand-anim/gif-vs-pillow.py` rebuilds each clip frame as RGBA --
the colour clip's three planes and the alpha clip's plane -- and compares it
with Pillow's composited `ImageSequence` frame for the presentation it shows.
All 14 frames match. That is the alpha rule, the disposal rule and the
transparency rule in one comparison, because Pillow composites all three. The
record is `target/cand-anim/gif-vs-pillow.txt`. Note that Pillow's frames are
mode `P` per subframe and only 16x12 after compositing: the per-frame
rectangles `gif` reports and Pillow's canvas are consistent, which is why this
is a comparison of the composited result rather than of the subframes.

**WebP.** libwebp's own `anim_dump.exe` writes one composed PNG per
presentation using its animation decoder;
`target/cand-anim/webp-vs-libwebp.py` compares the clip's sampled frames with
the presentation each shows. All 14 match, including the three that are not the
full 16x12 canvas, which is the part a project compositor has to get right.
The record is `target/cand-anim/webp-vs-libwebp.txt`.

Taken with the six-fixture agreement above, each of the two compositors to be
written has a reference implementation that already agrees byte for byte with
the plugin's current output, so a migration has something to fail against
rather than something to interpret.

### the order to implement it in

1. **GIF.** The crate is already linked through `image`, the timeline pass is
   verified, and the compositor is the only new code. The disposal rules, the
   transparent-canvas behaviour and the alpha clip below are the parts to get
   right. Gate: `gif-vs-pillow.py` must keep printing 14 of 14, which is both
   clips against an independent compositor.
2. **Animated WebP.** A first-party RIFF walk for the timeline, then the
   compositor over libwebp's existing decode of each sub-rectangle; `ANMF`'s
   fields are read byte for byte today and `WebPDemux` and `webpinfo` are the
   cross-checks for them. Gate: `webp-vs-libwebp.py` must keep printing 14 of
   14 against libwebp's own composed frames.
3. **The AVIF and HEIF fallbacks onto `libheif`, and the split-extent join.**
   This is what retires the `libheif_rs::integration::image` hook and the
   `image` feature. The grid route falls out of the same change.
4. **The shared types.** [29](29-decoder-types-without-image.md) last, because
   steps 1 to 3 are where the module shape settles.

Steps 1 and 2 need no dependency change at all. Step 3 removes one.

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

## GIF plan — selected

`gif 0.14.2`, used directly, is the decoder. Its `DecodeOptions` has
`skip_frame_decoding(true)`, which skips LZW pixel decoding while permitting
frame metadata traversal, and that was **verified against the fixture**: it
reports the same four frames, rectangles, delays and disposal methods as a full
decode, for 0.160 ms against the walk's 0.045 ms. The timeline is built from
frame delays and logical screen/subframe rectangles in that pass, with the
palettes, extensions, transparency, ICC presence and bounds read then too. See
the [GIF decoder API](https://docs.rs/gif/latest/gif/struct.Decoder.html) and
the versioned
[source](https://docs.rs/crate/gif/0.14.2/source/src/reader/mod.rs).

During playback, the `gif` frame is a subframe, not a displayed canvas, so the
canvas is composed here. Replacing the import alone loses essential behavior:

- Honor offsets, local/global palettes, transparent indices and interlacing.
- Preserve keep/background/previous disposal and its order relative to
  publishing a presentation. **Do not substitute the GIF background colour**:
  the compositor being replaced says so in as many words -- `image`'s
  `codecs/gif.rs` carries `// intentionally ignore the background color for web
  compatibility` -- keeps its canvas at `Rgba8`, and restores a
  `Background`-disposed rectangle to transparent rather than to the background
  index. The `gif-vs-pillow.py` comparison above is what pins that behaviour:
  Pillow composites transparency the same way and all 14 frames match.
- Bound saved previous-canvas or previous-rectangle storage and account for it
  alongside the presentation window. Do not mutate a cached presentation while
  composing its successor.
- Keep still one-picture behavior, including partial canvases, and make alpha
  correct for both the color-only and `ReadAlpha` requests.

The compositor being replaced is about 250 lines inside `image`'s
`codecs/gif.rs`: a canvas buffer, a `non_disposed_frame` for `Previous`, and one
`blend_and_dispose_pixel` per sample. So this is a port with the buffer type
changed rather than a new algorithm, and
[27](27-direct-still-decoders.md)'s provenance rules apply to it.

Reuse `Presentation`, `Rate`, `SegmentInfo` and the project's `AnimationDecoder`
trait rather than recreating image-rs `Frame` and `Delay`. Keep sequential state
and restart/checkpoint as necessary for backward reads. Skipping an output tick
does not mean skipping a subframe that a later displayed picture depends on.

## animated WebP plan — selected

Still WebP pixels already use libwebp, and the timeline comes from a
first-party RIFF walk. `anim_diff.exe` and `webpinfo.exe` are on this machine
and both read the same fields, so the walk has two independent cross-checks
besides the ones below.

The three candidates were weighed, and the middle row of the table is the
decision:

| candidate | run | verdict |
| --- | --- | --- |
| libwebp `WebPAnimDecoder` | not built; the header and `demux.h` were read | rejected: it hands over composed BGRA canvases from a buffer the library owns, which the plugin would copy on every frame, and it hides the sub-rectangles a demand-aware reader needs |
| first-party walk + existing still decoder + project compositor | the walk was written and run against three containers, and `WebPDemux` was linked and run on the same files | **selected**: the two agree on canvas, frame count, offsets and delays |
| direct `image-webp 0.2.4` | not built: it is the decoder being removed | rejected: it keeps `image-webp` in the tree and pays the same discovery decode [26](26-remove-image-rs.md) records |

`WebPDemux` itself is not selected, and that is the more interesting answer: it
needs a second archive linked, packaged and licensed, and the walk reaches the
same fields without it. The walk reads the canvas and loop count from `VP8X`
and `ANIM`, and each `ANMF`'s offset, size, duration, blend and disposal. Run on
the committed fixture, `WebPDemux` agrees with it on all of those -- canvas
16x12, four frames, the same offsets and the same 80/170/110/240 ms durations --
and `webpinfo.exe` agrees on both flags as well (`Dispose: 0`, `Blend: 1`
against the walk's `dispose=none blend=no-blend`). Two independent
implementations agreeing on every field is what makes the walk safe to rely on;
linking the demux archive would buy the same numbers and cost a second native
dependency. The vcpkg `x64-windows-static-md` install does put
`libwebpdemux.lib` beside `libwebp.lib`, but it publishes no package metadata
for it, so the plugin's build script would have to name three archives by hand
where it now names one package. See the
[WebP container API](https://developers.google.com/speed/webp/docs/container-api).

Animated WebP currently yields composed RGBA buffers to the existing output
writer. Preserve that output contract. Keep lossy opaque **still** WebP on its
native YUV route. Check one-frame animation containers explicitly rather than
letting them fall between the demux and still readers.

A future implementation that wants the demux archive instead must inspect
`build.rs`, vcpkg, manylinux and Homebrew linkage for it and its transitive
inputs, then update notices, exact licenses and wheel checks as required. This
research changes none of those files.

## other animations — selected

APNG already composes directly over `png`, including 16-bit channels, JXL
already scans headers and decodes presentations from checkpoints, and the
AVIF/HEIF track adapters already read sample tables and clean apertures in
project code. Each is landed and needs no decoder decision; what is left for
them is the shared-type migration in [29](29-decoder-types-without-image.md),
plus an audit of the `decoder::probe` and still-poster routes that still use
`image`. The 16-bit APNG fixture is the one to keep in the gate: `image`'s own
compositor refuses every 16-bit colour type, which is why this path is already
ours.

Preserve one play regardless of loop count, exact constant-rate sampling,
zero/absent delay as one output tick, default `24/1`, checked arithmetic, and
the bounded presentation window. Count decoder replay and cache hits separately
from the avoidable discovery decode. Do not combine this work with the deferred
JXL decoder-continuation experiment described in [BENCH.md](../BENCH.md).

## AVIF still fallbacks — selected

`formats/avif.rs` uses `Meta::native_eligible` to keep probe and decode
consistent. Its reader covers the containers whose primary item is one payload
of one extent or of `idat`; everything else falls back, and today that fallback
is `image`. The route inventory below is what the fallback actually carries,
taken from the built plugin rather than from the list of things that might:

| fixture | format through the plugin | which decoder produced it |
| --- | --- | --- |
| `avif-split-extents.avif` | `RGB24` 64x48 | `image` (mp4parse joins the extents) |
| `alpha-rgba8.avif`, `cicp-rgb8.avif`, `orientation-avif-rgb-irot-1.avif` | `RGB24` | `image` |
| `mono-alpha.avif`, `mono-alpha-10.avif`, `mono-alpha-12.avif` | `Gray8`/`Gray10`/`Gray12` | `image`, corrected to gray |
| every `avif-yuv*` and `orientation-avif-irot-*` | `YUV420P8`/`YUV422P8`/`YUV444P8`/`YUV444P10` | this module, through `dav1d` |
| `avif-no-picture.avif` | an error naming the file | this module, by design |
| a `2x2` grid (`avifenc -g 2x2`) | an error | `image`, which refuses it too |

The fallback is therefore three things: a monochrome item, an item split over
extents, and a container whose matrix or layout this reader will not state a yuv
format for. The decision is `libheif` for all three, because it was run against
each of them and it answers planar rgb at the stored depth:

- `cicp-rgb8.avif`: planar r/g/b 3x2, and `monochrome` refused as an
  unsupported conversion, which is the right refusal for an rgb file.
- `grid2x2.avif`: planar r/g/b 256x256, a file neither this reader nor `image`
  can decode.
- `avif-split-extents.avif`: **refused**, with `Unknown OBU type 0 of size 264`.
  This is the one container `libheif` cannot replace, and the answer is the
  existing walker rather than a new adapter: join the item's extents' bytes
  before handing them to `dav1d`. `Meta::native_eligible` already decides that
  case, `data_range` already rejects it with a named error, and the fixture
  already exists, so the change is small and testable.

Preserve native YUV output only on the routes that currently expose it. For a
fallback RGB route, preserve conversion, matrix/range interpretation, stored
word and alpha behavior. A different RGB conversion can change samples even
when the source is lossless. Retain monochrome depth corrections and container
`irot`/`imir` orientation. Keep path-qualified errors, checked file/idat bounds
and the low-latency no-picture behavior from [15](15-avif-decoder-progress.md)
and [17](17-avif-container-robustness.md).

## HEIF RGB fallback — selected

`formats/heif.rs::handles` accepts non-RGB output, and `color_space_of` already
maps `Rgb8..Rgb16` to `Rgb(C444)`, so the entry point that decodes an rgb page
is the one the gray and yuv pages already use. Only the routing is missing: an
rgb page is still decoded through `libheif_rs::integration::image`, which
constructs a primary handle, describes it without pixel decoding, then requests
RGB and copies libheif's interleaved rows into image-rs's caller buffer, which
the plugin then writes into VapourSynth planes.

The decision is to delete the hook and decode the rgb page here. Ask libheif for
`Rgb(C444)` -- planar, one plane per channel -- rather than an interleaved
spelling, because a frame is written from planes and the interleaved form would
only be split again here. `pack_plane` already drops libheif's row padding into a
tightly packed plane, and it is the same function the gray and yuv pages use.

Preserve the fallback's stored word depth and alpha alongside the native paths.
Container transforms may already be applied by libheif: use the project's current
transform/inverse-transform policy so orientation is applied exactly once and
`apply_rotation=False` still returns the stored picture. Audit ICC and colour
metadata availability on each route rather than assuming the old integration
exported everything the direct handle can read; changes to public properties
beyond parity need explicit scope. Only after this route is direct can the Cargo
`image` feature and the process-global hook registration be removed. See the
[libheif-rs API](https://docs.rs/libheif-rs/3.0.0/libheif_rs/).
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
