# 19 — Measure a shared thread budget for AVIF decoding

Status: measured on 2026-09-29 and decided against. The defaults stay; the
numbers are in [BENCH.md](../BENCH.md#avif-decoder-threads). Priority: closed.

## evidence

Every call to `avif::decode_item` creates `dav1d::Decoder::new()`, including the
separate alpha item. The locked Rust wrapper uses default settings. The native
[dav1d 1.5.3 settings](https://github.com/videolan/dav1d/blob/1.5.3/include/dav1d/dav1d.h)
default their thread count to the host's logical cores. Meanwhile,
`src/prefetch.rs` may run several image decodes concurrently.

Consequently, the outer worker count is not a total decode-thread limit. The
actual thread count and CPU cost need measurement; do not infer either by
multiplying settings, since codec scheduling and per-item lifetimes matter.

## experiment

Compare current defaults against bounded per-decoder settings using the pinned
wrapper's `Settings` API. Include `prefetch=0`, automatic prefetch, four workers,
and a larger user-selected count on small and large AVIFs, with and without
alpha. Test a single clip and simultaneous clips.

Record wall time, CPU time per delivered frame, peak threads, resident memory,
creation/destruction overhead and random-seek latency. Preserve warm/cold-cache
distinctions. Do not reuse a decoder across independent items until state reset
and image independence have been demonstrated.

## what was run

`IMGSEQS_AVIF_THREADS` was wired to `dav1d::Settings::set_n_threads` for the
measurement and removed again; the temporary hook is the only edit this plan
ever made to the plugin. `target/bench/p19/measure.py` runs one configuration
per process, so each number has its own VapourSynth environment and its own copy
of the plugin, and reports wall time, process CPU, CPU per delivered frame and
the peak native thread count. `target/bench/p19/sweep.py` rotates the order of
the configurations over three rounds and reports each one's best;
`target/bench/p19/make-alpha-corpus.py` builds the alpha-bearing corpus and
`target/bench/p19/parity.py` hashes every plane of every frame.

The corpora are the 35 colour pages of `sandbox/avif` (3312x4717 and 3672x5274),
that set cut to one file, two clips of the whole set walked together, twelve
pages of it cropped to 1536x2304 and encoded with and without an alpha item, and
64 repetitions of the 64x48 and 4x4 avif fixtures. `prefetch` was swept over 0,
2, 4, 8 and 16 and the decoder's thread count over `auto` (dav1d's default, ten
here), 1, 2, 3, 4, 6 and 8. The small corpora are built from
`avif-yuv420p.avif` and `alpha-yuv420p.avif` because the smaller
`alpha-rgba8.avif` fixture is three by two and probes as `RGB24`, which hands it
to the fallback `image` decoder rather than to dav1d: a corpus of it would have
measured a decoder this plan does not touch. Resident memory was not measured,
and the session's seek cost is what [21](21-animated-images.md) already recorded.
One decoder per item is unchanged and reuse across items was not attempted: the
plan makes it conditional on state reset and image independence being
demonstrated, and nothing here had a reason to demonstrate either.

## result

The two levels of parallelism are real and they do not fight.

- The default, `prefetch=4`, already runs 8.81 of this machine's ten cores.
- A budget of `cores / workers` gives two decoder threads there, which is
  **1607.6 ms against 1343.6** for the same 35 pages: a 20% regression. A
  3672x5274 page needs three to four threads before dav1d's tile parallelism is
  saturated, so the division starves each decoder to feed the pool a share it
  cannot use.
- Every explicit count from four up is within 2% of `auto`, in both directions,
  at every prefetch depth, on one clip and on two.
- CPU per delivered frame is flat across thread counts at a fixed depth
  (338.2 against 335.7 ms at four, 342.2 against 343.5 at sixteen). The pool
  costs about 21% more CPU than the serial path, but that is the concurrency
  itself, not the decoder's threads.
- Only the thread count falls: 57 to 33 at the default, 189 to 93 at
  `prefetch=16`. Nothing measurable pays for those threads.
- With lookahead off a decode saturates at four threads — 2.89 cores, against
  1.85 at two and 1.01 at one — so the plan's own condition, enough intra-frame
  parallelism when the pool is disabled, is already what the default does.
- A small page never becomes a many-threaded decoder: 64x48 and 4x4 pictures
  show the same thread count whatever the setting, so the nesting only exists
  where the pool is already full.
- Thread count does not move a sample: every plane of every frame hashes
  identically at one, two and `auto`.

## decision

Retain the defaults. The acceptance test was "exact output parity, lower
CPU/thread overhead, and no material regression on large single-frame reads";
parity holds and nothing regresses, but nothing improves either except a thread
count that costs nothing measurable here. The sweep leaves no third option: a
budget low enough to be worth having — a worker's share of the machine — costs
20%, and a budget high enough not to cost anything changes only the thread count.
A change that buys no throughput and no CPU, needs a floor tuned to these files'
tile counts to avoid the regression, and cannot be validated on a machine with
another core count, is not worth the code it would add to `decoder.rs`,
`clip.rs` and `formats/avif.rs`.

What would reopen it: a machine where four workers and their decoders actually
oversubscribe, which means more cores than this one has, a lookahead depth a
caller chose, and a corpus whose pages carry more tiles than these do. Resident
memory is the one number this sweep did not take, so the thread count is the
only part of the nesting left unaccounted for, and that is where a budget would
have to show a cost.

The plan asked for [15](15-avif-decoder-progress.md) to land first so the
benchmark could not hang on a no-picture item. It had, and no run here hung.
