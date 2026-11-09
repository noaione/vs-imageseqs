# 45 — the second clip of a call

- status: implemented
- touches: `src/prefetch.rs`, `src/clip.rs`
- expected: the two clips of a `ReadAlpha` call stay one decode per frame when
  the consumer walks them with a lag, instead of one decode per clip
- outcome: the lag a call's clips may drift apart before costing a second
  decode moved from 1 frame to 10, and that second decode costs 1.5x rather
  than 4.2x. `ReadAlpha` read as two branches with `prefetch=16` is 2.07x
  faster, and every one-clip row is unchanged
- risk: low for a one-clip call, which holds nothing longer than it did before

## problem

`Read` hands out one clip and `ReadAlpha` two, and both come from one
`Prefetcher<FrameBuilder>` ([`src/source.rs`](../../src/source.rs)). The pool is
the thing that makes "a file is decoded once per frame" true across the two
clips: a payload is a `ClipFrames`, which builds a frame for every clip of the
call from one decode, and each clip asks `fetch(n)` for itself.

That share only holds while the payload is still in `State::ready`, and `ready`
is bounded by a *lookahead* number — `max(192 MiB, window * largest_frame)`,
plus `entry_cap` entries. Nothing in the accounting knows that a call can have
two consumers.

The other half of the problem turned out to matter more, and the measurement in
`## evidence` is what found it. `Prefetcher::plan` decides whether a request is
part of the same sequential walk from a single `last_index`, and a call whose two
clips are consumed as separate branches produces requests that alternate between
two positions. Every lagging request lowered `last_index` to its own index, so the
next forward step read as a seek: the generation was bumped, the queue was
cleared, and `worker` discards any result that finishes after a bump. A consumer
with two cursors was paying for its lookahead twice over.

## evidence

`sandbox/webp`, 35 pages of 3672x5274 `YUV420P8`, 27.7 MiB decoded per frame,
`ReadAlpha` payloads 46.2 MiB, i5-11400H (6 cores, 12 threads), release build.
Measured with `target/bench/read-async.py`, whose `--lag` row asks for colour
frame `n` together with alpha frame `n - lag`: the distance two branches of one
graph drift apart. `x` is cpu per delivered frame over one shared decode.

baseline, before this change:

| lag | wall | cpu/frame | x |
| ---: | ---: | ---: | ---: |
| 0 | 2.777 s | 303 | 1.00 |
| 2 | 15.385 s | 1411 | 4.66 |
| 4 | 13.882 s | 1282 | 4.23 |
| 8 | 13.609 s | 1215 | 4.01 |
| 12 | 12.294 s | 1172 | 3.87 |
| 16 | 11.562 s | 972 | 3.21 |

`prefetch=0`, which has no workers, no queue and therefore no generation to bump,
is the control that says the pool is the cause and not the cache alone:

| lag | cpu/frame | x |
| ---: | ---: | ---: |
| 0 | 269 | 1.00 |
| 4 | 345 | 1.28 |
| 8 | 431 | 1.60 |

so the four background workers made a lagging two-clip read 3.3x *worse* than no
pool at all at lag 4. A cache that is too small cannot do that; discarding
finished decodes can.

the same set with both clips walked as branches rather than interleaved:

| two clips, branches | before | after |
| --- | ---: | ---: |
| serial, wall / cpu | 7.697 s / 575 | 7.927 s / 449 |
| async 8, wall / cpu | 4.005 s / 636 | 3.192 s / 578 |
| async 8 `prefetch=16`, wall / cpu | 3.297 s / 703 | 1.594 s / 378 |

## change

1. `Prepare` gains `consumers(&self) -> usize`: how many clips one payload
   serves. `Read` answers 1, `ReadAlpha` 2, and `FrameBuilder` answers
   `clips.len()`.
2. `automatic_budget` is told it, and the window term becomes
   `window * largest_frame * consumers`. `entry_cap` becomes
   `window * consumers + READY_ENTRY_MARGIN`. The floor stays 192 MiB.
3. `State` records how many hand-outs each cached payload still owes, `store`
   sets it from `consumers`, `fetch` decrements on every hand-out, and
   `pick_victim` prefers a payload at zero. A payload a second clip still wants
   is the last thing `evict` and `make_room` drop, and the budget still binds:
   nothing is exempt.
4. `plan` treats a request behind the newest position as part of the same walk
   rather than a seek, and keeps `last_index` as a high-water mark instead of
   lowering it. Only a forward jump bumps the generation. This is the half the
   measurement demanded; without it the other three change nothing measurable.

## decision

**the budget half alone was rejected by its own acceptance.** the plan was
written to size retention by the clips of a call, and that alone leaves lag 2 at
4.66x. it was kept because it is what makes lag 10 to 16 cost 1.5x instead of
3.2x, not because it fixes the small lags.

**an unbounded hold keyed by frame index is rejected.** the branch split that
the table shows would need 35 x 46.2 MiB, and `files:data[]` has no upper bound,
so the shape of the fix cannot be "keep everything until every clip has had it".
lag 10 and beyond still re-decodes, and 1.5x is the price of that.

**any request behind the newest position counts as the same walk.** a large
backwards seek therefore no longer clears the queue. what that risks is bounded
by the window: the pool keeps at most `window` frames queued ahead of a position
the consumer has left, `plan_window` re-plans from wherever the consumer really
is, and the stale entries are decoded at most once before they are evicted. A
seek that has to reset the lookahead costs a stale decode; a seek that does not
is a small replay, so the reset was not worth the price it charged a two-clip
call.

**narrowing the re-decode to the plane it needs is rejected.** a `Demand` that
skipped the colour plane when only the alpha clip asks would save the frame
allocation and the write, a few ms of a 250 ms frame, and not the decode.

**the residual is documented rather than hidden.** a consumer that walks one
clip to the end and then the other still decodes every file twice, because the
distance between the two requests is the sequence length and no bounded cache
holds 35 payloads. the README's `ReadAlpha` paragraph should say that asking for
both clips of a frame together is free and that a whole-sequence split is not.

## result

same set and machine, `--lag`, after the change:

| lag | wall | cpu/frame | x | before x |
| ---: | ---: | ---: | ---: | ---: |
| 0 | 2.798 s | 304 | 1.00 | 1.00 |
| 2 | 2.761 s | 304 | 1.00 | 4.66 |
| 4 | 2.761 s | 304 | 1.00 | 4.23 |
| 6 | 2.772 s | 302 | 1.00 | 4.26 |
| 8 | 2.741 s | 305 | 1.00 | 4.01 |
| 10 | 6.332 s | 460 | 1.52 | 4.28 |
| 12 | 6.483 s | 459 | 1.51 | 3.87 |
| 16 | 6.749 s | 455 | 1.50 | 3.21 |

lag 2 is 5.57x faster and lag 16 is 1.71x faster. the crossing point moved from
1 to 10, which is more than the window because the budget grew with the number
of clips the call hands out: retention is 12 payloads at the default and 6.9
before.

the one-clip rows did not move, which is the control this change needs since
`consumers` is 1 for `Read`:

| request style | before | after |
| --- | ---: | ---: |
| `prefetch=0`, serial | 10.061 s | 9.315 s |
| default, serial | 2.742 s | 2.683 s |
| default, async 8 | 1.829 s | 1.724 s |
| `prefetch=16`, serial | 1.554 s | 1.535 s |

`ReadAlpha` interleaved, which was already one decode per frame, also improved,
from 3.458 s and 344 cpu/frame to 2.761 s and 306: at lag 0 the two clips ask for
the same index and the high-water mark is what keeps that from reading as a seek
when the graph schedules them out of order.

## acceptance

- the largest lag that still delivers one decode per frame. **met and exceeded**:
  it is 10 against the 6 the plan expected, because the budget doubles with
  `consumers` and the old crossing was 1 rather than 6.
- the interleaved rows must not move. **met, and improved**: 344 → 306 cpu per
  delivered frame at the default, 406 → 321 at 8 in flight.
- `sandbox/jpeg`, whose frames are smaller, is where `entry_cap` binds rather
  than the budget. **not measured**: the corpus this plan was judged on is webp,
  and the arithmetic that predicts jpeg is unchanged from `## evidence`.
- `tests/readalpha.vpy` passes, `cargo test --locked` is 370 tests, and
  `cargo clippy -- -D warnings` and `cargo fmt --check` are clean. Four new unit
  tests hold the change: `a_budget_follows_the_consumers_of_a_call`,
  `a_two_clip_call_caches_two_clips_worth`,
  `a_payload_a_second_clip_still_wants_is_the_last_one_dropped` and
  `a_clip_that_lags_behind_is_not_a_new_access_pattern`.
