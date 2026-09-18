# Benchmarks

two plugins reading the same image sequence, one frame at a time. every
measured pass recreates both clips, times each `get_frame` on its own, and
alternates the order of the configurations so a slow period of the machine
cannot favour one of them. the numbers below are the best pass of three unless
the row says otherwise.

```console
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --dir sandbox/webp --pattern "snek - p%03d.webp"
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
  drops every 25th image.
- the two plugins do not produce the same frames. imgseqs returns the image's
  own format when the file has one (`GRAY8` for monochrome jpeg, png and jxl
  pages, `RGB24` for colour ones) and converts the yuv containers to `RGB24`,
  while bestsource keeps whatever format its decoder emits: `YUV420P8` for
  webp, `YUV444P8` for jpeg, a variable format for a mixed folder. this compares
  read throughput, not identical output.
- `open` is the time to create the clip. bestsource indexes the sequence while
  the clip is created, which for images means reading every file once.

## sandbox corpus

`sandbox/` holds the same 35 pages in six containers, so the formats can be
compared like for like. the pages are not uniform: `p000` is 3312x4717 and the
other 34 are 3672x5274, of which 31 are monochrome. it is the same four colour
pages (`p000`, `p001`, `p002`, `p018`) and the same 31 monochrome ones in every
set; what imgseqs makes of them depends on the container:

| set | what imgseqs returns | decoded |
| --- | --- | --- |
| webp, avif | 35 `RGB24` frames | 1928 MiB, one 44.7 MiB frame and 34 of 55.4 MiB |
| jpeg, png, jxl, heic | 31 `Gray8` frames of 18.5 MiB and 4 `RGB24` | 784 MiB |

every set needs `mismatch=True` for this, and those frame sizes (18.5 to
55.4 MiB) are what the 192 MiB lookahead budget is up against: it holds ten of
the gray frames but only three and a half of the colour ones.

```console
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --extra --prefetch 16 --dir sandbox/webp --pattern "snek - p%03d.webp"
```

frames, best pass of three:

| set | imgseqs | bestsource | verdict |
| --- | --- | --- | --- |
| webp | 5.88 s | 2.17 s | bestsource 2.71x faster, 1.28x including the open |
| jpeg | 2.87 s | 7.52 s | imgseqs 2.62x faster, 4.81x including the open |
| png | 0.66 s | 0.89 s | imgseqs 1.36x faster, 2.88x including the open |
| jxl | 6.42 s | cannot read | 3.02x over imgseqs's own serial row |
| avif | 9.25 s | cannot open | 1.22x over imgseqs's own serial row |
| heic | 7.50 s | cannot open | 3.21x over imgseqs's own serial row |

one pattern holds across all of them: the cheaper a frame is to decode, the more
a deep lookahead is worth, so `prefetch=16` wins on jpeg and jxl and loses by 2x
on webp and avif. the sections below have the detail.

## webp

35 files, 146 MB, all colour: `p000` is 3312x4717, 44.7 MiB decoded, and the
other 34 are 3672x5274 at 55.4 MiB.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 5.88 s | 103.6 ms | 0.003 s | 5.89 s |
| imgseqs `Read`, `prefetch=0` | 20.21 s | 617.0 ms | 0.004 s | 20.22 s |
| imgseqs `Read`, `prefetch=16` | 11.36 s | 107.4 ms | 0.004 s | 11.36 s |
| bestsource `VideoSource` | 2.17 s | 23.9 ms | 2.42 s | 4.59 s |
| bestsource `VideoSource`, `threads=1` | 11.92 s | 371.4 ms | 11.81 s | 23.74 s |

bestsource is 2.71x faster on frames and 1.28x including the open, and its
decoder is the whole reason: ffmpeg spreads one vp8 frame over every core, so
`threads=1` to `threads=0` turns 371 ms per frame into 24 ms, while imgseqs
decodes a frame in a single pure rust thread. imgseqs turns its own lookahead
into 3.4x over its serial row, which is not enough to close that gap. this is
the set [04](improvements/04-webp-decoder.md) is for.

lookahead depth, first 16 files, wall clock and process CPU per delivered frame
(one serial frame is 590 ms of CPU):

| prefetch | wall | cpu | cores busy | cpu over serial |
| --- | --- | --- | --- | --- |
| 0 | 590.4 ms | 585.0 ms | 0.99 | 1.00x |
| 2 | 329.4 ms | 685.5 ms | 2.08 | 1.17x |
| 4 | 169.1 ms | 680.7 ms | 4.02 | 1.16x |
| 8 | 245.4 ms | 1232.4 ms | 5.02 | 2.10x |
| 16 | 297.4 ms | 1513.7 ms | 5.09 | 2.58x |

at the default the pool is fully busy and wastes little; at 16 it is busy
re-decoding frames that were evicted before anyone asked for them. a colour
frame is 55.4 MiB, so the 192 MiB budget holds three and a half of them and the
wall time stops improving past 4: the budget caps what the consumer can be
handed, which is what [01](improvements/01-lookahead-scheduling.md) changes.

## jpeg

35 files, 213 MB. 31 of the pages are monochrome and come out as `Gray8` of
18.5 MiB; the four colour ones are `RGB24` (`p000` at 44.7 MiB, `p001`, `p002`
and `p018` at 55.4 MiB).

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 2.87 s | 44.3 ms | 0.20 s | 3.07 s |
| imgseqs `Read`, `prefetch=0` | 9.42 s | 261.8 ms | 0.20 s | 9.62 s |
| imgseqs `Read`, `prefetch=16` | 2.36 s | 11.7 ms | 0.20 s | 2.56 s |
| bestsource `VideoSource` | 7.52 s | 210.2 ms | 7.22 s | 14.74 s |
| bestsource `VideoSource`, `threads=1` | 7.54 s | 211.8 ms | 7.21 s | 14.75 s |

the strongest result of the six containers: 2.62x faster than bestsource on
frames and 4.81x including the open. bestsource reads all 35 files while it
creates the clip, 7.22 s against imgseqs's 0.20 s, and gains nothing from its
threads here (7.52 s against 7.54 s with one), so the win is imgseqs's
lookahead, 3.3x over its own serial row. at `prefetch=16` the gap against
bestsource widens to 3.2x on frames, and a frame costs 11.7 ms.

## png

35 files, 66 MB: 31 monochrome pages of 18.5 MiB (`Gray8`) and four colour ones
(`RGB24`, one of 44.7 MiB and three of 55.4 MiB).

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 0.66 s | 9.4 ms | 0.004 s | 0.66 s |
| imgseqs `Read`, `prefetch=0` | 1.43 s | 29.0 ms | 0.004 s | 1.43 s |
| imgseqs `Read`, `prefetch=16` | 0.89 s | 12.0 ms | 0.004 s | 0.89 s |
| bestsource `VideoSource` | 0.89 s | 15.4 ms | 1.01 s | 1.90 s |
| bestsource `VideoSource`, `threads=1` | 2.23 s | 49.7 ms | 2.30 s | 4.53 s |

the closest of the six. imgseqs is 1.36x faster on frames and 2.88x including
the open, and it is the only set where it wins the serial comparison as well as
the parallel one (1.43 s against 2.23 s), so here the margin is the decoder and
the skipped indexing pass rather than the lookahead: sixteen workers buy nothing
on a set this cheap (0.89 s against 0.66 s).

## jxl

35 files, 174 MB, same manga pages, again 31 `Gray8` and four `RGB24`.

bestsource cannot read this set at all: `bs.VideoSource` fails with
`Video codec not found`, because that ffmpeg build has no jpeg xl decoder. the
bench reports the failure and then measures imgseqs on its own.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 6.42 s | 137.7 ms | 0.003 s | 6.42 s |
| imgseqs `Read`, `prefetch=0` | 19.36 s | 557.8 ms | 0.003 s | 19.37 s |
| imgseqs `Read`, `prefetch=16` | 5.68 s | 25.4 ms | 0.003 s | 5.68 s |

jxl has the most expensive frame of the six (137.7 ms median) and gets the most
from lookahead, 3.02x over its serial row, with a median frame that drops from
557.8 ms to 25.4 ms. `prefetch=16` is its best row, and opening the clip is free
because the probe only reads the header.

## avif

35 files, 130 MB, all colour and therefore all `RGB24` like the webp set, 1928
MiB decoded. bestsource cannot open it.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 9.25 s | 151.2 ms | 6.10 s | 15.35 s |
| imgseqs `Read`, `prefetch=0` | 11.30 s | 327.9 ms | 6.15 s | 17.45 s |
| imgseqs `Read`, `prefetch=16` | 19.11 s | 202.8 ms | 6.09 s | 25.20 s |

two costs stand out. creating the clip takes 6.10 s, which is 174 ms for each
of the 35 files, and the decode reports another ~150 ms of container parsing per
frame on top of the av1 decode itself. the lookahead is worth only 1.22x here,
the least of any set, and `prefetch=16` is 2x slower than the default.

## heic

35 files, 236 MB, and bestsource cannot open the set. imgseqs reads all 35: 31
monochrome pages of 18.5 MiB decoded as `Gray8`, three colour pages of 55.4 MiB
and `p000` at 44.7 MiB as `RGB24`. it is the only set here whose frames mix
formats and sizes.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 7.50 s | 206.3 ms | 0.006 s | 7.51 s |
| imgseqs `Read`, `prefetch=0` | 24.11 s | 709.6 ms | 0.006 s | 24.12 s |
| imgseqs `Read`, `prefetch=16` | 7.92 s | 11.5 ms | 0.006 s | 7.93 s |

three of the frames are ten times the cost of the rest, so the median and the
total disagree: the lookahead is worth 3.21x over the serial row, and the median
frame falls from 709.6 ms to 11.5 ms at `prefetch=16`, which is the cheapest
median here, though the total says sixteen workers are not better than the
default four. the monochrome pages are also the one place where the decode is
not the `image` crate's: they go through `libheif` directly, see
[05](improvements/05-monochrome-heif.md).

## per stage cost

`debug=True` with `prefetch=0`, averaged over the first four files of each
sandbox set, which are three of the colour pages plus the first monochrome one
where the set has any:

| set | decode | copy into the frame | total |
| --- | --- | --- | --- |
| webp 3672x5274 | 520 ms | 86 ms | 607 ms |
| jpeg 3672x5274 | 535 ms | 51 ms | 586 ms |
| png 3672x5274 | 132 ms | 59 ms | 191 ms |
| jxl 3672x5274 | 751 ms | 63 ms | 814 ms |
| avif 3672x5274 | 263 ms | 81 ms | 345 ms |

the decode figure includes the first-touch page faults of the decode buffer,
55.4 MiB for a colour frame and 18.5 MiB for a monochrome one, which is why
allocating that buffer measures as ~0 ms on its own. avif is the one row holding
a second cost: 150 ms of its 263 ms is parsing the container, before any av1
decoding happens. jxl is the slowest of the five, and it is the format whose
frames the lookahead helps most.

## known headroom

the numbers point at three things: the pure rust single threaded webp decoder,
the lookahead pool decoding frames it cannot keep, and the copy into the frame
running on the requesting thread. the per-frame cost of each, and the plans for
changing them, are written up in [improvements](improvements/README.md).

separately from the speed work, the sandbox found one format that used not to
work at all: the 31 monochrome heic files failed to decode, and they now read as
`Gray8` through [05 monochrome heif](improvements/05-monochrome-heif.md).
