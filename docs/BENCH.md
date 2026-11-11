# Benchmarks

read path only. two plugins reading the same image sequence, one frame at a
time: every measured pass recreates the clips, times each `get_frame` on its own,
and alternates the order of the configurations so a slow period of the machine
cannot favour one of them. every number is the best pass of three (jpeg 2000 is
one pass, because it decodes slowly).

```console
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --plugin target/avx512-bench/release/vs_imageseqs.dll --reps 3 --extra --prefetch 16 --dir sandbox/webp --pattern "snek - p%03d.webp"
```

change `--dir` and the extension in `--pattern` for each set. `--extra` adds the
single-worker rows, `--prefetch N` adds a row with that many lookahead workers.

**machine**: windows 11, i5-11400h (12 threads), rust release build with
`RUSTFLAGS="-C target-cpu=x86-64-v4"` (avx-512), plugin defaults unless the row
says otherwise.

## corpus

`sandbox/` holds the same 35 pages in seven containers (`snek - p000` to
`p034`). `p000` is 3312x4717 and the other 34 are 3672x5274; only `p000`,
`p001` and `p002` hold colour (plus `p018`, which is grey content stored in a
colour format). every set needs `mismatch=True`.

| set | files | what imgseqs returns | decoded |
| --- | --- | --- | --- |
| webp | 35 | 35 `YUV420P8` | 964 MiB |
| avif | 35 | 34 `YUV420P8` and one `Gray8` | 965 MiB |
| heic | 35 | 31 `Gray8` and 4 `YUV420P8` | 685 MiB |
| jp2 | 35 | 32 `Gray8` and 3 `RGB24` | 748 MiB |
| jpeg, png, jxl | 35 | 31 `Gray8` and 4 `RGB24` | 784 MiB |

## fair comparison

- bestsource is opened with `cachemode=0` and both plugins get
  `apply_rotation=False`, so the comparison is the read path alone. bestsource's
  `cachesize`, `threads` and `maxdecoders` stay at the defaults; `threads=1` is
  the row that matches `prefetch=0`.
- bestsource is never given `fpsnum`/`fpsden`: ffmpeg reads image sequences at
  25 fps, so any other rate drops every 25th image.
- the plugins return the same planes only for webp. elsewhere this compares read
  throughput, not identical output (bestsource keeps its decoder's format, for
  example `YUV444P8` for jpeg).
- `open` is the time to create the clip. bestsource indexes the sequence while
  creating it, which reads every file once.

## overall status

total time of the 35 frames (`get_frame` only, seconds, lower is better). "before"
is the number this page held before this re-run; the change is before / now.

| set | default before | default now | change | `prefetch=0` before | now | change | `prefetch=16` before | now | change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| webp | 3.39 | 2.48 | 1.37x faster | 11.94 | 8.58 | 1.39x faster | 1.87 | 1.38 | 1.35x faster |
| jpeg | 2.75 | 2.42 | 1.14x faster | 9.32 | 8.08 | 1.15x faster | 2.04 | 1.74 | 1.17x faster |
| png | 0.322 | 0.282 | 1.14x faster | 0.803 | 0.775 | 1.04x faster | 0.249 | 0.223 | 1.12x faster |
| jxl | 6.50 | 4.12 | 1.58x faster | 18.86 | 13.22 | 1.43x faster | 4.57 | 2.80 | 1.63x faster |
| avif | 2.04 | 1.88 | 1.09x faster | 4.94 | 4.94 | unchanged | 1.99 | 1.87 | 1.06x faster |
| heic | 7.21 | 6.72 | 1.07x faster | 23.20 | 22.57 | 1.03x faster | 4.80 | 4.52 | 1.06x faster |
| jp2 | 13.64 | 13.36 | 1.02x faster | 46.27 | 47.00 | 1.02x slower | not run | 8.95 | |

webp, jpeg, png and jxl are clearly faster, jxl the most (1.4x to 1.6x). avif, heic and jp2 are within a few percent. avif first read 13% to 19% slower in the batch run, but an alternating A/B of the avx-512 build against the plain release build (two rounds, two passes each) put both at 1.9 to 2.1 s default and 4.9 to 5.1 s serial, so that was machine drift and the avif figures here are the best passes of that A/B. jp2's old pooled row used `prefetch=4`, so it has no like for like figure for `prefetch=16`.

against bestsource, default settings, frames only:

| set | imgseqs | bestsource | verdict |
| --- | ---: | ---: | --- |
| webp | 2.48 s | 1.60 s | bestsource 1.55x faster on frames, imgseqs 1.32x faster including the open |
| jpeg | 2.42 s | 6.13 s | imgseqs 2.54x faster, 4.95x including the open |
| png | 0.282 s | 0.661 s | imgseqs 2.34x faster, 5.30x including the open |
| jxl | 4.12 s | 4.33 s | imgseqs 1.05x faster, 2.11x including the open |
| jp2 | 13.36 s | 13.12 s | bestsource 1.02x faster on frames, imgseqs 1.96x faster including the open |
| avif | 1.88 s | cannot open | |
| heic | 6.72 s | cannot open | |

bestsource now decodes jxl on this machine (it failed with `Video codec not
found` before), so jxl has a real comparison for the first time.

## webp

35 files, 35 `YUV420P8` frames of 22.3 MiB (`p000`) and 27.7 MiB.

| reading the set | frames | median frame | open | total |
| --- | ---: | ---: | ---: | ---: |
| imgseqs `Read` | 2.483 s | 74.71 ms | 0.004 s | 2.486 s |
| imgseqs `Read`, `prefetch=0` | 8.576 s | 259.08 ms | 0.004 s | 8.580 s |
| imgseqs `Read`, `prefetch=16` | 1.384 s | 0.10 ms | 0.004 s | 1.388 s |
| bestsource `VideoSource` | 1.601 s | 17.82 ms | 1.674 s | 3.275 s |
| bestsource `VideoSource`, `threads=1` | 10.282 s | 312.11 ms | 10.283 s | 20.565 s |

the only set where both plugins hand out byte-identical planes. bestsource is
faster on frames because ffmpeg spreads one vp8 frame over every core, but it
indexes all 35 files while creating the clip. with one thread each, imgseqs is
1.20x faster (8.58 s against 10.28 s).

## jpeg

35 files, 31 `Gray8` of 18.5 MiB and four `RGB24`.

| reading the set | frames | median frame | open | total |
| --- | ---: | ---: | ---: | ---: |
| imgseqs `Read` | 2.415 s | 65.86 ms | 0.003 s | 2.417 s |
| imgseqs `Read`, `prefetch=0` | 8.077 s | 225.76 ms | 0.002 s | 8.079 s |
| imgseqs `Read`, `prefetch=16` | 1.741 s | 0.08 ms | 0.003 s | 1.743 s |
| bestsource `VideoSource` | 6.127 s | 176.72 ms | 5.847 s | 11.974 s |
| bestsource `VideoSource`, `threads=1` | 6.181 s | 177.89 ms | 5.960 s | 12.141 s |

bestsource gains nothing from its threads here, so the win is imgseqs's lookahead
(3.3x over its own serial row).

## jpeg 2000

35 files: 32 `Gray8` of 18.5 MiB and three `RGB24`. one pass.

**the decoder changed on 2026-10-25**: [43](improvements/43-hayro-jpeg-2000.md)
replaced `jpeg2k` and its vendored OpenJPEG with the pure-Rust
`hayro-jpeg2000`. The rows below are from this machine and that build; the
comparison the swap was accepted on — every frame byte identical, 7.8% faster
at the default window and 1.5x the peak memory at `prefetch=16` — is on an
arm64 machine and is in that plan, not here.

| reading the set | frames | median frame | open | total |
| --- | ---: | ---: | ---: | ---: |
| imgseqs `Read` | 13.363 s | 323.95 ms | 0.020 s | 13.384 s |
| imgseqs `Read`, `prefetch=0` | 47.001 s | 1347.25 ms | 0.004 s | 47.005 s |
| imgseqs `Read`, `prefetch=16` | 8.945 s | 0.13 ms | 0.004 s | 8.949 s |
| bestsource `VideoSource` | 13.120 s | 26.05 ms | 13.081 s | 26.202 s |
| bestsource `VideoSource`, `threads=1` | 78.985 s | 2226.41 ms | 76.966 s | 155.951 s |

**`hayro-jpeg2000` 0.4.1 → 0.5.0**, measured 2026-11-03 on macOS arm64.
Both release builds read the same 35 files from `sandbox/jp2`; each pass ran in
a fresh VapourSynth process, and the build order alternated. These are medians
of three passes. Node creation is excluded from the frame times below.

| `prefetch` | 0.4.1 | 0.5.0 | change |
| ---: | ---: | ---: | ---: |
| 4 (default) | 7.884 s | 7.815 s | 0.9% faster |
| 0 (serial) | 29.492 s | 29.385 s | 0.4% faster |

Both differences are below 1%, so 0.5.0 shows no meaningful load-time
improvement on this corpus. The alternating measurements are in
`target/bench/ab-hayro-jp2.py`.

## png

35 files: 31 `Gray8` and four `RGB24`.

| reading the set | frames | median frame | open | total |
| --- | ---: | ---: | ---: | ---: |
| imgseqs `Read` | 0.282 s | 1.39 ms | 0.003 s | 0.285 s |
| imgseqs `Read`, `prefetch=0` | 0.775 s | 15.06 ms | 0.002 s | 0.777 s |
| imgseqs `Read`, `prefetch=16` | 0.223 s | 0.11 ms | 0.003 s | 0.226 s |
| bestsource `VideoSource` | 0.661 s | 10.19 ms | 0.851 s | 1.512 s |
| bestsource `VideoSource`, `threads=1` | 2.046 s | 44.64 ms | 2.209 s | 4.256 s |

imgseqs also wins the serial comparison (0.775 s against 2.046 s), because the
plugin places the rows itself instead of buffering the picture.

## jxl

35 files: 31 `Gray8` and four `RGB24`.

| reading the set | frames | median frame | open | total |
| --- | ---: | ---: | ---: | ---: |
| imgseqs `Read` | 4.115 s | 87.54 ms | 0.003 s | 4.119 s |
| imgseqs `Read`, `prefetch=0` | 13.220 s | 380.80 ms | 0.003 s | 13.223 s |
| imgseqs `Read`, `prefetch=16` | 2.798 s | 0.12 ms | 0.004 s | 2.801 s |
| bestsource `VideoSource` | 4.328 s | 122.65 ms | 4.377 s | 8.706 s |
| bestsource `VideoSource`, `threads=1` | 13.160 s | 379.85 ms | 13.551 s | 26.712 s |

the `imgseqs` rows above are the build before [49](improvements/49-jxl-decode-threads.md):
with the decode's runner the same set is 2.9 s at the default lookahead and
4.7 s at `prefetch=0`.

### the decode's threads

`jxl`'s decoder takes a `JxlParallelRunner` and this tree passed it none, on all
three `process` calls, which is the crate's `SequentialRunner`: one thread a
page. [49](improvements/49-jxl-decode-threads.md) gives it one, and the
build before it against the build that carries it, alternated over
`sandbox/jxl` in three rounds and the best round each, whole-set wall clock
(`target/bench/plan48/jxl-threads.py`, `jxl-async.py`):

| set | before | after | change |
| --- | ---: | ---: | ---: |
| one decode at a time, `prefetch=0` | 335.4 | 134.6 | 2.49x |
| the default lookahead, `prefetch=4` | 132.1 | 83.1 | 1.59x |
| four `get_frame_async` requests, pool off | 134.4 | 82.8 | 1.62x |

milliseconds a frame. The last row is the consumer that decodes on its own
threads, which is the shape a per-decode budget has to survive: it does, by the
same 1.6x. The width is `available_parallelism().min(4)`, and the four is the
sweep rather than a taste -- with the pool decoding too, four and eight threads
are 1.60x and 1.61x, so eight only buys contention. The stage split moves with
it: `read` was 415 of a 437 ms frame and is 136 of a 155 ms one at four
threads.

## avif

35 files: 34 `YUV420P8` and one `Gray8`. bestsource cannot open the set.

| reading the set | frames | median frame | open | total |
| --- | ---: | ---: | ---: | ---: |
| imgseqs `Read` | 1.881 s | 24.10 ms | 0.004 s | 1.885 s |
| imgseqs `Read`, `prefetch=0` | 4.944 s | 153.38 ms | 0.003 s | 4.948 s |
| imgseqs `Read`, `prefetch=16` | 1.872 s | 0.12 ms | 0.004 s | 1.876 s |

## heic

35 files: 31 `Gray8` and four `YUV420P8`. bestsource cannot open the set.

| reading the set | frames | median frame | open | total |
| --- | ---: | ---: | ---: | ---: |
| imgseqs `Read` | 6.716 s | 175.36 ms | 0.009 s | 6.725 s |
| imgseqs `Read`, `prefetch=0` | 22.566 s | 661.55 ms | 0.010 s | 22.576 s |
| imgseqs `Read`, `prefetch=16` | 4.524 s | 0.09 ms | 0.009 s | 4.533 s |

## decoder

[48](improvements/48-decoder-vectorisation.md) is the plan about the loops this
tree owns in its own decoders rather than the scheduling around them. This is
the aarch64 round of its interleave step: the pass that takes an interleaved
row apart into planes, and the sixteen bit big-endian one a sixteen bit png and
a farbfeld hand over.

measured 2026-11-10 on macOS 27, rustc 1.99.1, one `Read` and one `ReadAlpha`
clip, `prefetch=0`, the `debug=1` stage log, two builds of the same commit
alternated inside one batch (`target/bench/plan48/ab.py`, best of five
rounds). `before` is the commit without the change, so the table is the change
and not the branch. Milliseconds a frame.

| set | read before | read after | total before | total after | change |
| --- | ---: | ---: | ---: | ---: | ---: |
| png rgba16 | 16.56 | 15.38 | 16.65 | 15.47 | 0.93x faster |
| png rgb16 | 15.48 | 14.50 | 15.57 | 14.59 | 0.94x faster |
| png la16 | 8.84 | 8.56 | 8.90 | 8.62 | 0.97x faster |
| png gray16 | 5.87 | 5.57 | 5.92 | 5.62 | 0.95x faster |
| png rgba8 | 8.53 | 7.77 | 8.59 | 7.83 | 0.91x faster |
| png la8 | 3.44 | 3.19 | 3.48 | 3.23 | 0.93x faster |
| png rgb8 | 4.89 | 4.88 | 4.94 | 4.94 | unchanged |
| png gray8 | 2.21 | 2.20 | 2.24 | 2.23 | unchanged |
| farbfeld | 1.77 | 1.25 | 1.81 | 1.28 | 0.71x faster |

the png set is `target/bench/png-depth`, eight 1200x900 pages of each colour
type from `target/bench/make-png-depth-corpus.py`, because `step5-corpus` has
no png page; farbfeld is `step5-corpus`'s eight `.ff` pages. A sixteen bit png
spends most of its frame in inflate, so a loop 4x faster is 3% to 7% of the
frame; farbfeld has nothing to inflate, so the same kernel is 29% of its
frame. The untouched formats were swept the same way and none moved beyond
this machine's noise (bmp 0.99x, dds 1.00x, ppm 0.99x, qoi 0.99x, tiff
1.00x, exr 0.98x, hdr 1.00x, ico 1.03x at 0.12 ms a frame). Those rows are
the arm64 machine's. The x86-64 libraries were measured afterwards, on the
buffered write and on the loops that were still open, below.

### the buffered targa

`tga.rs`'s buffered path is the one the old corpus could not measure: its
eight pages were top-down 24-bit, which the row sink takes. 32 pages of the
four buffered shapes, 1200x900, `target/bench/tga-corpus`
(`target/bench/make-tga-corpus.py`), three alternating rounds:

| shape | read before | read after | change |
| --- | ---: | ---: | ---: |
| raw 24-bit, bottom-up | 0.542 | 0.542 | unchanged |
| 24-bit RLE | 3.779 | 3.805 | unchanged (noise) |
| 16-bit, five bits a channel | 0.575 | 0.257 | 2.24x faster |
| 8-bit palette into a 24-bit map | 2.538 | 1.412 | 1.80x faster |
| all four shapes | 1.852 | 1.496 | 1.24x faster |

the two fast shapes now write each sample in the frame's order while they
widen it, so the pass that exchanged blue and red is gone; the plain memcpy
shapes keep it, because fusing the byte copy in beside the memcpy measured
20% slower. Every page is byte identical, and so are the fifteen fixtures.

measured again on the Windows host, `prefetch=0`, both x86-64 libraries
alternating against a build of the same tree with the fold reverted (the
`src/formats/tga.rs` of the commit before it), best of four rounds. The
baseline library first, then the `.avx512` one:

| shape | library | read before | read after | change |
| --- | --- | ---: | ---: | ---: |
| raw 24-bit, bottom-up | baseline | 2.528 | 2.632 | unchanged |
| 24-bit RLE | baseline | 5.003 | 5.368 | unchanged |
| 16-bit, five bits a channel | baseline | 1.979 | 1.243 | 1.59x faster |
| 8-bit palette into a 24-bit map | baseline | 4.000 | 2.435 | 1.64x faster |
| raw 24-bit, bottom-up | `x86-64-v4` | 2.626 | 2.524 | unchanged |
| 24-bit RLE | `x86-64-v4` | 5.185 | 5.239 | unchanged |
| 16-bit, five bits a channel | `x86-64-v4` | 1.526 | 0.931 | 1.64x faster |
| 8-bit palette into a 24-bit map | `x86-64-v4` | 3.822 | 2.149 | 1.78x faster |

the two plain-copy shapes are unchanged on both libraries: a second look at
eight rounds read 1.02x on the RLE pages and 0.85x on the bottom-up ones,
which is this machine's noise around an unchanged code path, and it is that
second look the `unchanged` cells report. The two folded shapes still gain
1.6x to 1.8x, less than the arm64 machine's 2.24x and 1.80x but the same
direction. Every plane of every frame of the 32 pages is byte identical
between the two builds.

### the sixteen bit apng blend

`apng.rs`'s `blend16` was the one blend loop LLVM left a sample at a time.
`target/bench/apng16-corpus` is two 1024x900 sixteen bit RGBA pages of four
frames, three `over` at a quarter, a half and three quarters coverage, written
by `target/bench/make-apng16-corpus.py`; the row is `fetch`, three rounds:

| build | fetch | change |
| --- | ---: | ---: |
| scalar sample at a time | 6.26 | |
| `vmull_u16` block of four, multiply-high for `/ 65535` | 6.08 | 1.03x |

the loop alone is 1.92 ms a page against a scalar 1.92 ms measured outside the
plugin, which is why the frame moves only 3%: the blend is called a pixel at a
time with that pixel's own alpha, so the setup is half of a block, and the
whole blend is 9% of a presentation. Both pages, the 80 frame eight bit corpus
and the 121 animation fixture frames are byte identical.

the buffered write is the same pass. `src/pixel.rs`'s `write_planes` gathered
one plane at a time from a decoder that buffers its picture; it now fills
every plane a non-transposed frame asks for from one read, with the row
mirrors and the alpha drop folded in, and stops at a transpose or a depth
that needs a shift. Same method and build pairs as the table above:

| set | convert before | convert after | total before | total after | change |
| --- | ---: | ---: | ---: | ---: | ---: |
| apng 1024x1024 rgba8, 8 frames | 0.264 | 0.164 | 1.782 | 1.688 | 0.62x on the write |
| qoi 1200x900 rgb8, 8 pages | 0.240 | 0.197 | 3.831 | 3.809 | 0.82x on the write |
| tiff 1200x900, 8 pages | 0.274 | 0.214 | 6.905 | 6.898 | 0.78x on the write |
| pnm 1200x900, 8 pages | 0.000 | 0.000 | 0.600 | 0.593 | unchanged |

the corpora are `target/bench/apng-corpus` and `target/bench/step5-corpus`
(`target/bench/plan48/ab.py`, four alternating rounds). The read stage does
not move, so a frame's total moves less than its write does; every plane of
every frame of both clips, and of every format under `tests/fixtures`, is byte
identical under the two builds, and both validators pass.

### the buffered write on x86-64

The one-pass `write_planes` was A/B'd on the Windows host against a build of
the same tree that declines it (the walk then takes `write_channel` a plane at
a time), alternating the two builds inside one batch with
`target/bench/plan48/ab.py`, best of five rounds, `prefetch=0`. Milliseconds a
frame; the `convert` stage is the whole of the difference, because the read
stage is not touched.

measured 2026-11-11 on Windows 11, i5-11400H, `x86-64-v4` is the `.avx512`
library. The apng pages are 1024x1024 and the others are 1200x900.

| set | build | convert, one pass | convert, per plane | change |
| --- | --- | ---: | ---: | ---: |
| qoi rgb8, 8 pages | baseline | 1.558 | 1.704 | 1.09x |
| qoi rgb8, 8 pages | `x86-64-v4` | 0.893 | 1.251 | 1.40x |
| tiff, 8 pages | baseline | 1.586 | 1.634 | 1.03x |
| tiff, 8 pages | `x86-64-v4` | 0.913 | 1.114 | 1.22x |
| apng rgba8, 8 frames | baseline | 1.398 | 1.526 | 1.09x |
| apng rgba8, 8 frames | `x86-64-v4` | 0.849 | 1.030 | 1.21x |

the one pass wins on both x86-64 libraries, by less than the arm64 round's
1.2x to 1.6x and never by nothing, so it stays. A frame's total moves 1% to
9% because the read is the larger stage.

### the sixteen bit big-endian walk

`place16be`'s swap was `#[cfg(target_arch = "aarch64")]`, so both x86-64
libraries ran `scalar16be`, and the generated code says the compiler does not
vectorise it there: in the `x86-64-v4` build
`formats::png::Rows::<RowStream>::fill`, which is where a sixteen bit page's row
walk is inlined, has no `pshufb` at all, while `planar::place8::<3>` and `<4>`
carry 21 and 6. The runtime plane count is what stops it: the walk iterates
`planes`, and a slice whose length is only checked to be at most `C` is not
unrolled. The asm is one command, `cargo rustc --release --locked --target
x86_64-pc-windows-msvc -- --emit asm -C target-cpu=x86-64-v4`; `cargo asm`
itself will not read this crate, because the artifact it looks for is an rlib
and this one is a cdylib.

so it has a block now. A block is one thirty-two byte load: the swap is a
shift pair, and a `pshufb` a channel reads that channel's words out of the
load, with the two lanes joined by a dword permute. One, two and four channels
are covered; three has no whole block and keeps the whole-row walk.

| set | library | read before | read after | change |
| --- | --- | ---: | ---: | ---: |
| farbfeld, 8 pages | baseline | 6.188 | 4.614 | 1.34x faster |
| farbfeld, 8 pages | `x86-64-v4` | 5.676 | 4.093 | 1.39x faster |
| png gray16, 4 pages | baseline | 4.319 | 3.734 | 1.16x faster |
| png la16, 4 pages | baseline | 14.441 | 13.724 | 1.05x faster |
| png rgba16, 4 pages | baseline | 29.572 | 29.869 | unchanged |
| png rgb16, 4 pages | baseline | 23.251 | 22.889 | unchanged |

measured 2026-11-11 against a build of the same tree without the block, the two
alternated inside one batch, best of five rounds, `prefetch=0`. The png pages
are `target/bench/png-depth`, which `target/bench/make-png-depth-corpus.py`
writes; they are 7 to 30 ms a frame because a sixteen bit png is inflate
bound, which is why the 30% the block takes off a farbfeld is 5% to 14% of a
page. Three channel pages are the control: the block declines them and they
read unchanged. Every plane of every page of those sets, of `tests/fixtures`'
png pages and of the eight farbfeld pages is byte identical.

### the per-format stages on this host

The plan's evidence table was measured on an x86-64 host before any of its
steps landed. Re-running it on this one, with the tree as it stands and the
same method (`target/bench/plan48/measure.py`, 1200x900 pages of
`target/bench/step5-corpus`, `prefetch=0`, three reps):

| set | decode base | decode `v4` | convert base | convert `v4` | total `v4` |
| --- | ---: | ---: | ---: | ---: | ---: |
| bmp 24-bit | 3.045 | 2.168 | — | — | 2.349 |
| dds DXT5 | 3.114 | 3.385 | 1.597 | 1.040 | 7.029 |
| tga 24-bit | 2.515 | 2.163 | — | — | 2.344 |
| pnm `P6` | 2.908 | 1.872 | — | — | 2.030 |
| qoi | 5.986 | 6.007 | 1.421 | 0.924 | 9.201 |
| hdr | 9.827 | 7.602 | 3.444 | 3.610 | 13.804 |
| farbfeld | 5.310 | 5.287 | — | — | 5.505 |
| tiff | 6.617 | 6.177 | 1.426 | 0.888 | 9.123 |
| exr | 21.543 | 19.207 | 4.187 | 3.852 | 24.253 |
| ico | 0.182 | 0.183 | 0.076 | 0.044 | 0.535 |

**this is a starting point, not a comparison.** the same sets measured hours
apart on this machine differ by up to 40%, and the formats that read the whole
file worst of all (`exr` carries its time in `open`), so a row here and a row
in the evidence table differ by more than anything a landed step could have
done. every claim this round makes is two builds alternated inside one batch,
which is the method the tables above use.

## animation

the gif compositor is the `src/animation/gif.rs` section of
[48](improvements/48-decoder-vectorisation.md). `Canvas::compose` walks the
rectangle a frame states now, one `copy_from_slice` a row, and the per-sample
`blend` became a row walk whose disposal branch is hoisted; on aarch64 the row
is a `vbslq_u32` select four pixels a block. `place`, the still gif path, got
the same rectangle row walk.

measured 2026-11-10 on macOS 27, rustc 1.99.1, `prefetch=0`, the `debug=1`
stage log, two builds of the same commit alternated (`target/bench/plan48/
measure.py`, best of three alternating runs, five reps each). Milliseconds a
frame; the animation cost is the `fetch` stage and the total includes the
frame the worker builds. The gif and webp rows under this heading are the
arm64 machine's; the x86-64 halves of the compose row were written, measured
and declined afterwards, and that measurement is the subsection below.

| set | fetch before | fetch after | total before | total after | change |
| --- | ---: | ---: | ---: | ---: | ---: |
| gif animation, 1024x1024, 16 frames | 2.00 | 1.57 | 2.05 | 1.60 | 0.78x faster |
| gif still, 1024x1024 | 4.45 | 3.89 | 4.45 | 3.90 | 0.88x faster |
| webp animation, 1024x1024, 16 frames | 1.92 | 1.64 | 1.92 | 1.64 | 0.85x faster |

The gif rows compare this commit with the one before it; the webp row compares
a build of the same tree with the reciprocal table and without it, because the
write it sits beside had already moved.

### the shared canvas

`Pixels::Interleaved`'s buffer is an `Arc<[u8]>` now, so the presentation cache
hands out a pointer rather than a canvas. Measured against a build of the
previous commit, same corpus and method, three alternating rounds:

| set | fetch before | fetch after | change |
| --- | ---: | ---: | ---: |
| gif animation, 1024x1024 | 1.50 | 1.47 | 2% faster |
| webp animation, 1024x1024 | 1.58 | 1.55 | 2% faster |
| apng, 1024x1024 | 1.59 | 1.53 | 4% faster |

the copy removed is about 2 MB an output frame at these sizes, which memory
bandwidth prices at about 0.05 ms. It is kept because it is a copy less and
because sharing the canvas is what a later step builds on, not because it was
the frame's cost. Every frame is byte identical.

the corpus is `target/bench/plan48/anim-corpus`. Every plane of every frame
of both clips is byte identical under the two builds, and `tests/readalpha.vpy`
passes its 1034 checks. The compose loop itself went 0.897 to 0.424 to 0.237 ms
for a full-canvas `Keep` frame and 0.912 to 0.425 to 0.188 ms for a `Background`
one: the first step is the rectangle walk, the second the NEON select, and the
x86-64 libraries keep the first and not the second.

### the animation rows on this host

The same corpora measured on the Windows host, both libraries, `prefetch=0`,
`target/bench/plan48/measure.py`, three reps. One run each, so these are the
other host's column rather than a comparison with the arm64 numbers above:
they are three to five times the arm64 `fetch`, which is the `gif` crate's
LZW and the four megabyte canvases rather than anything the compose does.

| set | library | fetch | convert | total |
| --- | --- | ---: | ---: | ---: |
| apng, 1024x1024, 80 frames | baseline | 4.683 | 1.438 | 4.691 |
| apng, 1024x1024, 80 frames | `x86-64-v4` | 4.057 | 0.839 | 4.065 |
| gif animation, 31 frames | baseline | 6.012 | 1.543 | 6.020 |
| gif animation, 31 frames | `x86-64-v4` | 5.214 | 0.914 | 5.222 |
| gif still, 1024x1024 | baseline | 12.891 | 1.412 | 12.899 |
| gif still, 1024x1024 | `x86-64-v4` | 11.131 | 0.888 | 11.138 |
| webp animation, 31 frames | baseline | 7.841 | 1.535 | 7.849 |
| webp animation, 31 frames | `x86-64-v4` | 7.245 | 0.958 | 7.253 |
| sixteen bit apng, 1024x900, 4 frames | baseline | 29.035 | 2.788 | 29.046 |
| sixteen bit apng, 1024x900, 4 frames | `x86-64-v4` | 26.784 | 2.091 | 26.809 |

the last two are `target/bench/apng16-corpus`, which
`target/bench/make-apng16-corpus.py` writes by hand: nothing on this machine
emits an APNG, so the container's `acTL`, `fcTL`/`IDAT` and `fdAT` chunks are
assembled there. A frame is 29 ms against the arm64 round's 6.26, and the
sixteen bit blend is the scalar loop on x86-64, which is why the arm kernel's
3% has no x86-64 column to sit beside.

### the compose row on x86-64

`blend_row`'s AVX2 and AVX-512 blocks were written for the Windows host and are
**not landed**: on this machine the row already runs at the memory system's
ceiling, so a block that makes its arithmetic cheaper moves nothing. This is
the measurement that decided it, and `src/animation/gif.rs`'s ignored
`the_compose_measured` test is how to ask again.

The row alone, over a canvas `w` columns by `h` rows, best of 20 passes, one
canvas a pass, one run each, `prefetch=0`. `scalar` is the walk both x86
libraries run today, and the two blocks are named as the dispatch names them.
This host's run-to-run spread on this loop is wide, up to 40% at the largest
canvas, which is why the test alternates the paths round by round now and why
the decision rests on the end-to-end table below rather than on any one cell.

| canvas | build | scalar | avx2 | avx512 | scalar -> avx512 |
| --- | --- | ---: | ---: | ---: | ---: |
| 256x256 | baseline | 0.371 | 0.150 | 0.142 | 2.6x |
| 256x256 | `x86-64-v3` | 0.195 | 0.145 | 0.116 | 1.7x |
| 256x256 | `x86-64-v4` | 0.125 | 0.124 | 0.102 | 1.2x |
| 1024x1024 | baseline | 1.291 | 0.749 | 0.647 | 2.0x |
| 1024x1024 | `x86-64-v3` | 0.607 | 0.593 | 0.605 | 1.0x |
| 1024x1024 | `x86-64-v4` | 0.709 | 0.629 | 0.795 | 0.9x |
| 4096x4096 | baseline | 0.736 | 0.695 | 0.728 | 1.0x |
| 4096x4096 | `x86-64-v3` | 0.721 | 0.707 | 0.723 | 1.0x |
| 4096x4096 | `x86-64-v4` | 1.010 | 0.981 | 0.884 | 1.1x |

nanoseconds a pixel. The two wide builds already vectorise this loop: at
1024x1024 the baseline library's walk is 1.29 ns a pixel and theirs are 0.61
and 0.71, which is what the best block costs there (0.59 to 0.63). So the only
library where a hand block wins is the baseline one, which is step 0's result
again, and the only canvas where it wins is one small enough to sit in a cache.

The reason is the traffic rather than the arithmetic: a canvas is two four
megabyte buffers read and written, twelve bytes a pixel, and 0.65 ms of that is
19 GB/s, this host's single-core memory ceiling. A block removes work the
memory system is already waiting on. It is the opposite of the arm64 machine,
whose bandwidth per core left the scalar walk arithmetic bound and a NEON block
worth 1.8x.

End to end on the gif animation (31 output frames of 16 pictures, ten
alternating rounds, the best round each, `target/bench/plan48/ab-fetch.py`):

| library | fetch before | fetch after | change |
| --- | ---: | ---: | ---: |
| baseline | 4.922 | 4.930 | unchanged |
| `x86-64-v3` | 4.367 | 4.396 | unchanged |
| `x86-64-v4` | 4.365 | 4.338 | unchanged |

and a control build whose `blend_row` returns at once, so the whole blend is
skipped rather than its arithmetic, moves the same row from 4.716 to 4.400
(0.93x): every read and write the blend does is 0.32 ms of a 4.7 ms frame. A
block can only take the part of that which is arithmetic, and on this host that
part is small enough that ten alternating rounds cannot see it.


## PNGWrite

write path, measured earlier and not re-run here. [38 PNGWrite](improvements/38-png-write.md)
is the plan. `target/bench/png-write.py` and `target/bench/png-write-async.py` run
it. 1404x2000 synthetic pages, one warm-up and five measured passes with the route
order rotated, i5-11400h, Python 3.12, Pillow 12.3 over zlib-ng. Plugin is
`compression=6` over `zlib-rs`. Every route's outputs were decoded and compared
with the input frames, and all agree.

routes: the plugin asked for one frame at a time, Pillow saving serially, and
Pillow with six worker threads and oldest-future backpressure (the shape of
nmanga's `BoundedWritePool`; six is half the 12 logical cores). the headline is
wall time from the first request to the last closed file, with the plugin's safe
publication (temporary file, then rename or link) included.

| cohort | plugin, 1 in flight | Pillow serial | Pillow bounded (6) | PNG MiB plugin / Pillow |
| --- | ---: | ---: | ---: | ---: |
| gray8, 24 pages | 0.686 s | 0.960 s | 0.236 s | 3.61 / 3.60 |
| rgb8, 16 pages | 0.713 s | 1.342 s | 0.365 s | 4.66 / 4.65 |
| rgba8, 16 pages | 0.857 s | 1.799 s | 0.504 s | 5.81 / 5.81 |

one encoder beats serial Pillow by 1.4x (gray), 1.9x (rgb) and 2.1x (rgba) at the
same size. six Pillow workers beat one plugin request by 1.7x to 2.9x, because
that is six encodes in flight against one. the plugin's completion mode is
`Parallel`, so a consumer that keeps frames in flight gets them too (gray8, 24
pages):

| plugin | in flight | median s | vs Pillow bounded (6) |
| --- | ---: | ---: | ---: |
| `get_frame(n)` | 1 | 0.700 | 3.00x slower |
| `frames(prefetch=6)` | 6 | 0.171 | 1.37x faster |
| `get_frame_async(12)` | 12 | 0.124 | 1.88x faster |

six in flight is 4.3x one and twelve is 5.6x; twenty four buys another 5%. the
old `ParallelRequests` mode stayed at 1.0 s whatever the depth, because the
encode runs in the second half of the request and that mode serializes it.

**deflate backend**: `zlib-rs` (through `png`'s feature) replaced `miniz_oxide`:
1.46x faster serially, 1.33x at twelve deep, files 7.5% smaller, which closed the
size gap to Pillow. the png reader pays 1.6% for it, inside the spread. levels,
sequential / twelve deep / size: 0 = 0.139 / 0.046 s / 64.52 MiB, 1 = 0.223 /
0.061 s / 7.37 MiB, 3 = 0.347 / 0.088 s / 5.02 MiB, **6 (default) = 0.700 /
0.124 s / 3.61 MiB**, 9 = 5.091 / 0.674 s / 3.11 MiB. level 6 matches Pillow's
size and still beats it on time.

**yuv conversion**: six `sandbox/webp` pages (YUV420P8, 3672x5274), the writer
converting by itself against `resize.Bilinear` in front of it. the vector
kernels (AVX2 / AVX-512 on x86-64, NEON on aarch64) made the writer's own route
2.56x faster on x86-64 (11.656 s to 4.975 s) and 1.27x on aarch64, which puts it
level with the `resize` route (0.98x on x86-64, 1.01x on arm) and ahead of Pillow
(7.04 s). per kernel, ns per luma sample, 3672x5274 noise, `cargo test --release
--lib -- --ignored --nocapture the_kernels_measured`:

| x86-64 | scalar | AVX2 | AVX-512 |
| --- | ---: | ---: | ---: |
| 8 bit 4:2:0, centred | 56.73 | 1.372 | 1.343 |
| 8 bit 4:2:0, left | 22.27 | 1.290 | 1.026 |
| 8 bit 4:4:4 | 9.79 | 0.937 | 0.766 |
| 10 bit 4:2:0, left | 23.16 | 1.540 | 1.048 |

| aarch64 | scalar | NEON |
| --- | ---: | ---: |
| 8 bit 4:2:0, centred | 6.16 | 1.245 |
| 8 bit 4:2:0, left | 6.28 | 1.258 |
| 8 bit 4:4:4 | 4.02 | 0.916 |
| 10 bit 4:2:0, left | 7.83 | 1.196 |

that is 12x to 42x the scalar loop on x86-64 and 4.4x to 6.6x on arm, whose
scalar loop is already 2.4x to 9x faster. every kernel is held to the scalar path
sample for sample, and whole files from the two builds are byte identical.
gray and rgb routes did not move (0.680 / 0.663 s before against 0.682 / 0.673 s
after, 24 gray8 pages). `output_dir` only changes naming and did not move the
tables either.

## read path, several frames in flight

the tables above are a serial consumer: one `get_frame` at a time, with
`prefetch` frames decoded ahead of it. an async consumer is a different regime,
because `get_frame_async(n)` keeps N requests open and each of them may claim
and decode its own frame on the calling thread (`Prefetcher::fetch`) instead of
waiting for the pool to arrive at it.

`target/bench/read-async.py` is the measurement, re-recorded on the build
[45](improvements/45-async-request-cache.md) and
[47](improvements/47-claim-budget-accounting.md) leave behind. `sandbox/webp`,
all 35 pages, 3672x5274 `YUV420P8` and 27.7 MiB decoded per frame, best pass of
three:

| request style | wall | ms/frame | cpu/frame |
| --- | ---: | ---: | ---: |
| `prefetch=0`, serial | 9.310 s | 265.99 | 263 |
| default (4), serial | 2.693 s | 76.94 | 303 |
| default (4), async 4 | 2.634 s | 75.25 | 294 |
| default (4), async 8 | 1.689 s | 48.24 | 325 |
| default (4), async 12 | 1.601 s | 45.74 | 342 |
| `prefetch=0`, async 8 | 1.747 s | 49.91 | 334 |
| `prefetch=16`, serial | 1.513 s | 43.23 | 360 |
| `prefetch=16`, async 8 | 1.425 s | 40.71 | 348 |

four background workers and none are close to the same row at eight frames in
flight: 1.689 s against 1.747 s, which is the spread of the machine rather than
a gain. the default serial read is 1.59x slower than the same pool driven by a
consumer that keeps eight frames in flight, and the parallelism that does it is
the consumer's own threads. a deep pool pays in both regimes, but more to the
serial one: 2.693 s to 1.513 s is 1.78x, against 1.626 s to 1.324 s, which is
1.23x, for a consumer with eight frames in flight.
[46](improvements/46-request-thread-decode.md) is the attempt to move that work
onto the pool itself, measured and rejected.

`ReadAlpha`, both clips of every frame, same set:

| two clips of one call | wall | ms per delivered frame | cpu per delivered frame |
| --- | ---: | ---: | ---: |
| interleaved, default | 2.736 s | 78.17 | 301 |
| branches, default | 7.956 s | 227.31 | 448 |
| interleaved, async 8 | 2.067 s | 59.04 | 317 |
| branches, async 8 | 3.128 s | 89.37 | 565 |
| branches, async 8, `prefetch=16` | 1.566 s | 44.75 | 363 |

asking both clips for the same frame is free. walking one clip to the end and
then the other decodes every file twice, because the distance between the two
requests is the sequence length and no bounded cache holds 35 payloads.

`prefetch` is the other half of it. the pool's window is `workers + 2`, so the
default four covers six frames; a consumer that keeps more than that in flight
outruns it and decodes on its own threads instead. `Read`, async 8, best of
five, one run:

| `prefetch` | wall | ms/frame | cpu/frame |
| ---: | ---: | ---: | ---: |
| 0 | 1.659 s | 47.40 | 310 |
| 2 | 1.713 s | 48.95 | 326 |
| 4 (default) | 1.626 s | 46.45 | 305 |
| 6 | 1.493 s | 42.65 | 311 |
| 8 | 1.463 s | 41.81 | 310 |
| 10 | 1.372 s | 39.20 | 333 |
| 12 | 1.324 s | 37.84 | 338 |
| 14 | 1.522 s | 43.47 | 364 |
| 16 | 1.492 s | 42.62 | 353 |
| 20 | 1.452 s | 41.50 | 361 |

the last three rows are this machine's drift and not a cost: a paired A/B of 12
against 16, alternating inside one process, puts both at 1.444 s. so the curve
is about 1.2x from the default to `prefetch=12` and flat after it, and 20 is 16
because `MAX_WORKERS` and the window both stop at 16. peak working set is
1065 MiB at the default against about 1130 MiB at 12.

a two-clip call wants the same depth. `ReadAlpha`, async 8, `prefetch` 4, 8, 12,
16: interleaved 1.757 / 1.423 / 1.254 / 1.200 s and branches 2.855 / 1.905 /
1.327 / 1.297 s.

an alternating A/B of the two builds, before 45 and 47 against after, five
separate processes each, because one long run heats this machine and moves both
builds by about 20%: the `prefetch=16` row above re-measures at 1.962 to 2.141 s
and 470 to 485 cpu per delivered frame before, against 1.175 to 1.368 s and 355
to 365 after. about 1.6x on the wall, and the cpu is the duplicate decode
itself rather than the wait it caused. the `ReadAlpha` table is one long run
and sits 15 to 20% above those floors.

`Read` at the same settings does not move: 1.398 to 1.441 s before against 1.425
to 1.428 s after, six alternating runs of five each, because `Read` has one
consumer and 45 is about the second.

`--lag` sweeps the distance between the two clips, and it is the row
[45](improvements/45-async-request-cache.md) was judged on: colour frame `n`
asked for together with alpha frame `n - lag`.

| lag | wall | cpu/frame | x one shared decode |
| ---: | ---: | ---: | ---: |
| 0 | 2.798 s | 304 | 1.00 |
| 4 | 2.761 s | 304 | 1.00 |
| 8 | 2.741 s | 305 | 1.00 |
| 10 | 6.332 s | 460 | 1.52 |
| 16 | 6.749 s | 455 | 1.50 |

before 45 the crossing was lag 1 and the cost beyond it was 4.2x.

peak working set of the whole process, one configuration per process, with
`--memory`:

| request style | peak |
| --- | ---: |
| `prefetch=0`, serial | 697 MiB |
| default (4), serial | 909 MiB |
| default (4), async 8 | 1065 MiB |
| default (4), async 12 | 1104 MiB |
| `prefetch=16`, async 8 | 1123 MiB |

the async rows sit above the 192 MiB the pool budgets, because a frame a
consumer claimed is the graph's to hold: `prefetch_memory` bounds the pool's
cache and queue, not the graph.

the run-to-run spread at fixed settings is up to 20% on this machine, so the
absolute numbers are one set of runs and only the same-run comparisons are safe
to read as ratios.
