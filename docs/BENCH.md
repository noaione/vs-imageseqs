# Benchmarks

two plugins reading the same image sequence, one frame at a time. every
measured pass recreates both clips, times each `get_frame` on its own, and
alternates the order of the configurations so a slow period of the machine
cannot favour one of them. the numbers below are the best pass of three unless
the row says otherwise.

```console
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3
```

`--extra` adds the single-worker variants, `--prefetch N` adds an imgseqs run
with that many lookahead workers, `--frames N` limits the run to the first `N`
files, and `--select GLOB` picks the files when the glob of `--pattern` is not
the wanted selection. the lookahead tables also record process CPU time per
frame; those two probes live outside the repository.

**machine**: windows 11, i5-11400h (12 threads), rust release build, plugins at
their own defaults unless the row says otherwise.

## fair comparison

- bestsource is opened with `cachemode=0` (never read or write an index on
  disk) and `apply_rotation=False` (imgseqs records the orientation but never
  transforms the picture). its `cachesize`, `threads` and `maxdecoders` stay at
  the defaults, and `threads=1` is the row that matches `prefetch=0`.
- bestsource is never given `fpsnum`/`fpsden` for an image sequence: ffmpeg
  reads image sequences at 25 fps, so any other rate resamples and silently
  drops every 25th image (163 files become 156 frames).
- the two plugins do not produce the same frames. imgseqs writes `RGB24` for
the webp and jpeg sets and `GRAY8` for the grayscale png set, while bestsource
keeps whatever format its decoder emits: `YUV420P8` for webp, `YUV444P8` for
jpeg, and a variable format for the png set. this compares read throughput, not
identical output.
- `open` is the time to create the clip. bestsource indexes the sequence while
  the clip is created, which for images means reading every file once.

## webp

163 files of 2903x4128, about 1 MB each, with a few different widths, so
`mismatch=True`.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 15.80 s | 68 ms | 0.01 s | 15.81 s |
| imgseqs `Read`, `prefetch=0` | 54.29 s | 331 ms | 0.01 s | 54.30 s |
| imgseqs `Read`, `prefetch=16` | 27.53 s | 84 ms | 0.01 s | 27.54 s |
| bestsource `VideoSource` | 5.96 s | 15 ms | 5.95 s | 11.90 s |
| bestsource `VideoSource`, `threads=1` | 30.41 s | 177 ms | 29.29 s | 59.70 s |

bestsource wins on the decoder: ffmpeg spreads one vp8 frame across every core,
so `threads=0` turns 177 ms per frame into 15 ms, while imgseqs decodes a frame
in a single pure rust thread. its indexing pass is what keeps the totals close:
with one worker each, imgseqs finishes sooner, 54.30 s against 59.70 s.

lookahead depth, first 32 files, wall and process CPU per frame:

| prefetch | wall | cpu | cpu over one decode |
| --- | --- | --- | --- |
| 0 | 272.5 ms | 268.1 ms | 1.00x |
| 1 | 227.6 ms | 262.2 ms | 0.98x |
| 2 | 128.5 ms | 285.6 ms | 1.07x |
| 4 | 88.8 ms | 360.8 ms | 1.35x |
| 6 | 78.2 ms | 431.2 ms | 1.61x |
| 8 | 95.6 ms | 595.7 ms | 2.22x |
| 12 | 112.6 ms | 752.9 ms | 2.81x |
| 16 | 136.2 ms | 925.3 ms | 3.45x |

one frame is 36 MB, so the 192 MiB decode budget holds about five of them while
the pool queues `workers + 2` frames ahead. past four workers the queue outruns
the budget: finished frames are evicted before they are used and decoded again,
which is why the CPU column climbs to several decodes per delivered frame, and
why `prefetch=16` ends up slower than `prefetch=4`.

## jpeg

130 files of 1404x2000, about 0.5 MB each.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 1.26 s | 9.6 ms | 0.04 s | 1.30 s |
| imgseqs `Read`, `prefetch=0` | 2.89 s | 22.4 ms | 0.05 s | 2.94 s |
| bestsource `VideoSource` | 2.21 s | 17.2 ms | 1.73 s | 3.93 s |
| bestsource `VideoSource`, `threads=1` | 2.17 s | 16.3 ms | 1.85 s | 4.03 s |

imgseqs wins here, 1.75x on frames and 3.02x including the open. the two
single-worker rows show why: ffmpeg still decodes a jpeg frame a little faster
than `zune-jpeg` does (16.3 ms against 22.4 ms) and bestsource gains nothing
from its threads for jpeg, while imgseqs turns its lookahead into a 2.3x
speedup over its own serial path. the win comes from parallelism, not from a
faster decoder.

lookahead depth, first 64 files:

| prefetch | wall | cpu | cpu over one decode |
| --- | --- | --- | --- |
| 0 | 23.9 ms | 22.9 ms | 1.00x |
| 1 | 10.9 ms | 15.6 ms | 0.68x |
| 2 | 10.1 ms | 23.4 ms | 1.02x |
| 4 | 10.8 ms | 26.4 ms | 1.15x |
| 8 | 11.2 ms | 28.6 ms | 1.25x |
| 16 | 12.1 ms | 27.8 ms | 1.21x |

a jpeg frame is 8 MB, so the same budget holds about 22 of them and nothing has
to be evicted: CPU per frame stays near a single decode at every depth. the wall
time still flattens at roughly 10 ms per frame, the part of the path that cannot
overlap, which is copying the decoded pixels into the VapourSynth frame and the
per-frame bookkeeping.

## png

130 files of 1404x2000, grayscale scans (`Gray8`), about 800 KB each.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 0.91 s | 9.0 ms | 0.01 s | 0.92 s |
| imgseqs `Read`, `prefetch=0` | 2.06 s | 16.5 ms | 0.01 s | 2.07 s |
| bestsource `VideoSource` | 0.80 s | 6.3 ms | 0.41 s | 1.21 s |
| bestsource `VideoSource`, `threads=1` | 2.03 s | 16.2 ms | 1.57 s | 3.60 s |

the closest match of the four sets. one decode worker each is a dead heat,
16.5 ms against 16.2 ms, so the 1.14x bestsource lead on frames is its own
parallelism, and imgseqs takes the total back to 1.32x because it skips the
indexing pass.

the same folder also holds two jpeg pages, `p000.jpg` and `p131.jpg`, in front
of and behind the 130 png pages. imgseqs reads all 132 as one clip at 0.90 s,
7.96 ms per frame; they share a size but not a format, so it needs
`mismatch=True`. bestsource cannot open that sequence at all, because one
printf pattern describes one extension. the bench says so and reports the
imgseqs numbers on their own:

```console
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --extra --dir "target/folder" --select "p*.*"
```

## jxl

29 files of 1500x2500 and 1500x824, manhua pages, so `mismatch=True`.
about 300 KB each.

bestsource cannot read this set at all: `bs.VideoSource` fails with
`Video codec not found`, because that ffmpeg build has no jpeg xl decoder. the
bench reports the failure and then measures imgseqs on its own.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 0.83 s | 15.2 ms | 0.00 s | 0.84 s |
| imgseqs `Read`, `prefetch=0` | 2.45 s | 88.1 ms | 0.00 s | 2.46 s |

the lookahead is worth 2.95x here. a frame costs 85 ms of decoding and 13 ms of
copying, which per pixel is the same rate as the pure rust webp decoder and
about three times slower than `zune-jpeg`.

## per stage cost

`debug=True` with `prefetch=0`, averaged over the first frames of each set:

| set | decode | copy into the frame | total |
| --- | --- | --- | --- |
| webp 2903x4128 | 265 ms | 48 ms | 314 ms |
| jpeg 1404x2000 | 18 ms | 12 ms | 30 ms |
| png 1404x2000 | 9 ms | 3 ms | 12 ms |
| jxl 1500x2500 | 85 ms | 13 ms | 98 ms |

the decode figure includes the first-touch page faults of the decode buffer,
36 MB for the webp set and 2.8 MB for the grayscale png set, which is why
allocating that buffer measures as ~0 ms on its own.

## known headroom

- the webp decoder is pure rust and single threaded, so a native decoder
  (libwebp, or ffmpeg) is the only way to close the per-frame gap there.
- the lookahead queues more frames than the byte budget can hold, so the same
  frames get decoded twice. sizing the window from the budget, instead of from
  the worker count, would remove that.
- the copy into the frame runs on the requesting thread. for jpeg it is already
  the floor of the curve above.
