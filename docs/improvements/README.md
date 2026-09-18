# improvement plans

one file per change, each with the evidence, the intended edit, and how to
check the result. the measurements come from [benchmarks](../BENCH.md) and from
the probes described there.

05 is implemented in `src/formats/heif.rs`, 01 in `src/prefetch.rs` plus
`src/source.rs`, and 04 + 03 in `src/formats/webp.rs` with the planar `Pixels`
type in `src/decoder.rs`. every other entry is a proposal to review.

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
matter how many decoders run behind it: the copy into the frame plus the frame
properties. two conclusions follow.

1. **the lookahead used to decode some frames twice** (fixed by
   [01](01-lookahead-scheduling.md)). cpu per delivered frame on the webp set:
   268 ms at `prefetch=0`, 361 ms at 4, 925 ms at 16, while the wall time got
   worse after 6. the queue was `workers + 2` frames deep, but the 192 MiB
   budget only held 5.6 of those 36 MB frames. the sandbox set was the extreme
   case: its colour frames are 55 MiB, so the budget held 3.5 of them, and
   `prefetch=16` spent 2.6x one decode of cpu per delivered frame while the
   default at 4 spent 1.16x. the window is now capped by the budget and the
   budget follows the window, which turns that 2.6x into 1.43x.
2. **parallelism cannot beat the floor.** the webp set sat at 88.8 ms/frame
   with four workers, and about 53 ms of that was unavoidable while the copy
   stayed on the requesting thread. [03](03-webp-yuv-output.md) cut the frame
   to 1.5 bytes per pixel, which took the floor to about 6 ms of the 91 ms that
   `prefetch=4` now delivers.

a third one, from reading `image-webp` rather than from a measurement: a webp
frame used to be copied twice and allocated three times per frame (the decoder's
own canvas, our zeroed `pixels` buffer, then the frame).
[04](04-webp-decoder.md) removed the canvas and the zero fill, and
[03](03-webp-yuv-output.md) removed the conversion that was left.

## plans

| plan | touches | expected | risk | status |
| --- | --- | --- | --- | --- |
| [01 lookahead scheduling](01-lookahead-scheduling.md) | `src/prefetch.rs`, `src/source.rs` | webp 88.8 → ~70 ms at `prefetch=4`, and `prefetch` above 4 stops being a pessimisation | low, internal only | implemented |
| [02 frame write path](02-frame-write-path.md) | `src/decoder.rs`, `src/source.rs`, `src/pixel.rs` | a few ms per frame from the buffer, and up to 1.6x on webp if the copy leaves the requesting thread | medium, frame lifetime | proposed |
| [03 yuv output for lossy webp](03-webp-yuv-output.md) | `src/formats/webp.rs`, `src/decoder.rs`, `src/pixel.rs`, `src/source.rs`, `src/color.rs` | webp 4.24 → 3.39 s, half the bytes per frame | medium, changes the output | implemented, with 04 |
| [04 webp decoder](04-webp-decoder.md) | `Cargo.toml`, `build.rs`, `vcpkg.json`, notices, `LICENSES/`, `src/formats/webp.rs`, `src/decoder.rs` | decode 265 → 111 ms per frame, and it enables 02 and 03 | medium, native dependency | implemented, with 03 |
| [05 monochrome heif](05-monochrome-heif.md) | `src/formats/heif.rs` | the 31 monochrome heic files in `sandbox/heic` decode as `Gray8` instead of failing | low, used to repair an always-failing path | implemented |

01 and 02 are independent of each other. 03 needs 04. 05 is independent of all
of them and is the only one that fixes correctness rather than speed, so it did
not have to wait for a decision on the others. 01 was the only purely internal
change, which made it the right first one to try; it is now in.

expected order of value: 04 + 03 (the only route past bestsource on webp), then
02 (which mostly matters for the same large frames), then the remaining
containers. 01 and 05 are done.

## validating a change

`cargo test --locked` plus the bench on two sets. the png set is the control:
its frames are small, nothing is evicted today, so it must not regress.

```console
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --extra --prefetch 16 --dir "I:/Manga/KamiKatsu/source/v06"
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --extra --dir "I:/Manga/Yuri Love Story/v02" --pattern "Yuri Love Story - v02 - p%03d.png"
```

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
