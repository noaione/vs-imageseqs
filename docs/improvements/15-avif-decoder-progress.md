# 15 — Terminate AVIF decoding when no picture can be produced

- status: implemented
- touches: `src/formats/avif.rs`, `tests/make-alpha-fixtures.py`,
  `tests/fixtures/avif-no-picture.avif` (new), `tests/readalpha.vpy`,
  `CHANGELOG.md`, `docs/BENCH.md`, `docs/IMPLEMENTATION.md`, `docs/HANDOFF.md`
  and the index row
- depends on: nothing. It is the first of the 2026-09-27 review to land, and
  [17](17-avif-container-robustness.md)'s malformed-file campaign and
  [19](19-avif-thread-budget.md)'s benchmarks both wait for it.
- expected: a frame request on an item that holds no picture ends with a
  path-qualified error instead of never returning, and the frames of every valid
  file are unchanged
- result: the loop ends on decoder progress instead of on a call that cannot make
  progress, and the item decoder is created for low-latency output
  (`max_frame_delay = 1`) so that "the decoder has all of the item and has no
  picture" is a fact rather than a race. `tests/readalpha.vpy` asks the committed
  fixture for a frame in a child process with a timeout and passes 330 checks;
  the pre-change build fails those two checks by timeout. All 120 data lines of
  `frame-parity.py` are byte identical, and the low-latency decode made the
  affected sets faster rather than slower: `sandbox/avif` 2284.3 → 1926.9 ms of
  decode (0.844) and `sandbox/hitokage-sample` 713.9 → 407.1 ms (0.570), with the
  untouched sets inside their drift.
- risk: low — an error path plus the decoder's own settings. The samples are not
  touched, and every valid file's frames are identical plane for plane.

## problem and evidence

`decode_item` in `src/formats/avif.rs` submits one AV1 item, then loops over
`get_picture()` and `send_pending_data()`. Both `Ok(())` and `Again` from the
second call cause another iteration, without tracking whether input remains.
Conversely, `Again` from the initial `send_data()` is treated as a fatal error.
These are two sides of the same incomplete input/output state machine.

The local `target/investigation/no-picture.avif` passes probing as 3x2 but a
`Read(..., prefetch=0).get_frame(0)` request did not finish within five seconds.
The subprocess was killed by the test timeout. This was reproduced using the
Windows wheel validated in plan 14; this is an observed timeout, not a measured
upper bound on how long the current implementation might run.

The pinned dav1d Rust wrapper is 0.11.1; its `send_pending_data()` returns
`Ok(())` immediately when no pending data exists. The upstream
[dav1d 1.5.3 API contract](https://github.com/videolan/dav1d/blob/1.5.3/include/dav1d/dav1d.h)
says input-side EAGAIN requires draining output, while output-side EAGAIN at the
end of draining means there is no further picture. Repeating an empty
submission does not supply the missing coded frame.

## what it did, measured

The hang is confirmed and it is unbounded, not slow: a frame request on
`target/investigation/no-picture.avif` was still running when the harness killed
it at 45 s, and the committed fixture did the same at 30 s. Both files pass
probing first, which is what puts them on the native decode path:
`avif-no-picture.avif` is described as a 3x2 `YUV444P8` page and then has no
picture to hand over.

**the plan's own premise was incomplete, and the first implementation failed on
it.** "The decoder has all of the input and answers `Again`" is not yet "the item
holds no picture", because dav1d's default frame delay is
`ceil(sqrt(n_threads))` frames: the picture is decoded on a worker thread, so the
first `get_picture` answers `Again` and the second returns the picture. A real
sequence header from `avif-yuv420p.avif`, with the frame that follows it cut off,
was used to trace this rather than a hand-made header dav1d rejects:

| item | settings | `send_data` | `get_picture` |
| --- | --- | --- | --- |
| `avif-yuv420p.avif` (valid) | default | `Ok` | `Again`, then the picture |
| `avif-yuv420p.avif` (valid) | `max_frame_delay = 1` | `Ok` | the picture |
| `avif-no-picture.avif` | default | `Ok` | `Again` forever |
| `avif-no-picture.avif` | `max_frame_delay = 1` | `Ok` | `Again` forever |
| `snek - p004.avif` (6.8 MB coded) | `max_frame_delay = 1` | `Ok` | the picture |

So the loop asks for low-latency output, which is the honest setting for one
item: a still image is one frame, so a frame delay can only add a pipeline the
plugin cannot use. `n_threads` is left at its default, so the tile threads that
decode one page in parallel are unchanged — and the measurements below say the
pipeline was not worth having, because the affected sets got faster.

Two calls the loop must not make, both found by the existing tests failing:

- **`flush()` is not the drain the plan describes.** `dav1d_flush` is documented
  as "flush all delayed frames in decoder and clear internal decoder state, to be
  used when seeking" — it discards, and calling it between the `Again` and the
  picture destroyed the picture of a valid file. The loop does not flush.
- **`send_pending_data()` cannot be the progress signal.** It answers `Ok` both
  when it handed the last of the input over and when it had none to hand over,
  which is the spin the plan identified. The loop tracks the submission's own
  answer instead, and the two are told apart by `Submission`.

The loop as landed submits the item, then repeats: a picture ends it; an `Again`
with all of the input submitted is the item saying it holds no picture; an
`Again` with input the decoder kept drains that output and resubmits the input;
and a decoder that refuses input while reporting no output to drain has stopped
making progress and is an error rather than a spin. All four are unit tested
against a scripted decoder, which panics if the loop asks for more than the case
describes, so a loop that lost its exit fails instead of hanging.

The fixture is generated, not committed as a binary blob of unknown provenance:
`tests/make-alpha-fixtures.py` writes the container — a file type box, the item
metadata and a media data box — with the item payload reduced to fifteen AV1
temporal delimiter units, so the file states no sequence header and no frame.
The generator is the only thing needed to reproduce it.

Validation, in the order it was run:

- `tests/readalpha.vpy`, against the pre-change build: **328 checks pass and the
  two new ones fail by timeout**, at `prefetch=0` and at `prefetch=4`. Against
  the build after it: **330 checks pass**, with the error reported as
  `failed to decode image '…avif-no-picture.avif': the item holds no picture`.
  The check runs in a child process because the defect it guards is a request
  that never returns, which would stall the runner rather than fail it, and the
  child has to exit on its own so that pool destruction is covered too.
- `cargo test --locked`: **131 passed** (was 123). The eight new ones are the
  four loop shapes, the two real-decoder shapes, the valid item's first-call
  picture and the committed fixture end to end.
- `target/bench/frame-parity.py`, both builds: **120 of 120 data lines byte
  identical**, the only difference being the plugin path it prints.
- `target/bench/ab-sets.py`, three interleaved rounds, best of each:

  | set | decode before → after | ratio |
  | --- | --- | --- |
  | avif 35 | 2284.3 → 1926.9 ms | 0.844 |
  | hitokage 5 | 713.9 → 407.1 ms | 0.570 |
  | mixed 35 | 3377.5 → 3271.3 ms | 0.969 |
  | heic 35 | 6340.1 → 6237.1 ms | 0.984 |
  | png 35 | 388.8 → 397.0 ms | 1.021 |
  | fixtures 99 | 6.3 → 6.4 ms | 1.016 |

  the three sets the change cannot reach are the control: heic and png never
  enter `formats/avif.rs`, and their 1.6% to 2.1% is the drift of the same
  binary run twice. the two sets it can reach both improved, which is the
  low-latency setting paying for itself.
- the `docs/BENCH.md` command on the affected set, `--reps 3 --extra
  --prefetch 16`, best pass of three per configuration:

  | avif 35 | before | after |
  | --- | --- | --- |
  | `Read` | 2.226 s | 2.044 s |
  | `Read`, `prefetch=0` | 5.350 s | 4.939 s |
  | `Read`, `prefetch=16` | 2.387 s | 1.994 s |
  | median frame | 47.91 ms | 13.01 ms |

## deviations from the plan

- **the decoder's settings changed, and the plan did not expect that.** The plan
  asks the loop to decide completion "using decoder progress"; the measurement
  above shows that with dav1d's default frame delay there is no observable
  progress to decide on, because a valid item's first `Again` and a missing
  item's only answer are the same call. Low-latency output is what makes the
  question answerable, and it is also 8% to 17% faster on the avif set, so it is
  recorded as a measured result rather than a cost. The thread count, which is
  [19](19-avif-thread-budget.md)'s subject, is untouched.
- **the "stopped making progress" branch is a guard, not a case any file here
  reaches.** `send_data` returned `Ok` for every item measured, including the
  6.8 MB sandbox page, so the backpressure the plan describes is covered by the
  scripted tests and by the loop rather than by a fixture.
- **the sequence-header-without-a-frame case is cut from a real fixture.** The
  plan asks for it; the first version used the test module's hand-made header,
  which dav1d rejects as malformed and which therefore tested a parse error
  instead. The test now takes `avif-yuv420p.avif`'s payload and keeps its
  sequence header OBU only.

## validation

this is the plan's own list, as written before the change; the measured answers
are in "what it did, measured" above.

- Commit a minimal redistributable no-picture fixture or a deterministic
  generator; the ignored investigation file is evidence, not a permanent test
  dependency. Cover empty/truncated input, a sequence header without a frame,
  valid color and alpha items, and initial backpressure through a suitable
  state-machine test. Check failures with `prefetch=0` and enabled, including
  process shutdown. Run the existing AVIF pixel/alpha checks and the full fixture
  validator.

## left over

- **the empty and truncated payloads are covered by the state machine, not by
  files.** `an_item_that_states_no_frame_ends_with_an_error` drives the real
  decoder with an empty payload, a lone temporal delimiter and a sequence header,
  and the committed fixture is the fourth shape. A truncated *valid* item — a
  frame OBU cut in half — is not among them, and would be the first fixture to
  add if a reader ever produces one.
- **`decode_item` is still called per item, so the low-latency setting is per
  item too.** Reusing one decoder across a sequence would have to re-establish
  that state, and [19](19-avif-thread-budget.md) is where that question belongs.
- **the alpha item is decoded with the same settings**, and its own loop is the
  same function, so the alpha path is covered by construction rather than by a
  separate fixture.
