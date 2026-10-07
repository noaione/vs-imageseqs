# improvement plans

one file per change, each with the evidence, the intended edit, and how to
check the result. performance measurements come from [benchmarks](../BENCH.md)
and the probes described there; correctness and distribution plans also cite
code inspection and targeted reproductions. `plans` below is the list itself, `deferred` is
everything the landed ones left over — work that is not a plan because nothing
has measured it as worth doing — and `not planned` is the decisions.
[HANDOFF.md](../HANDOFF.md) is the same thing from the other end: where the tree
stands and what to do first.

[13](13-icc-color-management.md) exposes embedded ICC profiles as the standard
binary `ICCProfile` frame property when `icc_profile=True`. The default remains
safe and compatible: it detects and preserves only the `ImgSeqHasICC` fact.

all five are implemented: 05 in `src/formats/heif.rs` (with one writer change in
`src/pixel.rs` and the avif probe in `src/decoder.rs`), 01 in `src/prefetch.rs`
plus `src/source.rs`, 02 in `src/clip.rs` with the generic pool in
`src/prefetch.rs`, and 04 + 03 in `src/formats/webp.rs` with the planar `Pixels`
type in `src/decoder.rs`. [09](09-exif-orientation.md) is implemented too, in
`src/pixel.rs` and `src/clip.rs` behind `apply_rotation`.

06 and 07 are implemented. 06 uses the `jpeg2k` wrapper with its vendored
OpenJPEG backend; 07 landed through the two `image` features the doc's own configuration block lists
(`dds` and `ff`), with hand-written DXT5 and farbfeld alpha fixtures. the three
other rows the list was written from all landed —
the colour properties its `# Color Metadata` section asks for, in `src/color.rs`
with a container read in `src/formats/heif.rs`, `src/formats/jxl.rs` and the new
`src/formats/png.rs` ([08](08-color-metadata.md)), the exif orientation it
probes and never applies ([09](09-exif-orientation.md)), and the nominal
10/12-bit depths its scope list defers ([10](10-nominal-bit-depth.md)) — and the
two of them whose cost could move keep their measurements: 0.15 ms per heic file
and 0.04 ms per png file for 08's container reads, measured against the sets it
describes, and 112.0 → 112.1 ms per frame for the convert stage of a ten bit
avif under 10's shift.

nothing left in the list is a speed plan: 06 and 07 are two formats no sandbox
set covers, and 09 was a transform the file asked for and did not get.

[11](11-jxl-direct.md) is the odd one out and came out of the last row of that
list: reading the orientation for a jxl showed that the decoder the plugin wraps
applies the file's orientation itself, so the format reports 1 and hands out the
rotated picture whatever `apply_rotation` says. dropping the `image` adapter for
the `jxl` crate underneath it fixes that, and it is what lets
[08](08-color-metadata.md) and [10](10-nominal-bit-depth.md) read a jxl's colour
encoding and its bit depth. it is implemented in `src/formats/jxl.rs`, with no
change to what a jxl decodes to: 35 pages, frame for frame identical. the two
format rows are covered by committed fixtures and the VapourSynth validator.

[12](12-heif-avif-yuv-output.md) came out of the samples in
`sandbox/hitokage-sample` and is the first plan here that is about what a frame
*is* rather than what it says: heif, heic and avif are stored as yuv and handed
out as rgb, so a 4:2:0 page costs 72 MB of frame where its planes are 36 MB, a
ten bit page loses its depth into `RGB48`, and both libheif and `image` spend a
conversion per frame that the graph may not even want. it hands out the planes
the decoders have instead, which needs [08](08-color-metadata.md)'s matrix and
range to label them honestly — and which changes the format of every colour heic
and avif frame, the largest output change this plugin has made. it is implemented
in `src/formats/heif.rs` and the new `src/formats/avif.rs` (`dav1d` reading the
avif item, with the container walked by the module itself), with the yuv variants
in `src/pixel.rs`: 34 of the 35 `sandbox/avif` pages and the 4 colour pages of
`sandbox/heic` are `YUV420P8` now, a frame is 55.40 → 27.70 MiB, and the avif
decode pass is 35% faster. the 31 monochrome heic pages, the unspecified-matrix
`hitokage-sample` avifs and every other container are unchanged, and the four
promises it does not keep — a heic's orientation code, the alpha item on `Read`,
`iinf`/`pixi`, and the monochrome avif decode — are listed in the plan.

[10](10-nominal-bit-depth.md) is the last of these three to land, and the
smallest of them, exactly because [12](12-heif-avif-yuv-output.md) took most of
it: a yuv frame's depth is its format, so the avif and heic pages needed nothing
here. what was left is the depth a colour type cannot state — a jxl codestream
header, `av1C` on the rgb path, and libheif's handle — so the probe of each
reader keeps one more number and `PixelFormat::at_depth` names the format for
it, while the writer moves every sample down by `word_bits - frame_bits`. that
shift is exact rather than approximate (scaling a sample of `bits` bits onto a
word of `word` bits multiplies it by less than `2^(word - bits) + 1`, so the
largest scaled sample stays below the smallest one of the value above it), which
is why jxl keeps asking for sixteen bit words and takes the same shift as every
other reader instead of being the exception the plan describes. it is implemented
in `src/pixel.rs` and the three container modules, against three hand-made jxl
fixtures: the two deep pages of `sandbox/hitokage-sample` are `RGB30` and `RGB36`
now, `jxl-gray10`/`jxl-gray12`/`jxl-rgba10` are `Gray10`/`Gray12`/`RGB30`, and
`mono-10.heic`/`mono-12.heic`/`mono-alpha-12.avif` cover the native-depth
monochrome paths. 105 of the 106 parity lines are byte identical to the build
before it, and the paired convert stage moved 112.0 → 112.1 ms on the ten bit
avif, which is to say the shift cost nothing.

## evidence in short

per frame with `prefetch=0` and `debug=True` (see the per stage table in
[BENCH.md](../BENCH.md)):

| set | decode | copy into the frame | serial floor |
| --- | --- | --- | --- |
| webp 2903x4128 | 111 ms | 5 ms | ~6 ms |
| jxl 1500x2500 | 85 ms | 13 ms | ~18 ms |
| jpeg 1404x2000 | 18 ms | 12 ms | ~17 ms |
| png 1404x2000 | 9 ms | 3 ms | ~8 ms |

the webp row is what [03](03-webp-yuv-output.md) and
[04](04-webp-decoder.md) left behind, and it is not the row this table had
before: 265 ms of decode and 48 ms of write with `image-webp` and a then-36 MB
rgb frame. the other rows are unchanged by either plan.

the *serial floor* is what the requesting thread must do for every frame no
matter how many decoders run behind it: for a long time that was the copy into
the frame plus the frame properties, and after
[02](02-frame-write-path.md) it is only the wait for a finished frame, because
the worker that decoded the file builds the frame too. three conclusions follow.

1. **the lookahead used to decode some frames twice** (fixed by
   [01](01-lookahead-scheduling.md)). cpu per delivered frame on the webp set:
   268 ms at `prefetch=0`, 361 ms at 4, 925 ms at 16, while the wall time got
   worse after 6. the queue was `workers + 2` frames deep, but the 192 MiB
   budget only held 5.6 of those 36 MB frames. the sandbox set was the extreme
   case: its colour frames are 55 MiB, so the budget held 3.5 of them, and
   `prefetch=16` spent 2.6x one decode of cpu per delivered frame while the
   default at 4 spent 1.16x. the window is now capped by the budget and the
   budget follows the window, which turns that 2.6x into 1.43x.
2. **parallelism could not beat the floor until [02](02-frame-write-path.md)
   moved it.** the webp set sat at 88.8 ms/frame with four workers, and about
   53 ms of that was unavoidable while the copy stayed on the requesting thread.
   [03](03-webp-yuv-output.md) cut the frame to 1.5 bytes per pixel, which took
   the floor to about 6 ms of the 91 ms that `prefetch=4` delivered, and 02 then
   moved the write itself into the pool, so the requesting thread only hands over
   a frame a worker finished. the win is proportional to how much the write *was*
   the floor: 5% to 35% on a pool with room, nothing on a pool that was already
   slower than the thread asking.

a third one, from reading `image-webp` rather than from a measurement: a webp
frame used to be copied twice and allocated three times per frame (the decoder's
own canvas, our zeroed `pixels` buffer, then the frame).
[04](04-webp-decoder.md) removed the canvas and the zero fill, and
[03](03-webp-yuv-output.md) removed the conversion that was left.

one more comes from the same family, and [05](05-monochrome-heif.md) is where it
was found: an avif used to be decoded twice, once by the probe and once for its
frame, because `image`'s avif decoder decodes the picture and its alpha item
inside `AvifDecoder::new`, before it can report a size. the probe now answers
from the container boxes instead, so probing the 130 MB avif set went from 6.43 s
of clip creation, 183 ms per file, to 2 ms, and reading that set from 11.92 s of
open plus frames to 4.99 s.

## plans

### frame source indices — 2026-10-07

[40 frame source indices](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/40-frame-source-indices.md)
is implemented: `ImgSeqIndex` names the path's position in `files` rather than
the output frame number, which an animation shifted for every file after it,
and a new `ImgSeqAnimationIndex` names the displayed picture's position within
its own file, written on animated files only. The two cannot be one property,
because an animation contributes several output frames. The validator's
`test_source_indices` holds both, including the ordering an animation's length
used to get wrong.

### removing libwebp — 2026-10-06

[39 removing libwebp](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/39-libwebp-removal.md)
is implemented, and nothing links libwebp any more: every webp, an animated
container's rectangle included, is decoded by wpd. The tests hold wpd against
reference payloads captured from libwebp before it was unlinked, and `build.rs`,
`vcpkg.json`, both Linux builders, the legal files and the notice bundle no
longer name it. Frame parity is byte identical on 809,492 sampled frames over
the webp fixtures, the 48 file upstream corpus, the 57 tiff fixtures that
existed then, three synthetic animations and the three sandbox sets, both clips
each; the validator passes its 954 checks; the animation read is 1.11x to 1.16x
faster than the build that still linked it; and the Windows library is 134 KB
smaller. The removal also fixed a four sample WebP-compressed tiff strip, which
now decodes to libtiff's own decode of the same file and used to come out with
the fourth sample of every pixel zeroed and every row but the first shifted.

### PNGWrite: request-driven PNG export

[38 PNGWrite](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/38-png-write.md)
is implemented in `src/writer.rs`: `core.imgseqs.PNGWrite(clip, output_path=...)`
returns a node whose frame request writes that frame as a PNG and writes nothing
for a frame nobody asks for. Integer Gray and RGB at eight to sixteen bits, an
optional matching Gray alpha clip, and a frame of nine to fifteen bits stored as
a sixteen bit PNG whose high bits hold the source precision with an `sBIT` chunk
saying so. The path grammar numbers by the writer node's own frame index plus
`start_number`, `always_save` and `overwrite` are separate permissions, success
is a per-instance bit set rather than a frame property, and a write is published
from a temporary file by a rename or a linking create. `tests/pngwrite.vpy`
checks it with 182 checks over its own PNG reader. It compresses with `zlib-rs`
through `png`'s feature of that name, which puts it 1.4x to 2.1x ahead of serial
Pillow on time and level with it on size;
[the writer benchmark](../BENCH.md#png-writer-comparison) compares it against
serial and six-worker Pillow over three cohorts, and
`target/bench/png-write.py` carries the barebone copy of nmanga's save path that
comparison needs. YUV and float conversion stays upstream, and low-bit gray and
palette output are not written.

### unsupported subtypes — 2026-10-06

[36 unsupported subtypes](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/36-unsupported-subtypes.md)
collects the refused subtypes it takes up: what the container states, where the
change lands, the fixture it needs
and the check that would accept it. All ten routes are implemented -- the TGA two
byte map entries, the bare DIB, cursors, the gray+alpha TIFF, the flat gray EXR,
twelve bit subsampled AVIF/HEIF, a heic storing av1, the ISO composition boxes, the
JP2 channel definitions and TIFF WebP, whose palette is expanded here, and a ycbcr TIFF
whose strip is a JPEG is read too -- and what stays refused by decision is JP2 signed or
mixed-precision samples.

### wpd WebP decoder — 2026-10-04

[35 wpd WebP decoder](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/35-wpd-webp-decoder.md)
is implemented. wpd decodes an ordinary still, writing the borrowed rows of the
picture it decoded straight into the frames the call allocated. The animated
container's rectangle, its canvas and its integer blend were libwebp's at the
time; [39](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/39-libwebp-removal.md)
has since moved the rectangle onto wpd too and unlinked libwebp. The
first-party probe and the one-presentation `first_picture` guard are
unchanged. The integrated measurement is 1.22x to
1.31x on the 35 page sandbox set and 1.53x on the serial per-frame stage split
(196.84 → 128.46 ms a frame), with byte identical pixels on 76 of 76 webp parity
lines and all 941 validator checks it ran then; `docs/BENCH.md` has the tables.
The all-wpd animation path was the plan page's optional experiment, and 39 is
what did it.

### input routing and planar decoding — 2026-10-04

[34 input routing and planar decoding](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/34-input-routing-and-planar-decode.md)
is closed in substance. Content-based still/animation routing, image-rs removal,
the shared probe input, the planar and row writes and the timing repairs have all
landed, and its status table separates those results from the historical 59-of-84
cohort. Current checks pass 0 of 76 renamed copies and 0 of 7 renamed timelines.
The one named item left is its DXT SIMD proposal at the end, which waits for a
quiet machine; its `dds` row is settled rather than open. The original inventory
and benchmarks are labeled historical.

Plan 34's candidate table has since lost its low-bit gray TIFF row: a gray page
of one, two or four bits a sample is read and expanded to `Gray8` rather than
refused, with six fixtures, a `tests/readalpha.vpy` section that reads them and a
Pillow parity check that agrees with every one of them.

### image-rs removal goal — 2026-10-03

These four plans record the research that preceded image-rs removal. The
dependency is now removed; plan 34's current-status table records that landed
work separately from the remaining input and planar work. The table below
retains the original research scopes and statuses. Its earlier GIF/WebP
discovery findings and benchmark requirements describe those snapshots, not
the current implementation inventory. Future performance changes still need
a matched baseline, correctness checks and memory measurements.

| plan | scope | original research status |
| --- | --- | --- |
| [26 remove image-rs](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/26-remove-image-rs.md) | staged goal, complete-coverage removal gate, required baseline protocol and current baseline attempts/results | proposed, no implementation |
| [27 direct still decoders](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/27-direct-still-decoders.md) | every still format's decoder selected, the candidate rejections and their reproductions, the parity rules a first-party reader inherits and the order to implement them in | **selected, not implemented**: 12 formats keep or promote a crate, 5 get a first-party reader, 2 zune candidates and the DDS pair are rejected on reproduced defects |
| [28 animation and container decoders](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/28-animation-container-decoders.md) | every animated format's decoder and both container fallbacks selected, the candidate runs, the animated parity baseline and the order to implement it in | **selected, not implemented**: GIF and animated WebP keep or add no dependency, APNG/JXL/AVIF-HEIF sequences are landed, and `libheif` replaces `image` as the fallback for every refused container |
| [29 decoder types without image](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/29-decoder-types-without-image.md) | enums, source labels, orientation, dispatch, frame ownership, tests and transitive dependency removal | proposed, no implementation |

### proposed for review — 2026-09-27

Plans 17–20 were documentation only when they were written. Each records the
evidence, intended scope and acceptance checks; performance ideas explicitly
require measurement before choosing a change.

[21](21-animated-images.md) planned one coordinated implementation for GIF,
APNG, animated WebP and JPEG XL, plus AVIF/HEIF sequence tracks, and has landed
for all of them. It keeps the existing `Read`/`ReadAlpha` API and still-image
behavior, and accounts for timeline sampling, APNG's 16-bit path, stateful
decoders and bounded memory. The benchmark the plan asks for is recorded in
[BENCH.md](../BENCH.md): a still image pays nothing for the new pool indexing,
and a backward walk of a long animation costs 1.3x to 2.4x a forward one.

[15](15-avif-decoder-progress.md),
[16](16-container-orientation.md),
[17](17-avif-container-robustness.md) and
[18](18-demand-aware-decoding.md) have landed, and so has
[20](20-distribution-followups.md)'s release-metadata slice. Address that plan's
**source-build consistency** next, which needs a container and a network the
machine this was written on does not have, then its macOS portability and source
provenance. [19](19-avif-thread-budget.md) has been measured and decided against;
it needs no further work.

| plan | evidence | priority | status |
| --- | --- | --- | --- |
| [19 AVIF thread budget](19-avif-thread-budget.md) | default native decoder threading runs inside the prefetch pool | medium | measured and decided against: the default already uses 8.8 of ten cores, and dividing them by the worker count costs 20% |
| [20 distribution follow-ups](20-distribution-followups.md) | sdist input mismatch, untested archive rebuilds, macOS dependencies and release metadata | high for source builds | release metadata, repeatable staging and macOS runtime portability implemented; source rebuilds and macOS provenance remain open |
| [21 animated images](21-animated-images.md) | GIF, APNG, WebP, JXL and AVIF/HEIF sequence tracks need timeline expansion, composition, delay sampling and bounded random access | medium to high | implemented for every format it names, with the regression and playback benchmarks recorded |
| [25 decode column parity](25-decode-column-parity.md) | Pillow's `decode` stops at `L` while the plugin's ends at the file's own format, so on a palette corpus the two columns are 12 MB against 36 MB per frame | medium for how the suites are read | measured, not implemented: after 22 the plugin is ahead on all three sets measured, so what is left is how the suites report the format they compared |

### implemented plans

The current distribution work is [14 — Linux wheel distribution and plugin manifests](14-linux-wheel-distribution.md): manylinux/auditwheel builds,
bundled Linux dependencies, an `imageseqs/manifest.vs` installation layout,
and release validation. Implemented and validated locally on Linux and Windows;
that page records the completed checks and remaining CI validation.

[23](23-cpu-variant-avx2.md) and [24](24-cpu-variant-avx512.md) are the
distribution half of that: an x86-64 wheel now carries one library per
microarchitecture level, and VapourSynth's own manifest rules pick the one the
host CPU supports. The measurement that made it worth a second and third build
is in the table below: 6% to 11% on every png and jpeg set tried, almost all of
it in the plane write. The baseline build still passes no `-C target-cpu` at
all, so no machine that could load the plugin before can stop loading it.
[33](33-musllinux-wheel.md) is the other half of the platform question: a
manylinux wheel cannot load on a musl host at all, so the release now carries a
second Linux wheel, built in the Alpine pypa image.

| plan | touches | expected | risk | status |
| --- | --- | --- | --- | --- |
| [37 Linux build caching](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/37-linux-build-caching.md) | Linux wheel CI, build/setup scripts, native cache checker, source packaging | reuse compiled codecs and Cargo dependencies, plus musl Rust and Python downloads | low: exact native keys and complete-install checks, repair and clean-container validation still run | implemented; local checks pass, CI cache reuse and timings pending |
| [33 musllinux wheel](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/33-musllinux-wheel.md) | `tools/build-musllinux.sh`, `tools/make-linux-source-bundle.sh`, `tools/check-linux-wheel.py`, CI, notices, `tests/check-packaging-tools.py` | a `musllinux_1_2_x86_64` wheel beside the manylinux one, with the same three plugin libraries and the C++ runtime the musl policy does not promise the host | low: it adds a wheel, and no frame, format, property or argument changes | written and locally checked; the container build and its validation run in CI |
| [32 host-safe CPU variant builds](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/32-host-safe-cpu-variant-builds.md) | `hatch_build.py`, `tests/check-packaging-tools.py` | an explicit Cargo target keeps AVX2/AVX512 flags out of build scripts and procedural macros that run on CI | low: target dependencies keep their CPU flags, wheel layout unchanged | implemented, Windows wheel, all variants and Cargo isolation validated |
| [31 stage bundled runtime libraries](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/31-stage-bundled-runtime-libraries.md) | `tools/stage-native.py`, `tests/check-packaging-tools.py` | macOS/Linux runtime dependencies under `imageseqs/lib/` stage without being mistaken for plugin variants | low: plugin validation stays at the manifest directory, wheel contents unchanged | implemented, local platform-layout regression checks |
| [30 Windows AVIF sequence decoder](https://github.com/noaione/vs-imageseqs/blob/master/docs/improvements/30-windows-avif-sequence-decoder.md) | `vcpkg.json`, `tools/vcpkg-ports/libheif/`, wheel/source packaging, CI, notices, `tests/check-autoload.py` | `animation.avif` decodes through libheif's built-in dav1d backend instead of failing with no matching decoder | low: enables the existing decoder, keeps libheif default features disabled | implemented, Windows release and installed-wheel validation |
| [23 avx2 variant](23-cpu-variant-avx2.md) | `hatch_build.py`, `tools/stage-native.py`, `tools/check-linux-wheel.py`, `tools/package-linux-wheel.py`, `tests/check-packaging-tools.py` | an x86-64 wheel carries `vs_imageseqs.dll`, `.avx2` and `.avx512`, and VapourSynth loads the widest its host CPU supports from a manifest that names only the stem | low: no pixel changes, and the validator logs are byte identical across the three builds | implemented |
| [24 avx512 variant](24-cpu-variant-avx512.md) | `hatch_build.py` | the same, with the `x86-64-v4` library and the `avx512` suffix | low: it needs avx512f/bw/cd/dq/vl, and the core only picks it on a host that has them | implemented |
| [22 png decode path](22-png-decode-path.md) | `src/formats/png.rs`, `src/decoder.rs`, `src/clip.rs`, `target/bench/decode/` | a png this module can walk fills the frame from the decoder's own rows, and a palette page is expanded here rather than by the decoder: 1.36x on `sandbox/posterize-check` and 1.23x on `sandbox/png`, which puts the plugin ahead of Pillow's decode column on every set measured (`0.689x`, `0.891x`, `0.906x`) | medium: it moves where every sample is written, so it ships with a 245 line parity check and a per-file refusal report | implemented |
| [18 demand-aware decoding](18-demand-aware-decoding.md) | `src/decoder.rs`, `src/clip.rs`, `src/prefetch.rs`, `src/formats/avif.rs`, `src/formats/heif.rs`, fixtures, `tests/readalpha.vpy` | a call that hands out no alpha clip does not decode an avif alpha item, and a call that does not export an ICC profile does not keep its bytes | low to medium, a colour-only read stops failing on a broken alpha item | implemented |
| [20 distribution follow-ups](20-distribution-followups.md) | `tools/`, `tests/check-packaging-tools.py`, CI, `.gitignore`, `pyproject.toml` | a release cannot be published from a tag whose version, `pyproject.toml`, `Cargo.toml` and changelog do not agree, and a second build in one checkout starts from what it built | low, release tooling and CI only | release metadata and repeatable staging implemented; source rebuilds, macOS portability and provenance proposed |
| [17 AVIF container robustness](17-avif-container-robustness.md) | `src/formats/avif.rs`, `tests/make-alpha-fixtures.py`, fixtures, `tests/readalpha.vpy` | a malformed avif container is refused instead of panicking, allocating gigabytes or answering wrongly, and a container this reader will not decode is described as the format the fallback decoder produces | low to medium, it changes what an unsupported container is described as | implemented |
| [16 container orientation](16-container-orientation.md) | `src/formats/avif.rs`, `src/formats/heif.rs`, `src/decoder.rs`, fixtures, `tests/readalpha.vpy` | an avif or heif that states `irot`/`imir` is handed out the way the file describes it, `apply_rotation=False` gives the stored picture back, and `ImgSeqOrientation` reports the code | medium, it moves the size of every file whose container states a transform | implemented |
| [15 AVIF decoder progress](15-avif-decoder-progress.md) | `src/formats/avif.rs`, fixtures, `tests/readalpha.vpy` | a frame request on an item that holds no picture errors instead of hanging, and the item decoder runs at low latency | low, an error path and decoder settings | implemented |
| [14 Linux distribution and manifests](14-linux-wheel-distribution.md) | Hatch, CI, packaging tools, notices | manylinux 2.28 wheel and matching ZIP with bundled codecs and manifest | medium, distribution layout | implemented; Linux/Windows checked locally, CI pending |
| [01 lookahead scheduling](01-lookahead-scheduling.md) | `src/prefetch.rs`, `src/source.rs` | webp 88.8 → ~70 ms at `prefetch=4`, and `prefetch` above 4 stops being a pessimisation | low, internal only | implemented |
| [02 frame write path](02-frame-write-path.md) | `src/clip.rs` (new), `src/prefetch.rs`, `src/source.rs`, `src/decoder.rs`, `src/pixel.rs` | a few ms per frame from the buffer, and up to 1.6x on webp if the copy leaves the requesting thread | medium, frame lifetime | implemented, 2b only |
| [03 yuv output for lossy webp](03-webp-yuv-output.md) | `src/formats/webp.rs`, `src/decoder.rs`, `src/pixel.rs`, `src/source.rs`, `src/color.rs` | webp 4.24 → 3.39 s, half the bytes per frame | medium, changes the output | implemented, with 04 |
| [04 webp decoder](04-webp-decoder.md) | `Cargo.toml`, `build.rs`, `vcpkg.json`, notices, `LICENSES/`, `src/formats/webp.rs`, `src/decoder.rs` | decode 265 → 111 ms per frame, and it enables 02 and 03 | medium, native dependency | implemented, with 03 |
| [05 monochrome heif](05-monochrome-heif.md) | `src/formats/heif.rs`, `src/pixel.rs` | the 31 monochrome heic files in `sandbox/heic` decode as `Gray8` instead of failing, and a monochrome avif is handed out as `Gray8` instead of `RGB24` | low, used to repair an always-failing path | implemented |
| [06 jpeg 2000 backend](06-jpeg-2000-backend.md) | `Cargo.toml`, README, notices, `LICENSES/`, `src/formats/jp2.rs`, `src/decoder.rs`, fixtures, `tests/readalpha.vpy` | a `.jp2`/`.j2k` file reads through a header-only probe as gray/RGB at its nominal depth, with planar sYCC at supported sampling and depth | medium, vendored native decoder | implemented |
| [07 dds and farbfeld](07-dds-and-farbfeld.md) | `Cargo.toml`, `README.md`, fixtures, `tests/readalpha.vpy` | `.dds` and `.ff` files stop failing the probe, for two feature flags and no native code | low | implemented |
| [08 color metadata](08-color-metadata.md) | `src/decoder.rs`, `src/formats/heif.rs`, `src/formats/jxl.rs`, `src/formats/png.rs` (new), `src/color.rs` | `_Primaries`/`_Transfer` from the container's `nclx`, `cICP` or jxl codestream header, and `_Matrix`/`_Range` from the file for a yuv frame, while an icc-only file changes nothing | low to medium, a wrong claim is worse than none | implemented |
| [09 exif orientation](09-exif-orientation.md) | `src/decoder.rs`, `src/source.rs`, `src/clip.rs`, `src/pixel.rs`, fixtures, `tests/readalpha.vpy` | a file whose exif says 6 comes out the way its thumbnail looks, behind an `apply_rotation` argument that defaults on, and `ImgSeqOrientation` keeps saying what the file said | medium, it swaps width and height | implemented |
| [10 nominal bit depth](10-nominal-bit-depth.md) | `src/pixel.rs`, `src/formats/avif.rs`, `src/formats/heif.rs`, `src/formats/jxl.rs`, fixtures, `tests/readalpha.vpy` | a 10-bit avif is `Gray10`/`RGB30` instead of `Gray16`/`RGB48`, with the samples shifted into the words a 10-bit frame holds, and the 16-bit files stay 16-bit: 112.0 → 112.1 ms per frame on the convert stage, so the shift is free | medium, every 9-to-15-bit file changes format | implemented |
| [11 jxl without the image integration](11-jxl-direct.md) | `Cargo.toml`, `src/formats/jxl.rs` (new), `src/formats/mod.rs`, `src/decoder.rs`, `src/pixel.rs`, fixtures, `tests/readalpha.vpy` | a jxl that states an orientation reports it and `apply_rotation=False` gives the stored picture back, and the codestream's colour encoding and bit depth reach the probe | medium, the decode loop becomes ours | implemented |
| [12 heif and avif planes](12-heif-avif-yuv-output.md) | `Cargo.toml`, `src/pixel.rs`, `src/decoder.rs`, `src/formats/heif.rs`, `src/formats/avif.rs` (new), `src/color.rs`, `src/clip.rs`, `src/source.rs`, fixtures, `tests/readalpha.vpy` | a colour heic or avif page is `YUV420P8`/`YUV444P10` instead of `RGB24`/`RGB48`, at half the bytes and with no conversion in the plugin: 55.40 → 27.70 MiB a frame, the avif decode pass 35% faster | high, it changes the format of every colour heic and avif frame | implemented |
| [13 expose embedded icc profiles](13-icc-color-management.md) | `src/source.rs`, `src/decoder.rs`, `src/formats/`, `src/color.rs`, fixtures, `tests/readalpha.vpy` | `False` preserves current properties; `True` exposes the raw embedded profile as `ICCProfile` without changing pixels or formats | medium, every reader needs exact profile extraction | implemented |

the dependencies were thin: 01 and 02 were independent of each other, 03 needed
04, and 05 was independent of all of them and is the only one that fixes
correctness rather than speed, so it did not have to wait for a decision on the
others. 01 was the only purely internal change, which made it the right first one
to try.

06 to 10 are independent of each other and of all five. 06 is the only one that
adds a native dependency, 07 is two feature flags, 08 changes what a frame says
about itself, 09 changed where its samples are, and 10 changes the format and the
samples together — so 10 is the last of them to do, and 08 is the one with a rule
("the file states it or the property is unset") instead of a measurement. 11 adds
no dependency, only a layer: the same `jxl` crate the adapter already wrapped,
with the adapter's own 367 lines replaced by the module that reads the header. it
went first because 08's jxl row and 10's jxl row have no other source, and
because it was the one item on this list that is a live defect rather than a
missing feature.

12 is the one that reorders the queue. its yuv hand-out needs the matrix and the
range the file states, which is 08's read of the container, so 12 comes after 08
and consumes `ImageInfo.cicp` instead of adding a second reader. it also takes
most of 10's avif and heic rows with it: on the yuv path the depth is carried by
the format, so there is nothing to shift and no `RGB30`-versus-`RGB48` choice to
make, and 10 keeps the jxl rows, the rgb files that state 9 to 15 bits, and the
monochrome ones. that makes the order 08, 12, 10 the cheapest one — but 12 is
also the widest change on the list, so doing the small metadata win and the
smaller depth work first is a defensible alternative, it just means writing the
avif depth shift for the rgb path that 12 would then retire. 08, 12 and 10 all
landed in that order, so the queue is down to 06 and 07, the two formats. the
rest of what is outstanding after all twelve — the leftovers each plan records
and the two `defer:` entries in `IMPLEMENTATION.md` that are neither done nor
refused — is the `deferred` section below.

order of value, as it turned out: 04 + 03 were the only route past bestsource on
webp, 02 was worth 5% to 35% on the frames it was written for and took the manga
jpeg corpus from 1.6x to 2.5x of bestsource, 05 repaired a format that never
worked at all, and 09 removed the last place where the two plugins disagreed
about what a file means. 12 is the second widest of the small-output changes
(35% off the avif decode pass and half the bytes per frame) and the only one that
makes a graph do different work rather than the plugin, which is why its plan
page carries a parity measurement beside its speed one. 09 also cost the most of the small ones: a transposing
write reads across the decoder buffer instead of along it, which was 9x the
identity write before the walk was blocked, and 1.7x on an interleaved page and
2.9x on a planar one once the mirrors went back to whole rows, the channels of a
frame shared one walk, and a sample move stopped being a runtime-sized copy. 10
is the cheapest of the lot: it moved the format and the position of every sample
of the files that state a depth, and the paired convert-stage measurement says it
cost nothing, because the load and the store were already there.

[15](15-avif-decoder-progress.md) is the first of the 2026-09-27 review to land,
and the only one of them that was a live defect: an item that holds no picture
left a frame request running forever. it cost an error path plus one decoder
setting — the item is decoded at low latency, because a still image is one frame
and a frame delay is a pipeline nothing can use — and that setting turned out to
be worth 15.6% of the avif decode pass and 43% of `sandbox/hitokage-sample`
rather than a cost, so the plan that asked only for termination made the format
faster as well.

[16](16-container-orientation.md) is the second, and it is the other half of what
[09](09-exif-orientation.md) started: a file states its orientation in three
places, and the two containers that state it as `irot` and `imir` item
properties had never been read. the plugin now maps every combination of the two
onto the exif code that describes the same picture, applies it to the avif it
decodes itself and accounts for the one `libheif` has already applied, so
`apply_rotation=False` reaches the stored picture for both. it is the widest
output change of the two — a rotated file's width and height move — and no file
in the sandbox states a transform, so the parity check is what says nothing else
did.

[17](17-avif-container-robustness.md) is the correctness one, and it is the
first plan here about what a file the plugin *cannot* read should do. four
defects were reproduced against the build before it — an extended box header read
as eight bytes, an extent the file does not hold reaching the allocator before
the read that fails on it, field widths and bit reads that shifted a value out of
its type, and a probe that described a container as yuv before `decode` refused
it. the first three are bounds; the fourth is what makes a file whose item is
split over several extents decode through the fallback decoder instead of failing
a frame request, which is the one of the four a reader can see.

[18](18-demand-aware-decoding.md) is the first plan here about work nothing asks
for, and both halves of it were measured before they were changed. `Read` was
decoding an avif alpha item it never hands out — the item is a coded item of its
own, so that is a whole decoder — and the probe was keeping a copy of every
file's embedded ICC profile although export is off by default. the first is
2.7–3.4 ms of a 1536x2304 page and 11–20% of a colour-only read; the second is
36 MiB on a 35-file clip whose files each carry a 1 MiB profile. it is also the
only plan here that changed what a file the plugin *can* read does: a colour-only
read no longer fails on a broken alpha item, which is tested rather than
discovered.

[20](20-distribution-followups.md) is the distribution follow-up, and its
release-metadata slice has landed: `tools/create-changelog.py` is strict by
default, so a tag whose version, `pyproject.toml`, `Cargo.toml` and changelog do
not agree fails the release job instead of publishing placeholder notes, and the
build outputs that accumulate wheels — `dist/`, the two manylinux wheel
directories, the staged bundle — are emptied of what a build writes before it
writes. it is the only plan here whose subject is the release rather than the
reader, and it is the one that came last in the plan's own order because its
first slice needs a container and a network. the rest of the review is still
open: 20's source rebuilds and macOS source provenance.

[19](19-avif-thread-budget.md) was the last of the 2026-09-27 review and is the
only one of them that ended without a change. it asked whether a decoder that
defaults to one thread per core, running several at a time inside the lookahead
pool, should be given a share of the machine instead; a sweep over five prefetch
depths and seven thread counts later, the answer is that the default is already
at 8.8 of this machine's ten cores, that a share of them starves each decoder by
20%, and that every explicit count above it moves nothing but the thread count.
the corpus, the grid and the parity check are in [BENCH.md](../BENCH.md), and
what would reopen it is a machine that actually oversubscribes rather than one
that does not.

## deferred

what the landed plans recorded under "left over", and what `IMPLEMENTATION.md`
defers without refusing. These remain exploratory unless explicitly promoted
to a numbered plan above. Performance work needs a corpus and a measurement
before implementation; correctness work needs a reproducible case or a clear
code-path defect. Each bullet names the page that keeps the full argument. the
rest of that `defer:` list — gpu output, manual simd, filesystem globbing and
format-based clip grouping — is in `not planned` below, with its reasons.

**one format is slower than the reader it replaced**

[26](26-remove-image-rs.md) moved ten still formats off the `image` crate and
verified every one of their pixels, planes and properties byte-identical. Three
of the four speed differences that measurement found are resolved: `hdr` is
faster (1.16x to 0.965x, three whole-picture buffers collapsed to one), `tga` is
1.13x from 1.34x (a redundant copy of the picture removed), and `ppm` needed
nothing -- its 1.19x was this tree's own harness comparing best-of runs between
a high-variance baseline and a low-variance build, and the median says it is
faster.

`dds` is still 1.12x, and the cause is verified rather than inferred:
`still::Decoder::read(self, buffer)` fills the caller's frame directly, while
`src/formats/dds.rs` builds a 4.3 MB `Vec` that the plugin then copies into the
frame -- about 0.86 ms a file of extra traffic against a measured gap of 1.15.
The fix is a `RowStream`, which `src/formats/png.rs:650` already models: hold the
path, re-open in `fill`, and hand each decoded row to the `RowSink`. For `dds`
that means walking block rows and decoding each block into the four pixel rows it
belongs to, so the intermediate never exists. The part that makes it more than
mechanical is that the sink is one entry per **plane**, so an interleaved pixel
is three plane writes and a correct stream decodes a whole block row into a small
buffer before scattering it.

Four measured attempts went into these two formats and three were wrong, so the
page records the split that worked: every fix came from removing a whole
traversal of memory, and every failure from rearranging one. `dds` has such a
traversal to remove and no arithmetic to tune.

then it was answered. **`dds` is settled rather than open**: four attempts were
built and measured, all reverted -- three row-stream and loop variants (67.1, 62.5
and 66.1 ms against a buffered 60.7) and one that copied whole lines instead of
pixels (57.7 against 55.3) -- and `widen`, `from_565`, `colour_block` and
`alpha_levels` were compared against `image`'s `dxt.rs` and are already the same
algorithm. The regression is real (15 interleaved pairs, paired ratio median
1.124, faster in 3 of 15) and it is not in the code that was looked at.

**The streaming idea did pay, on `ppm`.** One netpbm shape now answers
`Pixels::Stream`: a packed `P6` at `MAXVAL` 255, whose rows are `width * 3` bytes
of the file and therefore already the layout the frame wants. Fifteen interleaved
pairs put it at a paired ratio median of **0.789**, faster in 14 of 15 -- 21% off
-- and 0.921 against the reader it replaced. The technique is `RowSink::place_rgb8`
in `src/decoder.rs`, lifted out of `png.rs`, and the split it establishes is that
streaming pays when a decoder's own output order already matches the frame's and
loses when it must transpose, because then it trades a bulk copy for per-pixel
work. `tga`'s uncompressed rows and `bmp`'s bottom-up rows are the next candidates
by that rule; `hdr` transposes for three of its eight orientations and `qoi`
decodes to a buffer by construction.

**the decoder could write the frame itself**

- **decode straight into the frame's planes.** `JxlOutputBuffer::new_from_ptr`
  takes a byte stride, `dav1d::Decoder` is generic over a `PictureAllocator`, and
  libwebp's planar entry point writes into a buffer and a stride the caller owns,
  so jxl, avif and webp could each skip the copy the plugin does today
  ([11](11-jxl-direct.md), [12](12-heif-avif-yuv-output.md),
  [04](04-webp-decoder.md)). it waits for the copy to matter, and no measurement
  here shows it does: it is one pass over bytes the decoder just wrote, and the
  rgb paths would still have an interleave to do.
- **a blocked SIMD transpose** for `apply_rotation=False`, where the write is
  scalar and cannot beat ~0.6 ns per memory operation
  ([09](09-exif-orientation.md)): a rotated lossless page is 158 ms of frame
  time against 110 ms identity.
- **the jxl parallel runner and libwebp's intra-frame threading.** both nest
  another pool inside the lookahead pool the plugin runs, which is why neither
  is proposed ([11](11-jxl-direct.md), and `not planned` for webp). the same
  reason is behind the `custom Rayon inside get_frame()` entry in
  `IMPLEMENTATION.md`'s `defer:` list: the lookahead pool is the parallelism
  ([01](01-lookahead-scheduling.md)), and a second one inside a frame request
  would nest it.

**depth**

- **the depths that have a format and no file.** nine, eleven, thirteen,
  fourteen and fifteen bits are mapped and unit tested and no file here states
  one; a `P5` `PGM` or a `P7` `PAM` with another `MAXVAL` and one line in
  `tests/make-alpha-fixtures.py` makes one ([10](10-nominal-bit-depth.md)).
- **a sixteen bit file whose decoder does not fill the range**, which the format
  claims anyway, as every other reader here does
  ([10](10-nominal-bit-depth.md)).
- **`YUV420P14` and its siblings.** libheif can report fourteen bits for a page
  whose samples it decodes itself and `yuv_format` has no arm for it, so such a
  page falls back to rgb ([10](10-nominal-bit-depth.md),
  [12](12-heif-avif-yuv-output.md)).
- **twelve bit heic, tiled (grid) avif, gain maps, ten bit PQ/HDR avif**: read
  nowhere and absent from the sandbox ([12](12-heif-avif-yuv-output.md)).
- **a ten bit monochrome heic has no fixture.** the heif row of
  [10](10-nominal-bit-depth.md) is implemented and unverified: the fixture needs
  `heif-enc`, which is not installed on the machine the plan was built on, and a
  `heif-enc` pass over `tests/make-alpha-fixtures.py`'s ten bit gray source is
  what would close it.

**what a file states and the plugin does not read**

- **the avif and heif `Exif` item**, so a rotated avif reports a code at all and
  a rotated heic stops reporting 1 — libheif applies `irot`/`imir` itself, and
  the code can only be had by reading those boxes
  ([09](09-exif-orientation.md)). Promoted to
  [16](16-container-orientation.md), including the rotation-policy and
  double-transform questions.
- **`_ChromaLocation` from av1's `chroma_sample_position`**: two bits inside the
  sequence header, and `unknown` in every file of the corpus
  ([08](08-color-metadata.md)).
- **png's `cHRM`, `gAMA` and `sRGB` chunks**, which state the same thing as the
  `cICP` chunk less directly ([08](08-color-metadata.md)).
- **ICC conversion**, which remains a downstream color-management concern.
  [13](13-icc-color-management.md) exports the raw embedded bytes through the
  opt-in `ICCProfile` frame property, but does not transform pixels.
- **animation, previews and tone mapping** in a jxl and their equivalents
  elsewhere: one frame per file is the whole contract
  ([11](11-jxl-direct.md)).
- **`pclr`, `cdef`, `res`/`resc` and the `uuid` boxes of a jp2**
  ([06](06-jpeg-2000-backend.md)).
- **the tiles of a grid avif, in this reader.** `dimg` names them and the probe
  already reads that reference; what is missing is each tile's own coding record
  and the placement of the decoded tiles into a picture of the grid's size. A grid
  is read today by `libheif`, which the walk hands a container it refuses, and
  `tests/readalpha.vpy`'s `test_avif_grid` checks the joined picture's four cells;
  reading it here is what would remove that fallback, not what would make a grid
  readable ([17](17-avif-container-robustness.md)).
- **a heic that stores av1**: the `ispe`/`av1C` read would apply to it and
  nothing in the corpus is one ([05](05-monochrome-heif.md)).

**formats with no corpus**

- **a real dds or farbfeld file.** `sandbox/` has a folder per container it
  claims and neither of these has one, so two hand-written files prove the
  routing and the format mapping and not the codecs
  ([07](07-dds-and-farbfeld.md)); dds is also a container for mipmaps, cubemaps
  and volume textures, of which the top mip of the first face is all a
  one-frame-per-file reader can use.
- **camera RAW**, which is a decoder and a demosaic rather than a format
  correction ([12](12-heif-avif-yuv-output.md)).

**costs left standing, all small**

- the lookahead overhead at `prefetch=16` on webp, 1.43x of one decode per
  delivered frame, and the `--prefetch 4` row of the manga webp set that was
  never re-measured ([01](01-lookahead-scheduling.md)).
- the heif and heic hook probe's 5 ms for all 35 pages, and libheif parsing a
  monochrome page a second time when it decodes it
  ([05](05-monochrome-heif.md)).
- the jxl probe parsing the header twice when `apply_rotation=False`, 0.065 ms
  per file, which is what makes `ImgSeqOrientation` non-empty for a jxl at all
  ([11](11-jxl-direct.md)).
- `mismatch=True` beside rotation, two ways to get a heterogeneous clip with
  only one of them a property a graph can read
  ([09](09-exif-orientation.md)).
- **`adjust_orientation` is a trap for a future jxl upgrade**: the option exists
  in 0.7.4 and is read nowhere, and if a later version honours it, it must stay
  `true` ([11](11-jxl-direct.md)).

**a pass over the present-tense `image` references**

118 mentions of the `image` crate remain in `src/`, and most are provenance
rather than a claim about today -- a module says it is a port of that crate's
reader, which is what `CHANGELOG.md` says the notices are for. A few asserted
current behaviour instead: `dds.rs`'s header said a surface must be a multiple of
four where the code clips an odd one, and `avif.rs`'s `yuv_format` said a depth
with no format keeps the rgb "the `image` decoder produces". Both are corrected.
The remaining mentions have not been read one by one, and the distinction to
apply is whether the sentence is about where the code came from or about what it
does now.

## validating a change

`cargo test --locked` plus the bench on two sets. the png set is the control:
its frames are small, nothing is evicted today, so it must not regress.

```console
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --extra --prefetch 16 --dir "I:/Manga/KamiKatsu/source/v06"
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --extra --prefetch 16 --dir "I:/Manga/Yuri Love Story/source/v02" --pattern "Yuri Love Story - v02 - p%03d.jpg"
```

for a change that is meant to be pixel neutral, run `target/bench/frame-parity.py`
against the old and the new build and compare the two dumps: it hashes every
plane of every frame of seven sandbox sets and of both `ReadAlpha` clips.
`target/bench/stage-split.py` prints the per stage means of the debug log and
takes a prefetch argument.

the bench prints wall time per frame; the plans that talk about wasted work
also need process CPU per frame, which the local probe beside it measures.

building any of this needs the native dependencies back. `vcpkg_installed/` is
currently empty in this checkout, so refresh it first:

```console
C:/vcpkg/vcpkg.exe install --triplet x64-windows-static-md --x-manifest-root="$PWD" --x-install-root="$PWD/vcpkg_installed"
```

## not planned

- **intra-frame threading of the pure rust webp decoder.** ffmpeg spreads one
  vp8 frame over every core and that is most of why bestsource wins on webp.
  the decoder is not this repository's — `image-webp` when this was written, and
  `wpd` ([35](35-wpd-webp-decoder.md)) as its selected replacement — so slice
  parallelism inside one frame is upstream work either way. not a change this
  repository can make.
- **a thread budget for the avif decoder.** [19](19-avif-thread-budget.md) is
  the measurement that closed it: dav1d already defaults to one thread per
  core, the default `prefetch=4` already uses 8.8 of this machine's ten cores,
  and giving each worker `cores / workers` starves a page that needs three to
  four threads before its tiles are saturated — 1607.6 ms against 1343.6. every
  explicit count of four threads or more lands within 2% of the default in both
  directions and moves only the thread count (57 to 33 at the default, 189 to 93
  at `prefetch=16`), and CPU per delivered frame stays where it was. a floor
  low enough to be safe on another core count is a floor tuned to these files,
  which is the case for leaving dav1d's own answer alone.
- **ffmpeg as a dependency.** it would match bestsource's decoder behaviour,
  but it is a large build and licence surface for one format when libwebp is a
  smaller step with the same yuv entry point.
- **DDS BC6 and BC7, and a cubemap or a volume as more than one frame.** the
  reader takes DXT1, DXT3 and DXT5 and their DX10 equivalents, and a cubemap's
  other faces, a volume's other slices and anything past the top mip are ignored
  rather than refused, because one frame a file is the contract. BC7 would be a
  second per-block decoder for surfaces nothing here has a corpus for, and BC6H
  is half-float HDR, which would need a tone-mapping policy this plugin does not
  have
- **raising `AUTO_MAX_WORKERS` above four by default.** the memory question no
  longer blocks it, because the budget scales with the window
  ([01](01-lookahead-scheduling.md)), but the measured answer is still no: the
  cheap sets gain nothing from depth and the deepest rows only add cpu, so the
  default stays small and a caller who wants more asks for it.
- **the python `imgseqs.from_folder(...)` helper, a format-based clip grouping**
  **utility, and in-plugin globbing.** all three are in `IMPLEMENTATION.md` as
  things that "can later" be provided, and all three are policy rather than
  work: the caller passes the ordered list, and a graph that needs a folder
  walked, or clips split by format, does it in python where the list comes from.
  `AGENTS.md` keeps the wheel plugin-only for the same reason.
- **gpu/vulkan output.** `IMPLEMENTATION.md` defers it until `vapoursynth4-rs`
  exposes a frame allocator, and it still does not.
- **manual simd for the rgb deinterleave.** the per stage table in
  [BENCH.md](../BENCH.md) has never shown the copy as the bottleneck the way
  [02](02-frame-write-path.md) showed the floor, so there is nothing to measure
  it against yet. (the nominal 10/12-bit representation the same entry in
  `IMPLEMENTATION.md` defers is [10](10-nominal-bit-depth.md) now, and it landed
  without needing any of this.)
