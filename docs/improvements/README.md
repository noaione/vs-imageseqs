# improvement plans

one file per change, each with the evidence, the intended edit, and how to
check the result. the measurements come from [benchmarks](../BENCH.md) and from
the probes described there.

all five are implemented: 05 in `src/formats/heif.rs` (with one writer change in
`src/pixel.rs` and the avif probe in `src/decoder.rs`), 01 in `src/prefetch.rs`
plus `src/source.rs`, 02 in `src/clip.rs` with the generic pool in
`src/prefetch.rs`, and 04 + 03 in `src/formats/webp.rs` with the planar `Pixels`
type in `src/decoder.rs`.

[06](06-jpeg-2000-backend.md) to [10](10-nominal-bit-depth.md) are open. they
are the parts of [IMPLEMENTATION.md](../IMPLEMENTATION.md) that the build does
not have: a jpeg 2000 backend the plan names and nothing compiles, the two
`image` features the doc's own configuration block lists (`dds` and `ff`), the
colour properties its `# Color Metadata` section asks for with no source to read
them from, the exif orientation it probes and never applies, and the nominal
10/12-bit depths its scope list defers. none of the five is a speed plan, so none
of them carries a measurement: two are formats no sandbox set covers, two change
what a frame holds or claims about itself, and one is a transform the file asks
for and does not get.

[11](11-jxl-direct.md) is the odd one out and came out of the last row of that
list: reading the orientation for a jxl showed that the decoder the plugin wraps
applies the file's orientation itself, so the format reports 1 and hands out the
rotated picture whatever `apply_rotation` says. dropping the `image` adapter for
the `jxl` crate underneath it fixes that, and it is what lets
[08](08-color-metadata.md) and [10](10-nominal-bit-depth.md) read a jxl's colour
encoding and its bit depth, so it is proposed before them rather than beside
them.

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
| [06 jpeg 2000 backend](06-jpeg-2000-backend.md) | `Cargo.toml`, `vcpkg.json`, `build.rs`, README, notices, `LICENSES/`, `src/formats/jp2.rs` (new), `src/decoder.rs` | a `.jp2`/`.j2k` file reads as `Gray8`/`RGB24`/`Gray16`/`RGB48` instead of failing the probe, and the probe stays a header walk | medium, a fourth native dependency | proposed |
| [07 dds and farbfeld](07-dds-and-farbfeld.md) | `Cargo.toml`, `README.md`, fixtures, `tests/readalpha.vpy` | `.dds` and `.ff` files stop failing the probe, for two feature flags and no native code | low | proposed |
| [08 color metadata](08-color-metadata.md) | `src/decoder.rs`, `src/formats/heif.rs`, `src/formats/png.rs` (new), `src/color.rs` | `_Primaries`/`_Transfer` from the container's `nclx`/`cICP`, and `_Matrix`/`_Range` from the file for a yuv frame, while an icc-only file changes nothing | low to medium, a wrong claim is worse than none | proposed |
| [09 exif orientation](09-exif-orientation.md) | `src/decoder.rs`, `src/source.rs`, `src/clip.rs`, `src/pixel.rs`, fixtures, `tests/readalpha.vpy` | a file whose exif says 6 comes out the way its thumbnail looks, behind an `apply_rotation` argument that defaults on, and `ImgSeqOrientation` keeps saying what the file said | medium, it swaps width and height | implemented |
| [10 nominal bit depth](10-nominal-bit-depth.md) | `src/pixel.rs`, `src/decoder.rs`, `src/formats/heif.rs`, `src/clip.rs`, `src/source.rs` | a 10-bit avif is `Gray10`/`RGB30` instead of `Gray16`/`RGB48`, with the samples shifted into the words a 10-bit frame holds, and the 16-bit files stay 16-bit | medium, every 9-to-15-bit file changes format | proposed |
| [11 jxl without the image integration](11-jxl-direct.md) | `Cargo.toml`, `src/formats/jxl.rs` (new), `src/formats/mod.rs`, `src/decoder.rs`, `src/pixel.rs`, fixtures, `tests/readalpha.vpy` | a jxl that states an orientation reports it and `apply_rotation=False` gives the stored picture back, and the codestream's colour encoding and bit depth reach the probe | medium, the decode loop becomes ours | proposed |

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
no dependency, only a layer: the same `jxl` crate the adapter already wraps, with
the adapter's own 367 lines replaced by the module that reads the header. it
moves first because 08's jxl row and 10's jxl row have no other source, and
because it is the one item on this list that is a live defect rather than a
missing feature.

order of value, as it turned out: 04 + 03 were the only route past bestsource on
webp, 02 was worth 5% to 35% on the frames it was written for and took the manga
jpeg corpus from 1.6x to 2.5x of bestsource, 05 repaired a format that never
worked at all, and 09 removed the last place where the two plugins disagreed
about what a file means. 09 also cost the most of the small ones: a transposing
write reads across the decoder buffer instead of along it, which was 9x the
identity write before the walk was blocked, and 1.7x on an interleaved page and
2.9x on a planar one once the mirrors went back to whole rows, the channels of a
frame shared one walk, and a sample move stopped being a runtime-sized copy.

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
  `IMPLEMENTATION.md` defers is [10](10-nominal-bit-depth.md) now.)
