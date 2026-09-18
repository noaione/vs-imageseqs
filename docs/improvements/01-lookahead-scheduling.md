# 01 — lookahead scheduling

- status: implemented
- touches: `src/prefetch.rs`, `src/source.rs`
- expected: webp 88.8 → ~70 ms/frame at `prefetch=4`, and `prefetch` above 4
  stops being a pessimisation
- outcome: the window stops at the byte budget, the budget follows the window by
  default and `prefetch_memory` overrides it. `prefetch=16` is no longer the
  worst row anywhere, the sandbox webp set turns 11.36 s into 4.12 s, and the
  wasted cpu at the deepest depth falls from 2.58x to 1.43x of one decode per
  delivered frame
- risk: low, internal only, no behaviour change other than fewer wasted decodes

## problem

`Prefetcher::plan` queues every frame from `index + 1` to `index + window`,
where `window = (workers + 2).min(MAX_WINDOW)` and `MAX_WINDOW` is 16. decoded
frames wait in `State::ready`, bounded by two limits: `entry_cap`
(`window + 4` entries) and `READY_BYTE_BUDGET` (192 MiB).

`evict` runs after every `store`. When the budget is exceeded it drops the
oldest frame behind the newest request if there is one, and otherwise the
furthest frame ahead — but by definition everything in `ready` is ahead of the
consumer when it is running at full stretch. `plan` then sees those frames
missing from `ready`, `in_flight` and `queue`, so it queues them again. The pool
decodes them a second time, and the extra decode competes with the frames the
consumer actually needs next.

Three parts of the same issue:

- the delivered frames counted too. `ready` held the frames the consumer had
  already taken, so once the cache filled it stayed full: the pool had no room
  to plan into and simply stopped ahead of the consumer. that is what the first
  attempt at this change got wrong, see 3. in `## change`.
- only `ready` is budgeted. a frame being decoded by a worker counts nothing
  towards the budget, so the pool can hold 192 MiB of results *and* up to 16
  frames in flight.
- the window is derived from the worker count, not from how many frames fit.
  For 36 MB frames, `budget / frame = 5.6`, so 16 workers can never all be
  useful.

## evidence

webp, 2903x4128, 36 MB decoded per frame, first 32 files, wall clock and
process CPU time per frame:

| prefetch | wall | cpu | cpu / one decode |
| --- | --- | --- | --- |
| 0 | 272.5 ms | 268.1 ms | 1.00x |
| 2 | 128.5 ms | 285.6 ms | 1.07x |
| 4 | 88.8 ms | 360.8 ms | 1.35x |
| 6 | 78.2 ms | 431.2 ms | 1.61x |
| 8 | 95.6 ms | 595.7 ms | 2.22x |
| 12 | 112.6 ms | 752.9 ms | 2.81x |
| 16 | 136.2 ms | 925.3 ms | 3.45x |

one decode is ~268 ms of cpu, so the cpu column should stay near 1.0-1.2x. it
reaches 3.45x, and the wall time turns around after `prefetch=6`.

the jpeg set is the control: 8 MB frames, 23 of them fit the budget, cpu stays
at 1.0-1.25x for every depth, and the wall curve is flat because a different
limit takes over (the copy, see [02](02-frame-write-path.md)).

the sandbox webp set is the wide-frame extreme: 35 colour pages, 34 of them
3672x5274 and 55.4 MiB decoded, plus `p000` at 3312x4717 and 44.7 MiB. the old
192 MiB budget held three and a half of those frames, so a six frame window was
already wider than what could be kept:

| prefetch | wall | cpu | cores busy | cpu over serial |
| --- | --- | --- | --- | --- |
| 0 | 590.4 ms | 585.0 ms | 0.99 | 1.00x |
| 2 | 329.4 ms | 685.5 ms | 2.08 | 1.17x |
| 4 | 169.1 ms | 680.7 ms | 4.02 | 1.16x |
| 8 | 245.4 ms | 1232.4 ms | 5.02 | 2.10x |
| 16 | 297.4 ms | 1513.7 ms | 5.09 | 2.58x |

the default at 4 is already efficient here, which is the point: the pool is
100% busy and wastes 1.16x, and going past it adds nothing because the window
is wider than the budget. this set is the one that shows part 2 of the change
matters on its own — a 16 frame window cannot be useful when 3.5 frames fit,
and every depth above 4 only re-decodes evicted frames (2.58x the cpu at 16,
and a wall time 1.8x the default).

## change

1. account for the whole pipeline. `committed_bytes` sums what is ready, what a
   worker is decoding and what is still queued, and `plan_window` stops walking
   the window as soon as the next frame would push that total past the budget.
2. never queue what cannot be retained, and cap the window itself. `plan_window`
   is bounded by `budget / bytes_per_frame` in addition to `workers + 2` and
   `MAX_WINDOW`, so a window wider than the budget no longer queues frames that
   are certain to be evicted.
3. let the consumer release the budget it has already used. `make_room` drops
   cached frames behind the current index before planning, so a `ready` map full
   of already-delivered frames cannot starve the pool. this was not part of the
   original proposal and step 1 alone is not enough without it: measured that
   way, `prefetch=4` regressed from 169.1 to 396.1 ms/frame on the sandbox webp
   set, because the pool was idle while the cache sat at the budget.
4. keep `evict` as a backstop. variable-size sequences make any upfront
   estimate approximate, so the eviction path stays, it should just never be
   reached for a uniform sequence again.

frame size: `ImageInfo` already carries width, height and the decoded
`ColorType`, so `frame_bytes()` gives the exact size of each frame once per
clip into an `Arc<[usize]>`, and the queued range is summed exactly instead of
estimated. no new plumbing is needed in `prefetch.rs` beyond that slice.

### the budget argument

the budget is `automatic_budget(images, window)` unless the caller overrides it:
`max(192 MiB, window * largest_frame)`. the floor keeps every sequence that fit
192 MiB on exactly the memory it used before, and the window term is what lets a
large-frame sequence use the depth it was asked for. `prefetch_memory` (MiB)
replaces it when a hard ceiling matters more than depth.

## decision — the budget

192 MiB at 36 MB per frame means ~5 useful decodes on a 12 thread machine, and
the scheduling fix does not change that. it only stops the pool from wasting
work on frames it cannot hold. rather than choose between the three options the
proposal listed, the change takes all of them at once:

- keep the floor: 192 MiB stays the value when the frames fit it, so small and
  medium sequences are unchanged. the jpeg and png sets confirm this.
- scale it: `max(192 MiB, window * largest_frame)`, so asking for eight workers
  on 55.4 MiB frames also asks for 448 MiB and gets eight useful decodes rather
  than three and a half.
- expose it: `prefetch_memory`, in MiB, for the caller who wants the fixed
  tradeoff back. `prefetch_memory=0` is rejected with a hint to use `prefetch=0`,
  because that is what disabling lookahead means.

the resolved budget is printed in the `create:` line when `debug=True`, and
nothing else in the pipeline reads it — it is purely a lookahead knob.

worth remembering from the proposal: at 8 useful decodes the webp set is
decode-bound at 268 / 8 = 34 ms/frame, and the copy floor is ~53 ms, so the
budget only turns into wall time once [02](02-frame-write-path.md) or
[03](03-webp-yuv-output.md) lowers that floor. the scheduling half was still the
prerequisite, because it is what makes the depth real.

## result

measured on the sandbox sets after the change, best pass of three. the depth
curve is the metric this plan was written around: first 16 files of the webp
set, where one serial frame is 563 ms of cpu.

| prefetch | wall before | wall after | cpu before | cpu after | cpu over serial |
| --- | --- | --- | --- | --- | --- |
| 0 | 590.4 ms | 571.3 ms | 585.0 ms | 562.5 ms | 1.00x |
| 2 | 329.4 ms | 263.0 ms | 685.5 ms | 576.2 ms | 1.02x |
| 4 | 169.1 ms | 160.7 ms | 680.7 ms | 648.4 ms | 1.15x |
| 6 | none | 132.9 ms | none | 731.4 ms | 1.30x |
| 8 | 245.4 ms | 124.0 ms | 1232.4 ms | 758.8 ms | 1.35x |
| 12 | none | 126.3 ms | none | 801.8 ms | 1.43x |
| 16 | 297.4 ms | 145.6 ms | 1513.7 ms | 806.6 ms | 1.43x |

the curve is monotonic to 8 and flat to 12; 16 workers on a 12 thread machine
costs wall time again without spending more cpu on average. the per-set totals
are in [BENCH.md](../BENCH.md), and the sets that changed most are the ones whose
frames did not fit the old fixed budget:

| sandbox set | default before | default after | `prefetch=16` before | after |
| --- | --- | --- | --- | --- |
| webp | 5.88 s | 5.66 s | 11.36 s | 4.12 s |
| avif | 9.25 s | 4.74 s | 19.11 s | 5.41 s |
| heic | 7.50 s | 8.24 s | 7.92 s | 5.63 s |

the heic default is the one row that looks worse, and its serial row moved the
same way in that pair of runs (24.11 → 25.17 s), which is machine drift: the
change does not touch the `prefetch=0` path. its `prefetch=16` row, which the
change does govern, went from 6% slower than the default to 46% faster.

the sets that already fit 192 MiB are unchanged apart from the deepest depth:
jpeg 2.87 → 2.75 s and png 0.66 → 0.62 s at the default, jxl 6.42 → 6.50 s
(within run to run noise) with its `prefetch=16` row at 5.68 → 4.57 s, and the
`prefetch=0` rows identical everywhere, which is the control this plan asked
for. nothing in the change touches the serial path.

## acceptance

- webp set with `--prefetch 16`: cpu per delivered frame back to ~1.0-1.2x, and
  `prefetch=16` no longer slower than `prefetch=4`. **partly met**: 2.58x → 1.43x
  of one decode per delivered frame, and `prefetch=16` is now faster than
  `prefetch=4` (145.6 against 160.7 ms/frame), but the remaining overhead is the
  price of any lookahead on a 12 thread machine, so 1.0-1.2x was never reachable
  at depth 16. the depth curve is monotonic up to 8.
- sandbox webp set with `--prefetch 16`: total 11.36 s → below the default's
  5.88 s, with cpu per delivered frame near the default's 1.16x. **met**: 4.12 s,
  below the new default's 5.66 s, at 1.43x. the per-frame arrival is also
  steadier: median 70.2 ms against 107.4 ms.
- webp set with `--prefetch 4`: total 15.80 s → ~13 s. **not re-measured**: the
  manga webp set was not part of this round. the sandbox webp set is the same
  shape (large colour frames) and improved, so the row is expected to move the
  same way.
- jpeg and png sets: unchanged (no evictions there today, so any change is a
  regression). **met**: jpeg 2.87 → 2.75 s and png 0.66 → 0.62 s at the default,
  with the `prefetch=0` rows identical to the millisecond. the only depth that
  changed is `prefetch=16` on png, 0.89 → 0.60 s, because the frames fit the
  automatic budget either way.
- unit test in `src/prefetch.rs`: a pool whose budget is smaller than one frame
  still delivers every frame, and never queues more than the budget allows.
  `shares_one_decode_between_consumers` and `retries_failed_decodes` must keep
  passing, since both touch the queue and the ready map. **met, with one
  correction**: "never commits more than the budget" is not a valid runtime
  invariant, because the frame the consumer is being handed can legitimately
  exceed it, and the test was timing-dependent. the shipped tests assert the
  pure functions instead — `the_window_stops_at_the_budget`,
  `a_budget_too_small_for_one_frame_still_delivers`,
  `a_budget_that_fits_the_window_queues_all_of_it`,
  `a_frame_that_does_not_fit_is_skipped_not_stopped`,
  `delivered_frames_are_dropped_to_keep_the_lookahead_running`,
  `committed_bytes_counts_ready_decoding_and_queued` and
  `automatic_budget_keeps_the_floor_and_follows_the_window`.

`cargo test --locked` is 30 tests, all passing, with `cargo clippy -- -D
warnings` and `cargo fmt --check` clean.
