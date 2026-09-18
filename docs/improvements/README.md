# improvement plans

one file per change, each with the evidence, the intended edit, and how to
check the result. the measurements come from [benchmarks](../BENCH.md) and from
the probes described there.

none of this is implemented yet. every entry is a proposal to review.

## evidence in short

per frame with `prefetch=0` and `debug=True` (see the per stage table in
[BENCH.md](../BENCH.md)):

| set | decode | copy into the frame | serial floor |
| --- | --- | --- | --- |
| webp 2903x4128 | 265 ms | 48 ms | ~53 ms |
| jxl 1500x2500 | 85 ms | 13 ms | ~18 ms |
| jpeg 1404x2000 | 18 ms | 12 ms | ~17 ms |
| png 1404x2000 | 9 ms | 3 ms | ~8 ms |

the *serial floor* is what the requesting thread must do for every frame no
matter how many decoders run behind it: the copy into the frame plus the frame
properties. two conclusions follow.

1. **the lookahead decodes some frames twice.** cpu per delivered frame on the
   webp set: 268 ms at `prefetch=0`, 361 ms at 4, 925 ms at 16, while the wall
   time gets worse after 6. the queue is `workers + 2` frames deep, but the
   192 MiB budget only holds 5.6 of those 36 MB frames. the sandbox set is the
   extreme case: its colour frames are 55 MiB, so the budget holds 3.5 of them,
   and `prefetch=16` spends 2.6x one decode of cpu per delivered frame while
   the default at 4 spends 1.16x.
2. **parallelism cannot beat the floor.** the webp set sits at 88.8 ms/frame
   with four workers, and about 53 ms of that is unavoidable while the copy
   stays on the requesting thread.

a third one, from reading `image-webp` rather than from a measurement: a webp
frame is currently copied twice and allocated three times per frame (the
decoder's own canvas, our zeroed `pixels` buffer, then the frame), which is the
part plan 04 can remove rather than shorten.

## plans

| plan | touches | expected | risk | status |
| --- | --- | --- | --- | --- |
| [01 lookahead scheduling](01-lookahead-scheduling.md) | `src/prefetch.rs` | webp 88.8 → ~70 ms at `prefetch=4`, and `prefetch` above 4 stops being a pessimisation | low, internal only | proposed |
| [02 frame write path](02-frame-write-path.md) | `src/decoder.rs`, `src/source.rs`, `src/pixel.rs` | a few ms per frame from the buffer, and up to 1.6x on webp if the copy leaves the requesting thread | medium, frame lifetime | proposed |
| [03 yuv output for lossy webp](03-webp-yuv-output.md) | decoder path, `src/pixel.rs`, `src/source.rs`, `src/color.rs` | webp ~25-30 ms/frame, half the bytes per frame | medium, changes the output | proposed, needs 04 |
| [04 webp decoder](04-webp-decoder.md) | `Cargo.toml`, `vcpkg.json`, notices, `LICENSES/`, `src/decoder.rs` | decode 265 → ~150 ms per frame, and it enables 02 and 03 | medium, native dependency | proposed |
| [05 monochrome heif](05-monochrome-heif.md) | `src/decoder.rs` | the 31 monochrome heic files in `sandbox/heic` decode as `Gray8` instead of failing | low, repairs an always-failing path | proposed |

01 and 02 are independent of each other. 03 needs 04. 05 is independent of all
of them and is the only one that fixes correctness rather than speed, so it does
not need to wait for a decision on the others. 01 is the only purely internal
change, so it is the natural first one to try.

expected order of value: 01 (safe, steady, helps every large-frame set), then
05 (a format that simply does not work today), then 04 + 03 (the only route
past bestsource on webp), then 02 (which mostly matters for the same large
frames).

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
- **raising `AUTO_MAX_WORKERS` above four by default.** with a 192 MiB budget,
  four workers already fill the five frames that fit. raising the default
  before plan 01 and the budget question is answered would only add duplicate
  work.
