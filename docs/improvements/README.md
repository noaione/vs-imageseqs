# improvement plans

one file per change, each with the evidence, the intended edit, and how to
check the result. the measurements come from [benchmarks](../BENCH.md) and from
the probes described there. `plans` below is the list itself, `deferred` is
everything the landed ones left over — work that is not a plan because nothing
has measured it as worth doing — and `not planned` is the decisions.
[HANDOFF.md](../HANDOFF.md) is the same thing from the other end: where the tree
stands and what to do first.

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
now, `jxl-gray10`/`jxl-gray12`/`jxl-rgba10` are `Gray10`/`Gray12`/`RGB30`, 105 of
the 106 parity lines are byte identical to the build before it, and the paired
convert stage moved 112.0 → 112.1 ms on the ten bit avif, which is to say the
shift cost nothing.

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

| plan | touches | expected | risk | status |
| --- | --- | --- | --- | --- |
| [01 lookahead scheduling](01-lookahead-scheduling.md) | `src/prefetch.rs`, `src/source.rs` | webp 88.8 → ~70 ms at `prefetch=4`, and `prefetch` above 4 stops being a pessimisation | low, internal only | implemented |
| [02 frame write path](02-frame-write-path.md) | `src/clip.rs` (new), `src/prefetch.rs`, `src/source.rs`, `src/decoder.rs`, `src/pixel.rs` | a few ms per frame from the buffer, and up to 1.6x on webp if the copy leaves the requesting thread | medium, frame lifetime | implemented, 2b only |
| [03 yuv output for lossy webp](03-webp-yuv-output.md) | `src/formats/webp.rs`, `src/decoder.rs`, `src/pixel.rs`, `src/source.rs`, `src/color.rs` | webp 4.24 → 3.39 s, half the bytes per frame | medium, changes the output | implemented, with 04 |
| [04 webp decoder](04-webp-decoder.md) | `Cargo.toml`, `build.rs`, `vcpkg.json`, notices, `LICENSES/`, `src/formats/webp.rs`, `src/decoder.rs` | decode 265 → 111 ms per frame, and it enables 02 and 03 | medium, native dependency | implemented, with 03 |
| [05 monochrome heif](05-monochrome-heif.md) | `src/formats/heif.rs`, `src/pixel.rs` | the 31 monochrome heic files in `sandbox/heic` decode as `Gray8` instead of failing, and a monochrome avif is handed out as `Gray8` instead of `RGB24` | low, used to repair an always-failing path | implemented |
| [06 jpeg 2000 backend](06-jpeg-2000-backend.md) | `Cargo.toml`, README, notices, `LICENSES/`, `src/formats/jp2.rs`, `src/decoder.rs`, fixtures, `tests/readalpha.vpy` | a `.jp2`/`.j2k` file reads through a header-only probe as gray/RGB at its nominal depth, with planar sYCC 4:2:0 where supported | medium, vendored native decoder | implemented |
| [07 dds and farbfeld](07-dds-and-farbfeld.md) | `Cargo.toml`, `README.md`, fixtures, `tests/readalpha.vpy` | `.dds` and `.ff` files stop failing the probe, for two feature flags and no native code | low | implemented |
| [08 color metadata](08-color-metadata.md) | `src/decoder.rs`, `src/formats/heif.rs`, `src/formats/jxl.rs`, `src/formats/png.rs` (new), `src/color.rs` | `_Primaries`/`_Transfer` from the container's `nclx`, `cICP` or jxl codestream header, and `_Matrix`/`_Range` from the file for a yuv frame, while an icc-only file changes nothing | low to medium, a wrong claim is worse than none | implemented |
| [09 exif orientation](09-exif-orientation.md) | `src/decoder.rs`, `src/source.rs`, `src/clip.rs`, `src/pixel.rs`, fixtures, `tests/readalpha.vpy` | a file whose exif says 6 comes out the way its thumbnail looks, behind an `apply_rotation` argument that defaults on, and `ImgSeqOrientation` keeps saying what the file said | medium, it swaps width and height | implemented |
| [10 nominal bit depth](10-nominal-bit-depth.md) | `src/pixel.rs`, `src/formats/avif.rs`, `src/formats/heif.rs`, `src/formats/jxl.rs`, fixtures, `tests/readalpha.vpy` | a 10-bit avif is `Gray10`/`RGB30` instead of `Gray16`/`RGB48`, with the samples shifted into the words a 10-bit frame holds, and the 16-bit files stay 16-bit: 112.0 → 112.1 ms per frame on the convert stage, so the shift is free | medium, every 9-to-15-bit file changes format | implemented |
| [11 jxl without the image integration](11-jxl-direct.md) | `Cargo.toml`, `src/formats/jxl.rs` (new), `src/formats/mod.rs`, `src/decoder.rs`, `src/pixel.rs`, fixtures, `tests/readalpha.vpy` | a jxl that states an orientation reports it and `apply_rotation=False` gives the stored picture back, and the codestream's colour encoding and bit depth reach the probe | medium, the decode loop becomes ours | implemented |
| [12 heif and avif planes](12-heif-avif-yuv-output.md) | `Cargo.toml`, `src/pixel.rs`, `src/decoder.rs`, `src/formats/heif.rs`, `src/formats/avif.rs` (new), `src/color.rs`, `src/clip.rs`, `src/source.rs`, fixtures, `tests/readalpha.vpy` | a colour heic or avif page is `YUV420P8`/`YUV444P10` instead of `RGB24`/`RGB48`, at half the bytes and with no conversion in the plugin: 55.40 → 27.70 MiB a frame, the avif decode pass 35% faster | high, it changes the format of every colour heic and avif frame | implemented |

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

## deferred

what the landed plans recorded under "left over", and what `IMPLEMENTATION.md`
defers without refusing. none of it is a plan: a plan here needs a corpus and a
measurement that says the work is worth doing, and these are the notes that
have neither yet. each bullet names the page that keeps the full argument. the
rest of that `defer:` list — gpu output, manual simd, filesystem globbing and
format-based clip grouping — is in `not planned` below, with its reasons.

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
  ([09](09-exif-orientation.md)).
- **`_ChromaLocation` from av1's `chroma_sample_position`**: two bits inside the
  sequence header, and `unknown` in every file of the corpus
  ([08](08-color-metadata.md)).
- **png's `cHRM`, `gAMA` and `sRGB` chunks**, which state the same thing as the
  `cICP` chunk less directly ([08](08-color-metadata.md)).
- **the icc bytes**, read and dropped: nothing in this workspace reads a raw
  profile, and turning one into primaries is the guess rule 3 forbids
  ([08](08-color-metadata.md)). `full ICC color management` is the `defer:` entry
  in `IMPLEMENTATION.md` that this sits on.
- **animation, previews and tone mapping** in a jxl and their equivalents
  elsewhere: one frame per file is the whole contract
  ([11](11-jxl-direct.md)).
- **`pclr`, `cdef`, `res`/`resc` and the `uuid` boxes of a jp2**
  ([06](06-jpeg-2000-backend.md)).
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
  `image-webp` exposes no threading hooks and owns no slice parallelism, so
  this would need to happen upstream. not a change this repository can make.
- **ffmpeg as a dependency.** it would match bestsource's decoder behaviour,
  but it is a large build and licence surface for one format when libwebp is a
  smaller step with the same yuv entry point.
- **raising `AUTO_MAX_WORKERS` above four by default.** the memory question no
  longer blocks it, because the budget scales with the window
  ([01](01-lookahead-scheduling.md)), but the measured answer is still no: the
  cheap sets gain nothing from depth and the deepest rows only add cpu, so the
  default stays small and a caller who wants more asks for it.
- **the python `imgseqs.from_folder(...)` helper, a format-based clip grouping
  utility, and in-plugin globbing.** all three are in `IMPLEMENTATION.md` as
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
