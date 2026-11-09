# 47 — what the byte budget counts

- status: implemented
- touches: `src/prefetch.rs`, `src/source.rs`
- expected: `prefetch_memory` bounds what the pool holds, and a deep async
  consumer stops starving the lookahead it is not using
- outcome: the accounting is fixed and both new unit tests hold it, but no wall
  time moved: every row is within run-to-run noise of the build before it, and
  peak memory is 1043 → 1065 MiB at eight frames in flight and 1110 → 1104 MiB
  at twelve, which is noise in both directions. It is kept because the defect it
  removes is real and silent, not because it is faster
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

| request style | before | after |
| --- | ---: | ---: |
| `prefetch=0`, serial | 697 MiB | 697 MiB |
| default (4), serial | 911 MiB | 911 MiB |
| default (4), async 8 | 1043 MiB | 1065 MiB |
| default (4), async 12 | 1110 MiB | 1104 MiB |

the default serial row is the budget being spent, 192 MiB of cache plus slack.
the async rows are above it, and the frames above it are the claimed ones: on
27.7 MiB frames, 8 in flight is 222 MiB of payload the pool did not plan and
cannot evict.

wall time, before and after, best of three. every difference is inside the
spread this machine shows at fixed settings ([45](45-async-request-cache.md)
records the same runs at up to 20%):

| request style | before | after |
| --- | ---: | ---: |
| `prefetch=0`, serial | 9.315 s | 9.310 s |
| default (4), serial | 2.683 s | 2.693 s |
| default (4), async 4 | 2.637 s | 2.634 s |
| default (4), async 8 | 1.724 s | 1.689 s |
| default (4), async 12 | 1.544 s | 1.601 s |
| `prefetch=16`, async 8 | 1.453 s | 1.425 s |
| `ReadAlpha` branches, async 8 | 3.192 s | 3.128 s |
| `ReadAlpha` branches, async 8, `prefetch=16` | 1.594 s | 1.566 s |

so the plan's `## expected` is met and its implied speedup is not. What was
verified instead is the mechanism, in
`a_claimed_frame_does_not_stop_the_lookahead`: a state whose budget holds one
frame, whose only in-flight frame is claimed by a consumer, still queues the next
frame it is asked to plan. Before this change `committed_bytes` counted those
bytes and `plan_window` skipped every candidate instead.

## change

1. `State` records `claimed`, the subset of `in_flight` a consumer thread is
   decoding. `claim` inserts into it, and `store` removes it, so a claim is open
   exactly while the caller is building the payload.
2. `committed_bytes` sums `ready`, `queue` and the in-flight frames a worker
   owns, and leaves the claimed ones out. The pool can neither evict them nor
   refuse them, so counting them only turned the consumer's pipelining into the
   pool's starvation.
3. `resolve_prefetch_memory`'s documentation says what the number bounds: the
   pool's cache and queue, not the graph.

## decision

**accounting, not a ceiling.** the alternative — refusing or delaying a claim
that does not fit the budget — was rejected in
[01](01-lookahead-scheduling.md) and is rejected again here: a frame request the
graph has already made must be answered, and a budget is a policy about
speculative work, not about work that was asked for.

**kept without a speedup.** the honest reading of the tables is that this change
does not make anything faster on the corpus measured. It is kept because the
behaviour it removes is silent and surprising: a consumer that pipelines more
deeply than the budget holds turns its own lookahead off, and nothing reports it.
The cost is one `HashSet` membership test per in-flight frame in a function that
already walks three collections.

**the deeper consumer is not the pool's problem to fix.** a consumer that keeps
twelve 27.7 MiB frames in flight holds 332 MiB for its own reasons. this change
does not lower that number and does not claim to; it stops that number from
turning the pool off.

## acceptance

- `read-async.py --memory` at 8 and 12 in flight: peak working set unchanged or
  lower. **met within noise**: +22 MiB at eight, -6 MiB at twelve, which is less
  than one 27.7 MiB frame in either direction.
- a unit test that a claimed in-flight index does not prevent the next index
  from being queued. **met**: `a_claimed_frame_does_not_stop_the_lookahead`, and
  `a_claimed_frame_is_not_the_pools_to_account_for` holds the accounting it
  rests on.
- the serial rows identical. **met**: 2.683 → 2.693 s, and `prefetch=0` unchanged
  at 9.31 s.
- `docs/BENCH.md` gains the memory table and the sentence that `prefetch_memory`
  bounds the pool. **met**, and the tables there are re-recorded from this build.

`cargo test --locked` is 372 tests, `cargo clippy -- -D warnings` and
`cargo fmt --check` are clean, and `tests/readalpha.vpy` passes.
