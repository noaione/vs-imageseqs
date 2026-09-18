# 01 — lookahead scheduling

- status: proposed
- touches: `src/prefetch.rs`
- expected: webp 88.8 → ~70 ms/frame at `prefetch=4`, and `prefetch` above 4
  stops being a pessimisation
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

Two smaller parts of the same issue:

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

the sandbox webp set is the wide-frame extreme. its pages are 3672x5274, 55 MiB
for a colour frame and 18.5 MiB for the 31 gray ones, so the budget holds 3.5
and 10 of them respectively:

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

1. account for the whole pipeline. add `scheduled_bytes` to `State`, added when
   a decode is queued and removed when the frame is released, evicted or its
   decode fails, so `ready_bytes + scheduled_bytes` never exceeds the budget.
2. never queue what cannot be retained. in `plan`, stop walking the window once
   `scheduled_bytes` reaches the budget, and cap the window itself by
   `budget / bytes_per_frame`.
3. keep `evict` as a backstop. variable-size sequences make any upfront
   estimate approximate, so the eviction path stays, it should just never be
   reached for a uniform sequence again.

frame size: `ImageInfo` already carries width, height and the decoded
`ColorType`, so the queued range can be summed exactly instead of estimated.
`ColorType::bytes_per_pixel()` gives the per-pixel cost, so no new plumbing is
needed for `prefetch.rs` beyond reading it.

## open question — the budget itself

192 MiB at 36 MB per frame means ~5 useful decodes on a 12 thread machine, and
the scheduling fix does not change that. it only stops the pool from wasting
work on frames it cannot hold. options, in ascending order of ambition:

- keep 192 MiB and accept that `prefetch` saturates at ~5 for very large
  frames, documenting that the window is capped by the budget.
- scale it: `max(192 MiB, window * bytes_per_frame)`, so asking for 8 workers
  also asks for ~290 MB.
- expose it: an argument such as `prefetch_size`, defaulting to today's value.

all three need a decision on how much memory a source filter may claim, and
that is a user-visible tradeoff, so it is deliberately not part of the
scheduling fix. worth measuring: at 8 useful decodes the webp set would be
decode-bound at 268 / 8 = 34 ms/frame, but the copy floor is ~53 ms, so the
gain only appears once [02](02-frame-write-path.md) or
[03](03-webp-yuv-output.md) lowers the floor. that makes the budget question
lower priority than it looks.

## acceptance

- webp set with `--prefetch 16`: cpu per delivered frame back to ~1.0-1.2x, and
  `prefetch=16` no longer slower than `prefetch=4`.
- sandbox webp set with `--prefetch 16`: total 11.36 s → below the default's
  5.88 s, with cpu per delivered frame near the default's 1.16x.
- webp set with `--prefetch 4`: total 15.80 s → ~13 s.
- jpeg and png sets: unchanged (no evictions there today, so any change is a
  regression).
- unit test in `src/prefetch.rs`: a pool whose budget is smaller than one frame
  still delivers every frame, and never queues more than the budget allows.
  `shares_one_decode_between_consumers` and `retries_failed_decodes` must keep
  passing, since both touch the queue and the ready map.
