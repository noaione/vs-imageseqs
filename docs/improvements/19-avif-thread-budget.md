# 19 — Measure a shared thread budget for AVIF decoding

Status: proposed experiment; nested parallelism is present, but a performance
regression has not been demonstrated. Priority: medium after correctness work.

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

## implementation decision and acceptance

If results support it, pass an internal concurrency budget to the AVIF decoder,
with enough intra-frame parallelism when lookahead is disabled. Keep the public
`prefetch` meaning as the number of background workers. A new user option or a
process-wide scheduler is not justified by the current evidence.

Require exact output parity, lower CPU/thread overhead, and no material
regression on large single-frame reads. Otherwise retain defaults and record
the measurements as a decision against the change. Fix
[15](15-avif-decoder-progress.md) first so the benchmark cannot hang on a
no-picture item.
