# 47 — what the byte budget counts

- status: proposed
- touches: `src/prefetch.rs`, `docs/BENCH.md`
- expected: `prefetch_memory` bounds what the pool holds, and a deep async
  consumer stops starving the lookahead it is not using
- risk: low, accounting only

## problem

`committed_bytes` sums three things: the payloads in `ready`, the ones still in
`queue`, and the ones in `in_flight`. But `in_flight` holds two different kinds
of work. One is a worker decoding a frame `plan_window` queued. The other is a
consumer thread that called `claim` for an index the pool never planned.

`claim` does no budget check, and that is right: a frame request must not be
refused or delayed because a cache is full. The consequence is that a claimed
frame counts against the pool's budget anyway, so `plan_window` sees a budget
that is already spent and queues nothing. Under a consumer that keeps more
frames in flight than the budget can hold, the lookahead silently stops, which
is the opposite of what a long pipeline wants.

The second half is a documentation problem. `prefetch_memory` reads as a memory
ceiling, and it is not one: it bounds the pool, and a frame in flight in the
graph is the graph's.

## evidence

same set and machine as [45](45-async-request-cache.md). peak working set of the
whole process, one configuration per process, measured with
`target/bench/read-async.py --memory`. the `prefetch=0` serial row is the
baseline: python, the plugin, and one frame.

| request style | peak working set | above the baseline |
| --- | ---: | ---: |
| `prefetch=0`, serial | 697 MiB | — |
| default (4), serial | 911 MiB | 214 MiB |
| default (4), async 8 | 1043 MiB | 346 MiB |
| default (4), async 12 | 1110 MiB | 413 MiB |
| `prefetch=16`, async 8 | 1165 MiB | 468 MiB |

the default serial row is the budget being spent, 192 MiB of cache plus slack.
the async rows are above it, and the frames above it are the claimed ones: on
27.7 MiB frames, 8 in flight is 222 MiB of payload the pool did not plan and
cannot evict.

from the code rather than a measurement: `plan_window` computes
`committed_bytes` and skips every candidate that does not fit, so a claimed set
of eight disables the window until it drains. that is why
[46](46-request-thread-decode.md) finds four workers worth 0.2% at that depth —
there is nothing left for them to be planned.

## change

1. record which in-flight entries a worker owns and which a consumer claimed,
   and leave the claimed ones out of `committed_bytes`. the pool can neither
   evict them nor refuse them, so counting them only turns the consumer's
   pipelining into the pool's starvation.
2. keep `evict` as it is. it only ever touched `ready`, and this change does not
   give it a new kind of victim.
3. say what the argument bounds: the cache and the queue, not the graph. a deep
   consumer's memory is the depth it chooses times the frame it asks for, and
   the plugin's own accounting is not the thing to size it by.

## decision

**accounting, not a ceiling.** the alternative — refusing or delaying a claim
that does not fit the budget — was rejected in
[01](01-lookahead-scheduling.md) and is rejected again here: a frame request the
graph has already made must be answered, and a budget is a policy about
speculative work, not about work that was asked for.

**the deeper consumer is not the pool's problem to fix.** a consumer that keeps
twelve 27.7 MiB frames in flight holds 332 MiB for its own reasons. this change
does not lower that number and does not claim to; it stops that number from
turning the pool off.

## acceptance

- `read-async.py --memory` at 8 and 12 in flight: peak working set unchanged or
  lower, because this change must not add memory. it moves the accounting, not
  the data.
- a unit test in `src/prefetch.rs`: a claimed in-flight index does not prevent
  the next index from being queued, which is the `plan_window` behaviour this
  plan exists to protect.
- the serial rows identical, since no claim happens there beyond the one the
  consumer owns.
- `docs/BENCH.md` gains the memory table and the sentence that `prefetch_memory`
  bounds the pool.
