# 46 — who decodes a requested frame

- status: proposed
- touches: `src/prefetch.rs`
- expected: a frame the pool can produce is produced by the pool, so a graph
  with work downstream does not pay for this filter with its own threads
- risk: medium — it changes the `prefetch=0` path if done carelessly, and it
  adds a wake-up per frame that a bare sink does not need

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

same set and machine as [45](45-async-request-cache.md). `prefetch=0` is the
control: with no workers there is nothing to claim from, so every frame is
decoded by whoever asked for it.

| request style | wall | ms/frame | cpu/frame |
| --- | ---: | ---: | ---: |
| default (4), serial | 2.669 s | 76.3 | 297 |
| default (4), async 8 | 1.528 s | 43.6 | 297 |
| `prefetch=0`, async 8 | 1.525 s | 43.6 | 301 |
| `prefetch=16`, serial | 1.354 s | 38.7 | 363 |
| `prefetch=16`, async 8 | 1.253 s | 35.8 | 342 |

four workers against none, at the same eight frames in flight, is 1.528 s
against 1.525 s. the pool contributes 0.2%, which is to say the four background
workers are not what delivers the 1.75x that async buys over the default serial
read. the async consumer's own threads are.

a second run agreed on the pairing rather than on the absolute numbers (1.740 s
against 1.711 s), and the spread between runs is up to 20% at fixed settings, so
only the same-run comparison is trustworthy.

so `prefetch` is a serial-consumer argument, and an async consumer that sets it
is setting something that is not on its path. that is a problem of expectation
as much as of speed: the argument's documentation says it selects background
decode workers, and under async it does not select what does the decoding.

## change

1. make the wait policy a decision rather than a fallback. in `fetch`, when the
   payload is absent:
   - `workers == 0` keeps claiming. there is nothing else that can decode, and
     queueing into an empty queue would serialise the clip.
   - otherwise queue exactly this index at the front of the queue and wait on
     the same condvar the "another thread is already decoding this" branch
     already waits on. the existing `WAIT_TIMEOUT` stays as the escape hatch and
     falls back to a claim, so a worker that stalls cannot hang the graph.
2. the new "plan this index" is `plan_window`'s first candidate without the rest
   of the window, so it goes through the same byte accounting and the same
   generation check. an index the consumer asks for is not allowed to bypass the
   budget that a planned one respects.
3. `prefetch`'s documentation stops claiming what it does not do under async: it
   selects background decoders, and a consumer that keeps frames in flight gets
   that concurrency from the graph whether the argument is set or not.

## decision

**why this is not obviously a win, and is measured first.** claiming is the
right answer for a bare sink and the wrong one for a pipeline, and the two
cannot be told apart from inside the filter. so the change is worth landing only
if the pipeline case gains more than the bare-sink case loses.

**why the loss should be small.** queueing costs one condvar wake-up and one
mutex hand-off, tens of microseconds, against a decode of 250 ms on this set and
1.3 s on the jpeg 2000 set. it cannot be a wall-time cost; it can only be a
latency cost, and only for a frame that arrives before any worker is ready.

**why it might still be rejected.** if the pipeline measurement shows the gain
is inside run-to-run noise, then the honest outcome is to leave `fetch` alone
and document the thread ownership instead, which is the cheaper half of the
change and the half that fixes the wrong expectation.

## acceptance

- bare-sink rows in `read-async.py` within 3% of today at every depth, with the
  `prefetch=0` row identical to the millisecond, because that path is untouched.
- a pipeline row that does not exist yet: `PNGWrite` in `Parallel` mode over the
  same 35 pages, in the shape `target/bench/png-write-async.py` already measures
  the writer in on its own. the whole-pipeline wall time must improve, and the
  improvement must be larger than the bare-sink regression.
- no deadlock: `WAIT_TIMEOUT` must still fall back to a claim, and the
  `retries_failed_decodes` test in `src/prefetch.rs` must keep passing, since it
  exercises the path where a worker produces an error rather than a payload.
