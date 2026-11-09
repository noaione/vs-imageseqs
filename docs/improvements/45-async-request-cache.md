# 45 — the second clip of a call

- status: proposed
- touches: `src/prefetch.rs`, `src/clip.rs`
- expected: the two clips of a `ReadAlpha` call stay one decode per frame when
  the consumer walks them with a lag, instead of one decode per clip
- risk: low for a one-clip call, which holds nothing longer than it does today

## problem

`Read` hands out one clip and `ReadAlpha` two, and both come from one
`Prefetcher<FrameBuilder>` ([`src/source.rs`](../../src/source.rs)). The pool is the thing
that makes "a file is decoded once per frame" true across the two clips: a
payload is a `ClipFrames`, which builds a frame for every clip of the call from
one decode, and each clip asks `fetch(n)` for itself.

That share only holds while the payload is still in `State::ready`, and `ready`
is bounded twice: by `entry_cap` (`window + 4` entries) and by the byte budget.
Whichever is smaller binds, and the budget is a *lookahead* number —
`max(192 MiB, window * largest_frame)` — while the cache it also has to serve is
a *multi-consumer* one. Nothing in the accounting knows that a call can have
two consumers.

## evidence

`sandbox/webp`, 35 pages of 3672x5274 `YUV420P8`, 27.7 MiB decoded per frame,
i5-11400H (6 cores, 12 threads), release build, best pass of two, measured with
`target/bench/read-async.py --alpha`. One decode plus its frame write is about
300 ms of cpu per delivered frame at the default, which is what every other row
is a ratio of.

| two clips of one call | wall | ms per delivered frame | cpu per delivered frame |
| --- | ---: | ---: | ---: |
| interleaved, default | 2.733 s | 78.1 | 307 (1.0x) |
| branches, default | 7.037 s | 201.0 | 533 (1.78x) |
| interleaved, async 8 | 2.007 s | 57.3 | 319 |
| branches, async 8 | 3.317 s | 94.8 | 638 (2.13x) |
| branches, async 8, `prefetch=16` | 2.460 s | 70.3 | 502 (1.67x) |

asking both clips for the same frame is free and costs nothing extra. walking
one clip to the end and then the other decodes every file a second time, and the
cpu column is the receipt: 1.78x and 2.13x of a full decode per delivered frame.
async makes it worse rather than better, because what it widens is exactly the
distance between the two branches.

the retention arithmetic says the missing thing is retention and not decode:

| depth | window | budget | payloads the budget holds | `entry_cap` |
| --- | ---: | ---: | ---: | ---: |
| default (4) | 6 | 192 MiB (the floor, since 6 x 27.7 is 166) | 6.9 | 10 |
| 16 | 16 | 443 MiB | 16 | 20 |

the byte budget binds at the default and the entry cap does not, and doubling
the budget is what turns 2.13x into 1.67x. so more retention is the axis that
works, and it is currently derived from a number that has nothing to do with how
many clips the call hands out.

## change

1. `Prepare` gains `consumers(&self) -> usize`: how many clips one payload
   serves. `Read` answers 1, `ReadAlpha` 2, and `FrameBuilder` answers
   `clips.len()` rather than a constant.
2. `automatic_budget` is told it, and the window term becomes
   `window * largest_frame * consumers`. The floor stays 192 MiB, so a call
   whose frames fit it is unchanged.
3. `evict`, `make_room` and `plan_window` prefer to drop a payload that no clip
   still wants over one that only the first clip has taken. `ClipFrames` records
   how many times it has been handed out, which is also what makes a repeated
   request for one index cost nothing.
4. A payload is never exempt from the budget. What the budget cannot hold is
   evicted and decoded again, and `## decision` says why that is accepted rather
   than worked around.

## decision

**an unbounded hold keyed by frame index is rejected.** the branch split that
the evidence shows would need 35 x 27.7 MiB, which is 970 MB on this set, and
`files:data[]` has no upper bound, so the shape of the fix cannot be "keep
everything until every clip has had it".

**narrowing the re-decode to the plane it needs is rejected.** a `Demand` that
skipped the colour plane when only the alpha clip asks would save the frame
allocation and the write, about 5 ms of a 250 ms frame, and not the decode that
is the whole 1.78x.

**sizing retention by the call's clips is taken.** it is exact for a sequence
whose total size fits the derived budget, and merely better for one that does
not, which is the best a bounded cache can do.

**the residual is documented rather than hidden.** a consumer that walks one
clip to the end and then the other decodes every file twice, whatever the
retention is, because the distance between the two requests is the sequence
length. the README's `ReadAlpha` paragraph should say that asking for both clips
of a frame together is free and that splitting them is not.

**correction to the analysis this plan came from.** the claim that this change
fixes the branch split was wrong: it cannot, for the reason above. what it fixes
is the lag the two clips may drift apart, which is the budget rather than the
window, and that is the acceptance below.

## acceptance

- the first thing to measure, and not yet measured: the largest lag between the
  two clips that still delivers one decode per frame. a new row of
  `read-async.py` asks for colour frame `n` and alpha frame `n - L` and reports
  cpu per delivered frame against `L`. before the change it should cross 1.05x
  at about the single-clip budget (6 payloads on this set); after it, at about
  twice that. the change is worth landing only if the crossing point moves by
  roughly `consumers`.
- the interleaved rows must not move: 307 cpu per delivered frame at the
  default, and 319 at 8 in flight.
- `sandbox/jpeg`, whose 18.5 MiB frames make `entry_cap` the binding limit
  rather than the budget, is where the change should show at the default depth.
  measure it before believing the arithmetic generalises.
- `tests/readalpha.vpy` must pass unchanged, and `cargo test --locked` must keep
  `shares_one_decode_between_consumers` passing in `src/prefetch.rs`.
