# 49 — a jpeg xl page decoded on more than one thread

- status: implemented. The decoder is handed a runner, and a page decodes 2.7x
  faster on an idle machine and 1.6x with the lookahead pool also decoding.
- touches: `src/formats/jxl.rs` (`Threads`, `threads`, `draw`), `docs/BENCH.md`,
  `CHANGELOG.md`, `AGENTS.md`
- expected: a jpeg xl decode stops being one thread, with no change to a single
  pixel and no new dependency: the crate has always taken a runner, and this
  tree never gave it one.
- risk: low for correctness (the crate's tasks are independent and the runner is
  a scoped thread pool), medium for CPU use: four threads a decode beside up to
  four lookahead workers is sixteen runnable threads on a twelve thread
  machine. The token is the measurement below, which says that is where the
  gain stops on this host anyway.

## problem

`jxl 0.8.0` decodes with whatever `JxlParallelRunner` it is handed, and given
none it answers `SequentialRunner`, whose `num_threads` is one
(`jxl-0.8.0/src/api/decoder.rs`, `struct SequentialRunner`). This tree passed
`None` at all three `process` calls, so every jpeg xl page in every corpus
decoded a tile after another.

The cost was known and unnamed: plan 48's leftover list says "the jxl decode
path is 381 ms a frame serially and has never been profiled". Profiling it is
the stage log: on `sandbox/jxl`'s 35 pages, a frame is 437 ms, of which `read`
is 415 and everything else is under 10. The decode is 95% of the frame and the
frame is one thread.

## evidence

The change itself: the build before it against the build that carries it, the
two alternated over `sandbox/jxl` in three rounds and the best round each, with
`target/bench/plan48/jxl-threads.py` and `jxl-async.py`. Milliseconds a frame:

| set | before | after | change |
| --- | ---: | ---: | ---: |
| one decode at a time, `prefetch=0` | 335.4 | 134.6 | 2.49x |
| the default lookahead, `prefetch=4` | 132.1 | 83.1 | 1.59x |
| the consumer's own requests, window 4 | 134.4 | 82.8 | 1.62x |

then the width, one build with the runner's count forced so the sweep is one
batch and not six:

`target/bench/plan48/measure.py` over `sandbox/jxl`, 35 pages, two reps,
`prefetch=0`, so the stage split is one decode at a time. Milliseconds a frame,
best of the reps; `threads` is the runner's width, one being the crate's own
`SequentialRunner`:

| threads | fetch | read | total | read against one thread |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 421.8 | 401.3 | 421.8 | — |
| 2 | 226.5 | 208.4 | 226.6 | 1.93x |
| 4 | 154.7 | 135.5 | 154.8 | 2.96x |
| 8 | 118.9 | 98.8 | 118.9 | 4.06x |

and the same set as one wall clock, `target/bench/plan48/jxl-threads.py`, which
is the number a user feels. `prefetch=0` is one decode at a time, `prefetch=4`
is the default for this machine and keeps the pool's own workers decoding:

| threads | prefetch=0 | prefetch=4 | prefetch=4 against one thread |
| ---: | ---: | ---: | ---: |
| 1 | 388.8 | 133.1 | — |
| 2 | 227.2 | 98.5 | 1.35x |
| 4 | 148.3 | 83.4 | 1.60x |
| 8 | 118.9 | 82.7 | 1.61x |

A consumer that keeps its own requests in flight is the third shape, and the one
where a per-decode thread budget competes with the consumer's threads:
`target/bench/plan48/jxl-async.py` opens `window` `get_frame_async` requests
with the pool off, so the requester's threads are the ones decoding:

| threads | window 2 | window 4 | window 8 |
| ---: | ---: | ---: | ---: |
| 1 | 220.9 | 140.0 | 97.6 |
| 2 | 138.6 | 98.9 | 87.7 |
| 4 | 100.3 | 89.0 | 86.6 |
| 8 | 92.3 | 85.5 | 84.7 |

measured 2026-11-11 on an i5-11400H (six cores, twelve threads), one run each,
which is this host's caveat throughout: the columns are one run, the shape they
describe repeated. Every pixel of every page is byte identical, before and
after, because the tasks the crate hands out write disjoint tiles.

## the change

`src/formats/jxl.rs` gains a runner: a scoped thread per worker and one atomic
counter, which is what the crate's contract asks for ("different invocations of
`fun(i)` are not allowed to wait on each other"), and `draw` hands it to
`process`. The width is `available_parallelism().min(4)`, and the four is the
measurement rather than a taste: with the pool decoding as well, four and eight
are 1.60x and 1.61x, so eight only buys contention. A machine of one or two
cores gets one or two, and the runner's `num_threads` is a hint the crate uses
to choose how many tiles to split into, so a one thread runner is exactly the
old behaviour.

The header and frame probes keep `None`: they read no pixels, and a decode that
has not started has nothing to split.

## the acceptance

- `cargo test --locked` gains `a_runner_runs_every_task_once`, which runs 37
  tasks through four workers and requires each exactly once.
- `tests/readalpha.vpy` and `tests/pngwrite.vpy` pass, and every frame of the jxl
  fixtures, of `sandbox/jxl` and of `target/bench/jxl-depth` is byte identical
  under the old and new builds (`target/bench/plan48/parity.py`).
- the tables above, re-run: a serial decode is 2.5x, a consumer with the
  lookahead pool decoding is 1.6x, and one with its own requests in flight is
  1.6x.

## what is left

- **the thread count is not the reader's.** The reader has its own budget
  (`prefetch` workers), and the two were chosen separately: four threads a
  decode is a good default, but a reader told `prefetch=8` on a four core
  machine would be better served by a narrower decode. Passing the reader's
  answer down would need the decode to see the reader's configuration, which
  `decoder::decode` does not take today.
- **eight threads an idle machine is 4.06x the read**, so a decode that knows it
  is alone could take the wider width. That is the same "the reader's answer
  decides" question.
- **the other single-threaded decoder is the avif one**, which has the same
  shape: one item, one frame, and `dav1d` decoding it in one call. Its numbers
  are in `docs/BENCH.md`'s avif section and it has not been asked the runner
  question at all.
