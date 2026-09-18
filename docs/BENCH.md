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

every set needs `mismatch=True` for this. those frame sizes (18.5 to 55.4 MiB)
are what the lookahead budget is up against: 192 MiB holds ten of the gray
frames but only three and a half of the colour ones, so the budget is
`max(192 MiB, window x largest frame)` and a deep lookahead on these pages is
allowed to use more memory than that. see
[01](improvements/01-lookahead-scheduling.md).

```console
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --extra --prefetch 16 --dir sandbox/webp --pattern "snek - p%03d.webp"
```

frames, best pass of three:

| set | imgseqs | bestsource | verdict |
| --- | --- | --- | --- |
| webp | 5.66 s | 2.15 s | bestsource 2.65x faster, 1.29x including the open |
| jpeg | 2.75 s | 7.49 s | imgseqs 2.73x faster, 4.98x including the open |
| png | 0.62 s | 0.86 s | imgseqs 1.37x faster, 2.96x including the open |
| jxl | 6.50 s | cannot read | 4.13x over imgseqs's own serial row |
| avif | 4.74 s | cannot open | 2.52x over imgseqs's own serial row |
| heic | 8.24 s | cannot open | 3.06x over imgseqs's own serial row |

`prefetch=16` is the best row on five of the six sets — webp, jpeg, png, jxl and
heic — and avif is the exception, with its default 14% ahead of it. the deeper
pattern is in the single-worker rows: the lookahead is worth 2.4x to 4.8x over
the serial row on every set, which is what
[01](improvements/01-lookahead-scheduling.md) made reliable. the sections below
have the detail.

## webp

35 files, 146 MB, all colour: `p000` is 3312x4717, 44.7 MiB decoded, and the
other 34 are 3672x5274 at 55.4 MiB.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 5.66 s | 134.2 ms | 0.004 s | 5.67 s |
| imgseqs `Read`, `prefetch=0` | 19.97 s | 600.7 ms | 0.003 s | 19.97 s |
| imgseqs `Read`, `prefetch=16` | 4.12 s | 70.2 ms | 0.004 s | 4.12 s |
| bestsource `VideoSource` | 2.15 s | 23.5 ms | 2.27 s | 4.44 s |
| bestsource `VideoSource`, `threads=1` | 11.91 s | 361.4 ms | 11.73 s | 23.59 s |

bestsource is 2.65x faster on frames and 1.29x including the open, and its
decoder is the whole reason: ffmpeg spreads one vp8 frame over every core, so
`threads=1` to `threads=0` turns 361 ms per frame into 24 ms, while imgseqs
decodes a frame in a single pure rust thread. imgseqs turns its own lookahead
into 4.8x over its serial row, which is still not enough to close that gap. this
is the set [04](improvements/04-webp-decoder.md) is for.

lookahead depth, first 16 files, wall clock and process CPU per delivered frame
(one serial frame is 563 ms of CPU):

| prefetch | wall | cpu | cores busy | cpu over serial |
| --- | --- | --- | --- | --- |
| 0 | 571.3 ms | 562.5 ms | 0.98 | 1.00x |
| 2 | 263.0 ms | 576.2 ms | 2.19 | 1.02x |
| 4 | 160.7 ms | 648.4 ms | 4.04 | 1.15x |
| 6 | 132.9 ms | 731.4 ms | 5.50 | 1.30x |
| 8 | 124.0 ms | 758.8 ms | 6.12 | 1.35x |
| 12 | 126.3 ms | 801.8 ms | 6.35 | 1.43x |
| 16 | 145.6 ms | 806.6 ms | 5.54 | 1.43x |

the wasted work no longer grows the way it used to, which is what
[01](improvements/01-lookahead-scheduling.md) fixed: cpu per delivered frame was
2.58x one serial decode at `prefetch=16` and is now 1.43x, and the wall time
improves all the way to 8 instead of turning around after 4. 12 and 16 are the
same work on more threads — 16 workers on a 12 thread machine is contention, and
past 12 the wall time rises again. the budget that makes the deep rows possible
is the automatic one: `prefetch=16` on 55.4 MiB frames is allowed 886 MiB.

## jpeg

35 files, 213 MB. 31 of the pages are monochrome and come out as `Gray8` of
18.5 MiB; the four colour ones are `RGB24` (`p000` at 44.7 MiB, `p001`, `p002`
and `p018` at 55.4 MiB).

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 2.75 s | 58.2 ms | 0.20 s | 2.95 s |
| imgseqs `Read`, `prefetch=0` | 9.32 s | 255.4 ms | 0.20 s | 9.52 s |
| imgseqs `Read`, `prefetch=16` | 2.04 s | 11.8 ms | 0.20 s | 2.24 s |
| bestsource `VideoSource` | 7.49 s | 210.1 ms | 7.18 s | 14.67 s |
| bestsource `VideoSource`, `threads=1` | 7.44 s | 208.3 ms | 7.15 s | 14.59 s |

the strongest result of the six containers: 2.73x faster than bestsource on
frames and 4.98x including the open. bestsource reads all 35 files while it
creates the clip, 7.18 s against imgseqs's 0.20 s, and gains nothing from its
threads here (7.49 s against 7.44 s with one), so the win is imgseqs's
lookahead, 4.6x over its own serial row. at `prefetch=16` the gap against
bestsource widens to 3.7x on frames, and a frame costs 11.8 ms.

## png

35 files, 66 MB: 31 monochrome pages of 18.5 MiB (`Gray8`) and four colour ones
(`RGB24`, one of 44.7 MiB and three of 55.4 MiB).

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 0.62 s | 9.1 ms | 0.003 s | 0.63 s |
| imgseqs `Read`, `prefetch=0` | 1.43 s | 29.0 ms | 0.003 s | 1.43 s |
| imgseqs `Read`, `prefetch=16` | 0.60 s | 7.7 ms | 0.003 s | 0.61 s |
| bestsource `VideoSource` | 0.86 s | 14.3 ms | 1.00 s | 1.85 s |
| bestsource `VideoSource`, `threads=1` | 2.21 s | 48.4 ms | 2.30 s | 4.50 s |

the closest of the six. imgseqs is 1.37x faster on frames and 2.96x including
the open, and it is the only set where it wins the serial comparison as well as
the parallel one (1.43 s against 2.21 s), so here the margin is the decoder and
the skipped indexing pass rather than the lookahead: these frames fit the
lookahead budget either way, so sixteen workers only edge out the default
(0.60 s against 0.62 s).

## jxl

35 files, 174 MB, same manga pages, again 31 `Gray8` and four `RGB24`.

bestsource cannot read this set at all: `bs.VideoSource` fails with
`Video codec not found`, because that ffmpeg build has no jpeg xl decoder. the
bench reports the failure and then measures imgseqs on its own.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 6.50 s | 117.9 ms | 0.004 s | 6.51 s |
| imgseqs `Read`, `prefetch=0` | 18.86 s | 544.1 ms | 0.003 s | 18.86 s |
| imgseqs `Read`, `prefetch=16` | 4.57 s | 10.6 ms | 0.005 s | 4.58 s |

jxl needs the most cpu per frame of the six (751 ms in the stage table below)
and gets the most from lookahead, 4.13x over its serial row, with a median frame
that drops from 544.1 ms to 10.6 ms. `prefetch=16` is its best row, and opening
the clip is free because the probe only reads the header.

## avif

35 files, 130 MB, all colour and therefore all `RGB24` like the webp set, 1928
MiB decoded. bestsource cannot open it.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 4.74 s | 111.5 ms | 6.27 s | 11.01 s |
| imgseqs `Read`, `prefetch=0` | 11.97 s | 348.1 ms | 6.22 s | 18.19 s |
| imgseqs `Read`, `prefetch=16` | 5.41 s | 109.2 ms | 6.34 s | 11.75 s |

this is the set where the budget mattered most. creating the clip takes 6.27 s,
which is 179 ms for each of the 35 files, and the decode reports another ~150 ms
of container parsing per frame on top of the av1 decode itself. the frames are
`RGB24` of 55.4 MiB, so the old fixed 192 MiB budget held three and a half of
them and the pool re-decoded whatever it had to drop: the default was 9.25 s and
`prefetch=16` was 19.11 s, twice as slow as doing nothing in parallel. with the
budget following the window the same three rows are 4.74 s, 11.97 s and 5.41 s,
so the default is now 2.52x faster than the serial row and asking for sixteen
workers no longer costs anything.

## heic

35 files, 236 MB, and bestsource cannot open the set. imgseqs reads all 35: 31
monochrome pages of 18.5 MiB decoded as `Gray8`, three colour pages of 55.4 MiB
and `p000` at 44.7 MiB as `RGB24`. it is the only set here whose frames mix
formats and sizes.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 8.24 s | 203.4 ms | 0.005 s | 8.24 s |
| imgseqs `Read`, `prefetch=0` | 25.17 s | 749.7 ms | 0.006 s | 25.18 s |
| imgseqs `Read`, `prefetch=16` | 5.63 s | 11.4 ms | 0.006 s | 5.64 s |

three of the frames are ten times the cost of the rest, so the median and the
total disagree: the lookahead is worth 4.47x over the serial row, and the median
frame falls from 749.7 ms to 11.4 ms at `prefetch=16`, which is the cheapest
median here. `prefetch=16` is 1.5x faster than the default on this set, which is
what the budget change was for: the 31 gray frames fit 192 MiB but the four
colour ones do not, so the old default spent its time re-decoding. the default
row is the one number on this page that looks worse than before (8.24 s against
7.50 s) and the `prefetch=0` row moved the same way (25.17 s against 24.11 s),
which is machine drift over that pair of runs rather than the change: the serial
path is untouched. the monochrome pages are also the one place where the decode
is not the `image` crate's: they go through `libheif` directly, see
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

the numbers point at two things: the pure rust single threaded webp decoder, and
the copy into the frame running on the requesting thread. the per-frame cost of
each, and the plans for changing them, are written up in
[improvements](improvements/README.md).

a third one used to be here: the lookahead pool decoded frames it could not keep,
which cost both cpu and wall time on the largest sets. that is fixed in
[01](improvements/01-lookahead-scheduling.md), and the sandbox avif set shows
what it was worth — 9.25 s to 4.74 s at the default, and `prefetch=16` from
19.11 s to 5.41 s.

separately from the speed work, the sandbox found one format that used not to
work at all: the 31 monochrome heic files failed to decode, and they now read as
`Gray8` through [05 monochrome heif](improvements/05-monochrome-heif.md).
