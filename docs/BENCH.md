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
  disk) and both plugins are given `apply_rotation=False`, so neither of them
  moves a pixel and the comparison is the read path alone. Without it both
  plugins rotate, and they agree about the file — the sandbox sets carry no exif
  either way, so these numbers are unaffected. bestsource's `cachesize`,
  `threads` and `maxdecoders` stay at the defaults, and `threads=1` is the row
  that matches `prefetch=0`.
- bestsource is never given `fpsnum`/`fpsden` for an image sequence: ffmpeg
  reads image sequences at 25 fps, so any other rate resamples and silently
  drops every 25th image.
- the two plugins produce the same planes for webp and not for anything else.
  imgseqs returns the image's own format when the file has one (`GRAY8` for
  monochrome jpeg, png, jxl, heif and avif pages, `RGB24` for colour ones, and
  `YUV420P8` for a lossy webp without an alpha channel), while bestsource keeps whatever
  format its decoder emits: `YUV420P8` for webp, `YUV444P8` for jpeg, a
  variable format for a mixed folder. on the webp set the two decoders agree
  byte for byte; everywhere else this compares read throughput, not identical
  output.
- `open` is the time to create the clip. bestsource indexes the sequence while
  the clip is created, which for images means reading every file once.

## sandbox corpus

`sandbox/` holds the same 35 pages in six containers, so the formats can be
compared like for like. the pages are not uniform: `p000` is 3312x4717 and the
other 34 are 3672x5274, and only three of them (`p000`, `p001`, `p002`) hold
colour. `p018` is grey content that every container still stores in a three
channel format, so it decodes as `RGB24` or `YUV420P8` where the other grey
pages are one channel. what imgseqs makes of them depends on the container:

| set | what imgseqs returns | decoded |
| --- | --- | --- |
| webp | 35 `YUV420P8` frames | 964 MiB, one 22.3 MiB frame and 34 of 27.7 MiB |
| avif | 34 `RGB24` frames and one `Gray8` | 1892 MiB: one 44.7 MiB frame, 33 of 55.4 MiB and one of 18.5 MiB |
| jpeg, png, jxl, heic | 31 `Gray8` frames of 18.5 MiB and 4 `RGB24` | 784 MiB |

every set needs `mismatch=True` for this. those frame sizes (18.5 to 55.4 MiB)
are what the lookahead budget is up against: 192 MiB holds ten of the gray
frames and seven of the webp ones but only three and a half of the 55 MiB rgb
ones, so the budget is `max(192 MiB, window x largest frame)` and a deep
lookahead on these pages is allowed to use more memory than that. see
[01](improvements/01-lookahead-scheduling.md).

```console
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --extra --prefetch 16 --dir sandbox/webp --pattern "snek - p%03d.webp"
```

frames, best pass of three:

| set | imgseqs | bestsource | verdict |
| --- | --- | --- | --- |
| webp | 3.39 s | 1.88 s | bestsource 1.80x faster on frames, imgseqs 1.14x including the open |
| jpeg | 2.75 s | 7.49 s | imgseqs 2.73x faster, 4.98x including the open |
| png | 0.62 s | 0.86 s | imgseqs 1.37x faster, 2.96x including the open |
| jxl | 6.50 s | cannot read | 4.13x over imgseqs's own serial row |
| avif | 5.14 s | cannot open | 2.36x over imgseqs's own serial row |
| heic | 8.24 s | cannot open | 3.06x over imgseqs's own serial row |

`prefetch=16` is the best row on five of the six sets — webp, jpeg, png, jxl and
avif — and heic is the exception, with its default 30% ahead of it. the deeper
pattern is in the single-worker rows: the lookahead is worth 2.4x to 6.4x over
the serial row on every set, which is what
[01](improvements/01-lookahead-scheduling.md) made reliable, and the webp figure
nearly doubled when its frames halved in size
([03](improvements/03-webp-yuv-output.md)). the sections below have the detail.

## webp

35 files, 146 MB, three colour pages (`p000` to `p002`) and 32 grey ones that
the lossy encoder still stores as `YUV420P8`: `p000` is 3312x4717, 22.3 MiB
decoded, and the other 34 are 3672x5274 at 27.7 MiB.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 3.39 s | 81.5 ms | 0.003 s | 3.39 s |
| imgseqs `Read`, `prefetch=0` | 11.94 s | 370.8 ms | 0.004 s | 11.95 s |
| imgseqs `Read`, `prefetch=16` | 1.87 s | 10.6 ms | 0.004 s | 1.87 s |
| bestsource `VideoSource` | 1.88 s | 19.6 ms | 2.00 s | 3.88 s |
| bestsource `VideoSource`, `threads=1` | 11.70 s | 353.1 ms | 11.58 s | 23.28 s |

the closest set of the six, and the first one where imgseqs reads the same
pixels as bestsource: both decode the file with libwebp (ffmpeg's vp8 decoder
uses it) and hand out `YUV420P8` planes that are byte-identical, tagged
`_Matrix=5`/`_Range=0`, so this compares two decoders doing the same job.
bestsource is 1.80x faster on frames, which is its slice threading — ffmpeg
spreads one vp8 frame over every core, so `threads=1` to the default turns
353 ms per frame into 20 ms — and imgseqs is 1.14x faster including the open,
3.39 s against 3.88 s, because bestsource reads all 35 files while it creates
the clip and imgseqs probes only headers. imgseqs turns its own lookahead into
6.4x over its serial row, and `prefetch=16` delivers a frame every 10.6 ms.

webp used to be read as `RGB24`, and this section used to report 5.66 s, 19.97 s
and 4.12 s. the two changes behind the current numbers are
[04](improvements/04-webp-decoder.md) (`image-webp` → libwebp) and
[03](improvements/03-webp-yuv-output.md) (yuv planes instead of rgb):

| build | frames | `prefetch=0` | `prefetch=16` |
| --- | --- | --- | --- |
| `image-webp`, `RGB24` | 5.66 s | 19.97 s | 4.12 s |
| libwebp, `RGB24` | 4.24 s | 15.60 s | 2.62 s |
| libwebp, `YUV420P8` | 3.39 s | 11.94 s | 1.87 s |

lookahead depth, first 16 files, wall clock and process CPU per delivered frame
(one serial frame is 313 ms of CPU; the row this table had before the two
changes is in [03](improvements/03-webp-yuv-output.md)):

| prefetch | wall | cpu | cores busy | cpu over serial |
| --- | --- | --- | --- | --- |
| 0 | 316.2 ms | 312.5 ms | 0.99 | 1.00x |
| 2 | 160.7 ms | 331.1 ms | 2.06 | 1.06x |
| 4 | 91.2 ms | 351.6 ms | 3.85 | 1.13x |
| 6 | 71.9 ms | 382.8 ms | 5.32 | 1.23x |
| 8 | 65.7 ms | 343.8 ms | 5.23 | 1.10x |
| 12 | 56.6 ms | 416.0 ms | 7.35 | 1.33x |
| 16 | 54.2 ms | 419.9 ms | 7.75 | 1.34x |

the wasted work is what [01](improvements/01-lookahead-scheduling.md) fixed: cpu
per delivered frame was 2.58x one serial decode at `prefetch=16`, is 1.43x
before these two changes, and is 1.34x now, while the wall time improves all the
way to 16 instead of turning around after 8 — the frames the budget has to hold
are half the size, 27.7 MiB instead of 55.4 MiB, so the same depth keeps twice
as many of them: `prefetch=16` is allowed the larger of 192 MiB and
16 x 27.7 MiB, which is 443 MiB.

### what a graph that needs rgb pays

the 34 uniform files of this set, `prefetch=16`, read and then converted back:

| pipeline | frames | per frame |
| --- | --- | --- |
| imgseqs `Read` | 1.89 s | 55.6 ms |
| imgseqs `Read` + `Bicubic` to `RGB24` | 2.23 s | 65.5 ms |
| imgseqs `Read` from the pre-[03](improvements/03-webp-yuv-output.md) build | 4.24 s | 121.5 ms |

the conversion is +18% on the read, and it runs on the requesting thread, so
the lookahead cannot hide it: the read is at 56 ms per frame while the pool
decodes, and the copy into the frame plus the resize is what the caller waits
for. even so, converting in the graph is 1.9x faster than having the decoder
produce rgb, because the pool then carries twice the bytes per frame.

a page whose width or height is odd cannot be converted at all: `zimg` refuses
4:2:0 to rgb with `Resize error 1027` whatever the matrix arguments are, so the
odd row and column are cropped away, the rest is converted, and the size is
restored with an `RGB24` border. on a 2903x4128 page that costs the same as the
conversion itself, 125 ms to 153 ms per frame at `prefetch=0` and 26 ms to 49 ms
at `prefetch=8`. `std.Crop` and `std.AddBorders` need a constant clip
(`Crop: constant format and dimensions needed`, `AddBorders: input needs to be
constant format`), and `mismatch` makes the clip variable, so the crop cannot
run on a clip that mixes formats or sizes: `target/bench/mixed-conversion.py`
benches the two shapes that can, on both sandbox sets, best pass of two.

`sandbox/mixed`, 35 files (19 `Gray8`, 11 `RGB24`, 5 `YUV420P8`, one 3312x4717
and 34 of 3672x5274), `--corpus mixed --ogsov`:

| pipeline | total | per frame | added |
| --- | --- | --- | --- |
| `Read` | 4.36 s | 124.4 ms | |
| `Read` + `Bicubic` to `RGB24` | 4.46 s | 127.3 ms | +2.9 ms |
| `Read` + `FrameEval` chains to `RGB24` | 4.90 s | 139.9 ms | +15.5 ms |
| `Read` + `Bicubic` to `RGB24` + `GPUUpload` + `ogsov.AnalyzeVk` | 4.74 s | 135.5 ms | +11.1 ms |
| the same through `ogsov.Analyze` on the cpu | 6.07 s | 173.3 ms | +48.9 ms |
| `Read` at `prefetch=0` | 12.70 s | 363.0 ms | +238.5 ms |
| `Read` + `Bicubic` to `RGB24` at `prefetch=0` | 13.14 s | 375.4 ms | +251.0 ms |

five yuv frames among thirty-five cost 2.9 ms per frame at the default
lookahead, and 12.5 ms per frame at `prefetch=0`, which is 88 ms for each of the
five converted pages. the `FrameEval` row is the shape that also survives an odd
page, for 12 ms per frame more.

`sandbox/webp`, the same 35 pages all in webp, one of them 3312x4717 with an odd
height, `--corpus webp --ogsov`:

| pipeline | total | per frame | added |
| --- | --- | --- | --- |
| `Read` | 3.53 s | 101.0 ms | |
| `Read` + `Bicubic` to `RGB24` | `Resize error 1027` | | |
| `Read` + `FrameEval` chains to `RGB24` | 3.72 s | 106.4 ms | +5.4 ms |
| `Read` + chains + declaring `resize` + `GPUUpload` + `ogsov.AnalyzeVk` | 3.75 s | 107.2 ms | +6.2 ms |
| `Read` at `prefetch=0` | 12.30 s | 351.4 ms | +250.4 ms |

`FrameEval` builds one small graph per frame and crops only the odd `4:2:0`
frames, so the 34 uniform pages pay for the odd one: 5.4 ms per frame. it cannot
promise one format for its result and `std.GPUUpload` needs a constant one, so
the ogsov pipeline adds a declaring `resize` on top, which inside the pool is
free (3.72 s to 3.75 s).

ogsov agrees between the routes: `OGSOVIsColor` matches for all 35 webp frames
against a pre-[03](improvements/03-webp-yuv-output.md) build's own `RGB24`, and
the plane means differ by at most 0.07 of a level, 0.006 on average, worst frame
the odd page and its restored border. `GPUUpload` refuses the `mismatch` clip
itself (`ogsov.AnalyzeVk: clip must be a video node`), so the conversion is what
makes an ogsov graph possible on a mixed sequence at all.

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

jxl needs the most cpu per frame of the six (485 ms in the stage table below)
and gets the most from lookahead, 4.13x over its serial row, with a median frame
that drops from 544.1 ms to 10.6 ms. `prefetch=16` is its best row, and opening
the clip is free because the probe only reads the header.

## avif

35 files, 130 MB, 34 colour pages and one monochrome page. avif is the only set
whose colour pages are all coded with three channels: the grey pages beside
`p003` are stored that way too, so 34 files come back as `RGB24` of 55.4 MiB
(and `p000` as `RGB24` of 44.7 MiB) and `p003`, the one file whose bitstream says
`mono_chrome`, comes back as `Gray8` of 18.5 MiB. 1892 MiB decoded, against
1928 MiB before [05](improvements/05-monochrome-heif.md) corrected that page.
bestsource cannot open the set.

| reading the set | frames | median frame | open | total |
| --- | --- | --- | --- | --- |
| imgseqs `Read` | 4.99 s | 121.4 ms | 0.002 s | 4.99 s |
| imgseqs `Read`, `prefetch=0` | 11.77 s | 336.4 ms | 0.003 s | 11.77 s |
| imgseqs `Read`, `prefetch=16` | 5.14 s | 0.4 ms | 0.003 s | 5.14 s |

this is the set where the budget mattered most. the decode reports ~165 ms per
frame: 98 ms building the decoder, which has to decode the picture before it can
report a size, and 67 ms converting it to r,g,b and copying it into the frame.
the frames are `RGB24` of 55.4 MiB, so the old fixed 192 MiB
budget held three and a half of them and the pool re-decoded whatever it had to
drop: the default was 9.25 s and `prefetch=16` was 19.11 s, twice as slow as
doing nothing in parallel. with the budget following the window the same three
rows are 4.99 s, 11.77 s and 5.14 s, so the default is now 2.36x faster than the
serial row and asking for sixteen workers costs nothing.

creating the clip took 6.43 s before [05](improvements/05-monochrome-heif.md)
taught the probe to read the container instead of building the decoder, which
decodes the picture and the alpha item before it can report a size. it is 2 ms
now: 183 ms per file became 0.06 ms, so the set went from 6.43 s plus 5.49 s of
frames to 0.002 s plus 4.99 s, 11.92 s of work against 4.99 s. a file used to be
decoded twice, once to probe it and once for its frame, and the frame column moved
the same way in the row with no lookahead, 12.58 s against 11.77 s. `prefetch=16`
is the exception at 5.14 s against 5.06 s, which is inside the run to run spread
of this machine: with sixteen workers the second decode was hidden behind the
first rather than removed, so a cheaper probe has little left to give it.

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
| webp 3672x5274 | 204 ms | 14 ms | 220 ms |
| jpeg 3672x5274 | 341 ms | 44 ms | 389 ms |
| png 3672x5274 | 90 ms | 43 ms | 137 ms |
| jxl 3672x5274 | 485 ms | 47 ms | 540 ms |
| avif 3672x5274 | 165 ms | 73 ms | 244 ms |

the decode figure includes the first-touch page faults of the decode buffer,
27.7 MiB for a colour webp frame, 55.4 MiB for an rgb one and 18.5 MiB for a
monochrome one, which is why allocating that buffer measures as ~0 ms on its
own. with `prefetch=0` every column is the requesting thread's own work, and the
`fetch` field the log also prints is the sum of them. with workers the decode and
the write are measured in the worker that did them, and `fetch` is only the wait,
which is what [02](improvements/02-frame-write-path.md) changed. the webp row is
what [03](improvements/03-webp-yuv-output.md) and
[04](improvements/04-webp-decoder.md) left behind: it read 520 ms of decode and
86 ms of write with `image-webp` and an rgb frame, and its planes are now copied
into the frame row by row instead of converted from an interleaved buffer, which
is why its write is the smallest of the five. avif is the one row holding a
second cost: 98 ms of its 165 ms is building the decoder, which decodes the
picture and its alpha item before it can report a size — parsing the container is
half a millisecond of that — and the copy then converts the yuv it holds to the
r,g,b the frame takes. jxl is the slowest of the five, and it is the format whose
frames the lookahead helps most.

## frame write path

[02](improvements/02-frame-write-path.md) moved the frame build out of the
requesting thread and into the lookahead worker that decoded the file. the copy
is memory bound, so this only pays where the pool was already faster than the
thread asking for frames: the corpus below is that case, the small jpeg set is
the counter example.

132 manga pages of 1404x2000 (`I:/Manga/Yuri Love Story/source/v02`), wall clock
for the whole set, `--reps 1`, two rounds, bestsource measured in the same four
passes:

| round | default before | default after | `prefetch=16` before | after | bestsource |
| --- | --- | --- | --- | --- | --- |
| 1 | 1.177 s | 1.172 s | 1.030 s | 0.870 s | 1.938 s, 2.104 s |
| 2 | 1.234 s | 1.016 s | 1.522 s | 0.757 s | 2.070 s, 1.967 s |

32 sandbox png pages of 3672x5274 (`Gray8`, 18.5 MiB each), two rounds, `total`
in ms/frame from `target/bench/stage-split.py`, which takes a prefetch argument:

| prefetch | before | after |
| --- | --- | --- |
| 4 | 21.32, 19.76 | 18.09, 18.80 |
| 16 | 16.97, 16.12 | 12.43, 16.15 |

and the case where it is worth nothing, 8 of the same jpeg pages at
`prefetch=4`:

| stage | before | after |
| --- | --- | --- |
| wait for the pool (`decode`, then `fetch`) | 1.93 ms | 8.87 ms |
| copy on the requesting thread (`convert`) | 6.94 ms | gone |
| total | 8.96 ms | 8.89 ms |

the worker side of that last row moved the other way: `read` 15.13 → 18.11 ms and
`convert` 6.94 → 15.59 ms per frame, because four workers writing planes at once
compete for the memory bandwidth the decoder wants. the write does not scale with
the worker count, so moving it into the pool moves the floor instead of removing
it. that is why the same change is worth 5% to 35% on a pool that had room and
nothing on a pool that did not, and why `prefetch=0` is unchanged: it is the same
work on the same thread.

the parity check for it is `target/bench/frame-parity.py`, which hashes every
plane of every frame of seven sets (six from `sandbox/` plus `tests/fixtures`),
for `Read` and for both clips of `ReadAlpha`. the 75 line dump is identical
before and after except for the header that names the plugin.

## known headroom

the numbers point at one thing: a decoder that cannot use more than one core.
libwebp is single threaded per image — ffmpeg is ahead on webp only because it
slice-threads one vp8 frame, 353 ms to 20 ms per frame from one thread to
sixteen — and `image-webp` was behind it per thread as well
([04](improvements/04-webp-decoder.md)).

the copy into the frame used to be the second one. it now runs on the lookahead
worker instead of on the requesting thread
([02](improvements/02-frame-write-path.md)), which moves where it is paid rather
than removing it: the write is memory bound and does not scale with workers, so
the next step for a format that is expensive to write is a decoder that writes
the planes itself rather than one more worker (libwebp already can, which is what
[04](improvements/04-webp-decoder.md) used).

a second one used to be here: the lookahead pool decoded frames it could not
keep, which cost both cpu and wall time on the largest sets. that is fixed in
[01](improvements/01-lookahead-scheduling.md), and the sandbox avif set shows
what it was worth — 9.25 s to 4.74 s at the default, and `prefetch=16` from
19.11 s to 5.41 s.

separately from the speed work, the sandbox found one format that used not to
work at all: the 31 monochrome heic files failed to decode, and they now read as
`Gray8` through [05 monochrome heif](improvements/05-monochrome-heif.md).
