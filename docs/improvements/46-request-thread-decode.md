# 46 — who decodes a requested frame

- status: rejected by its own acceptance, after being implemented and measured
- touches: `src/prefetch.rs`
- expected: a frame the pool can produce is produced by the pool, so a graph
  with work downstream does not pay for this filter with its own threads
- outcome: implemented, measured, and reverted. Queuing the request instead of
  claiming it caps the frames in flight at the worker count, which costs 1.57x
  on a bare async consumer at eight frames in flight, and nothing measured
  gained enough to pay for it
- risk: high, as the measurement shows. The thread-ownership half of the change
  is not wrong; the mechanism it was built on is

## problem

`Prefetcher::fetch` has two ways to answer a request. Either the payload is
finished and `ready` hands out a clone, or `claim` marks the index as in flight
and the calling thread decodes it. The second path is the fallback that keeps
the pool from ever becoming a bottleneck: a consumer is never refused, and never
waits on a worker that has not started.

Under a serial consumer the fallback is rare, because the pool was planned for
that access pattern and is always ahead. An async consumer inverts it. Every
request that arrives before the pool reached that index claims, and a consumer
that keeps eight frames in flight is by definition asking for eight indices at
once, so the pool is behind almost all of them.

The consequence is not slowness. It is that the decode runs on a VapourSynth
worker thread: VapourSynth schedules filters on its own pool, sized from the
core count by default, so a filter that decodes on those threads competes with
every other filter in the graph. `PNGWrite` in its `Parallel` mode is the case
that matters here, because it wants several frames in flight itself and then
finds the threads busy decoding.

## evidence

same set and machine as [45](45-async-request-cache.md): `sandbox/webp`, 35
pages of 3672x5274 `YUV420P8`, 27.7 MiB decoded per frame, i5-11400H (6 cores,
12 threads), release build, best of three. `prefetch=0` is the control: with no
workers there is nothing to claim from and nothing to queue for, so it is
untouched by this change and moved by nothing.

| request style | before | after | change |
| --- | ---: | ---: | ---: |
| default (4), serial | 2.683 s | 2.687 s | unchanged |
| default (4), async 4 | 2.637 s | 2.732 s | 3.6% slower |
| default (4), async 8 | 1.724 s | 2.704 s | **1.57x slower** |
| default (4), async 12 | 1.544 s | 1.819 s | 18% slower |
| `prefetch=0`, async 8 | 1.768 s | 1.724 s | unchanged, as designed |
| `prefetch=16`, async 8 | 1.453 s | 1.521 s | 4.7% slower |

`ReadAlpha`, both clips of every frame:

| two clips | before | after | change |
| --- | ---: | ---: | ---: |
| interleaved, serial | 2.761 s | 2.786 s | unchanged |
| interleaved, async 8 | 2.038 s | 2.755 s | 35% slower |
| branches, async 8 | 3.192 s | 3.812 s | 19% slower |
| branches, async 8, `prefetch=16` | 1.594 s | 2.164 s | 36% slower |

## what the change was

1. `State` records `workers`, and `fetch` no longer claims a frame the pool has
   not planned: it offers that index to the front of the queue, so the next
   worker pop takes it, and waits for it on the condvar that already carried the
   wait-for-another-decoder case.
2. `reserve` refuses when the pool has no free worker
   (`in_flight.len() >= workers`), when it has no workers at all, or when the
   index does not fit the byte budget, which is the same rule `plan_window`
   applies to the frames it plans. What it refuses, the caller decodes as
   before.
3. The wait predicate gained "or still queued", and the `WAIT_TIMEOUT` takeover
   removes the index from the queue as well as from `in_flight`, so a worker
   that stalls cannot hang the graph.

## decision — reject

**why it fails.** reserving a frame is only cheap when a worker is genuinely
idle, and `in_flight.len() >= workers` is a poor test of that: a consumer that
opens eight futures does not open them one at a time. All eight requests check
the same counter before any worker has started, all eight see room, all eight
queue, and the concurrency of the read collapses from eight to the four workers.
At depth 4, where the burst cannot overshoot the worker count, the change is
within noise; the regression appears exactly at depth 8 and above, which is the
shape the diagnosis predicts.

**why it is not worth refining.** the honest version of this change counts the
frames a consumer has queued and stops offering once `in_flight + reserved`
reaches the worker count, which would keep concurrency at eight while moving
what it can onto the workers. That is more machinery for a benefit this plan
never measured: the only case it is for — a pipeline that has real work
downstream, `PNGWrite` in its `Parallel` mode over the same pages — does not
exist as a row anywhere, and a bare sink gains nothing by construction because
its wall time is bounded by the machine, not by which thread did the work. The
plan's own `## acceptance` asks for the pipeline row to improve by more than the
bare-sink regression, and a 57% regression is not a bar any unmeasured gain
should be allowed to clear.

**what is kept instead.** the observation that stands is a documentation one:
under an async consumer the decode runs on VapourSynth's threads, and `prefetch`
is therefore a serial-consumer argument. That belongs in `prefetch`'s own
documentation and in [BENCH.md](../BENCH.md), not in a scheduler change, and it
is recorded in [47](47-claim-budget-accounting.md), which is the plan that takes
the accounting half of this problem without touching who decodes.

## acceptance — not met

- bare-sink rows within 3% of the previous build at every depth. **failed at
  depth 8 and above**: 1.57x slower at eight frames in flight, 18% at twelve.
- a pipeline row that does not exist yet. **not built**, and a 57% bare-sink
  loss is not recoverable by a case nobody has measured.
- no deadlock. **met**: the timeout takeover dropped the stale queue entry and
  every test passed, including `retries_failed_decodes`, so the mechanism was
  sound and only the policy was wrong.

The code was reverted to the state [45](45-async-request-cache.md) left it in.
`cargo test --locked` is 370 tests and the release build's async rows are back
to 1.728 s at eight frames in flight against 1.724 s before the experiment.
