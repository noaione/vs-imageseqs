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
| avif | 2.04 | 2.36 | 1.16x slower | 4.94 | 5.58 | 1.13x slower | 1.99 | 2.37 | 1.19x slower |
| heic | 7.21 | 6.72 | 1.07x faster | 23.20 | 22.57 | 1.03x faster | 4.80 | 4.52 | 1.06x faster |
| jp2 | 13.64 | 13.36 | 1.02x faster | 46.27 | 47.00 | 1.02x slower | not run | 8.95 | |

webp, jpeg, png and jxl are clearly faster, jxl the most (1.4x to 1.6x). heic and
jp2 are within a few percent. avif is the one set that came out slower than the
number it replaces (13% to 19%); the earlier avif rows were already noted as
sensitive to drift on this machine, so re-check it before treating it as a real
regression. jp2's old pooled row used `prefetch=4`, so it has no like for like
figure for `prefetch=16`.

against bestsource, default settings, frames only:

| set | imgseqs | bestsource | verdict |
| --- | ---: | ---: | --- |
| webp | 2.48 s | 1.60 s | bestsource 1.55x faster on frames, imgseqs 1.32x faster including the open |
| jpeg | 2.42 s | 6.13 s | imgseqs 2.54x faster, 4.95x including the open |
| png | 0.282 s | 0.661 s | imgseqs 2.34x faster, 5.30x including the open |
| jxl | 4.12 s | 4.33 s | imgseqs 1.05x faster, 2.11x including the open |
| jp2 | 13.36 s | 13.12 s | bestsource 1.02x faster on frames, imgseqs 1.96x faster including the open |
| avif | 2.36 s | cannot open | |
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

## avif

35 files: 34 `YUV420P8` and one `Gray8`. bestsource cannot open the set.

| reading the set | frames | median frame | open | total |
| --- | ---: | ---: | ---: | ---: |
| imgseqs `Read` | 2.358 s | 24.10 ms | 0.005 s | 2.363 s |
| imgseqs `Read`, `prefetch=0` | 5.577 s | 153.38 ms | 0.007 s | 5.584 s |
| imgseqs `Read`, `prefetch=16` | 2.374 s | 0.12 ms | 0.004 s | 2.378 s |

## heic

35 files: 31 `Gray8` and four `YUV420P8`. bestsource cannot open the set.

| reading the set | frames | median frame | open | total |
| --- | ---: | ---: | ---: | ---: |
| imgseqs `Read` | 6.716 s | 175.36 ms | 0.009 s | 6.725 s |
| imgseqs `Read`, `prefetch=0` | 22.566 s | 661.55 ms | 0.010 s | 22.576 s |
| imgseqs `Read`, `prefetch=16` | 4.524 s | 0.09 ms | 0.009 s | 4.533 s |


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
