# 17 — Validate AVIF box boundaries and decode eligibility

- status: implemented
- touches: `src/formats/avif.rs`, `tests/make-alpha-fixtures.py` and
  `tests/fixtures/avif-split-extents.avif` (new), `tests/readalpha.vpy`,
  `AGENTS.md`, `CHANGELOG.md`, `docs/BENCH.md`, `docs/IMPLEMENTATION.md`,
  `docs/HANDOFF.md` and the index row
- depends on: [15](15-avif-decoder-progress.md), which ends the decode this
  hardens, and [16](16-container-orientation.md), whose container walk is the
  same item metadata reader
- expected: a malformed or unsupported avif container is refused with an error
  instead of a panic, a huge allocation or a wrong answer, and a container this
  reader will not decode is not described as the yuv its samples are
- result: every box read is bounded by the header it was read with and by the
  container it is written in, every offset and length is checked before it is
  indexed or allocated, and a width wider than an address ends the walk. A
  container whose primary item this reader cannot locate in one extent it follows
  is described as the format the `image` decoder hands back rather than as the yuv
  its samples are, which is what makes a split item decode instead of failing a
  frame request. The validator passes 431 checks (8 new, one fixture),
  `cargo test` 150 (11 new), and all 120 `frame-parity.py` lines are byte
  identical. Probing costs one file metadata call more per file, which is 1.6 →
  1.8 ms over a 35-file avif clip and nothing per frame.
- risk: low to medium — it changes what an unsupported avif container is
  described as, and it changes how a malformed one fails

## problem and evidence

The custom reader in `src/formats/avif.rs` has several concrete review targets:

- `leading_boxes` reads an extended 16-byte header but calculates the resize
  and payload slice using `header.len()` (eight). This can overwrite the
  extended-size bytes and consume the wrong amount of input.
- `Meta::data_range` checks `base + offset`, then constructs the end with
  unchecked `start + length`. Full decode calls use `usize::MAX` as their
  length limit. `read_range` allocates before verifying that the range fits
  the actual file. Method-1 extents also need bounds against their `idat` box.
- `leading_boxes` stops at the first `mdat`. Unsupported box ordering should
  produce a deliberate fallback or error, rather than an accidental result.
- The probe can describe a native YUV image before `decode` rejects a grid or
  split extents. Comments about falling back to `image` must match the actual
  routing: a successful native probe is not itself a fallback decision.

## what it did, measured

Every bullet was reproduced against the build before the change, and the four
kinds of defect are four different fixes.

**the extended header was eight bytes short.** A box whose size is one carries a
sixty four bit size after its kind, and `leading_boxes` read those eight bytes
and then sized the buffer as if the header were eight bytes long, so the box's
last eight bytes were lost, the box after it started eight bytes early, and every
range the walk handed out stopped being an offset into the file. The header
length is now the length that was read. `an_extended_size_box_is_read_with_its_own_header`
walks a container with one such box and requires the buffer to be the bytes of
the file.

**a range is now checked before it is allocated.** `data_range` built its end
with an unchecked `start + length` and never compared it with anything, and
`read_range` allocated `vec![0; end - start]` before reading a byte of it. A
hand-written 218 byte container whose `iloc` states a length of `0xFFFFFF00`
therefore asked the allocator for 4 GiB and only then failed on the read — the
before build reports `failed to fill whole buffer`, which is the read failing
rather than the allocation. The range is now bounded by the file for a
construction method of zero and by the `idat` box for method one, with checked
arithmetic throughout, and `read_range` compares the range with the file's length
before it allocates. Two more reads of the same walk could not be answered at
all: an `iloc` states the width of its own offset and length fields, and a field
wider than a `usize` was shifted into it one byte at a time, which discards the
top of the value and locates the item somewhere the file does not have; and a
base offset added to an extent offset could leave the address space, which is a
panic in a debug build and a wrapped offset in a release one. An av1 variable
length code with thirty two leading zeros was the same overflow — `1 << 32`,
which panics in a debug build and answers zero in a release one — and that read
comes from the item's own bytes rather than from the container around it.

**the walk's own bounds are stated rather than implied.** `leading_boxes` stops
at the media data box because the buffer it builds is a prefix of the file, which
is what lets a range into that buffer be read from the file as well; a container
that writes its item metadata *after* its media data is one this walker cannot
describe, and it is left to the decoder rather than described from offsets that
mean something else. That is now said where the walk stops and tested.

**the probe no longer promises a frame this reader refuses to produce.**
`image_info` described any container whose primary item carried an `av1C` as the
yuv its samples are, whether or not this reader could locate that item, and
`decode` then refused a grid of tiles, an item in several extents and a
construction method it does not follow. `Meta::native_eligible` is that decision
made once — the primary item has to be one extent of a construction method this
reader follows, inside the file, and the item must not be a grid — and a
container it refuses is described as the format the `image` decoder hands back.
The measured case is `avif-split-extents.avif`, which
`tests/make-alpha-fixtures.py` writes from the coded item `avif-yuv420p.avif`
holds, cut in half and located as two extents:

| build | `Read` creates | frame request |
| --- | --- | --- |
| before | `YUV420P8` 64x48 | fails: "the item is split over several extents, which this reader does not join" |
| after | `RGB24` 64x48 | decodes; the first pixel is 74, 75, 70 |

The two files are the same picture — `avifdec` writes byte identical pngs for
them (`magick compare -metric AE` = 0) — so the fallback decoder really does join
what this reader does not: `mp4parse`, the `image` avif reader, copies an item
spread over several extents into one buffer before it hands the payload to
`dav1d`. The first pixel of the plugin's `RGB24` frame is the one the reference
decoder reads for both files.

**what is measured, and what is not.** A grid is the case the plan names first
and the one this change does *not* repair. `avifenc -g 2x2` writes a grid whose
primary item is associated with `ispe`, `pixi` and `colr` and **not** with
`av1C`, so a probe that needs a coding record already declined it before this
change; and the fallback cannot read it either, because `mp4parse` accepts the
item type `grid` and then hands the eight byte grid descriptor to `dav1d`, which
rejects it — the plugin reports `Format error decoding Avif: Invalid argument` for
such a file, at clip creation. Grid assembly is out of scope for this plan, so
what `native_eligible` adds there is that the refusal is a decision rather than an
accident: a grid whose own properties state an `av1C` is now handed over rather
than described as yuv and then refused. No file in any corpus here is one, and
`avifenc` does not write one.

Validation:

- `cargo test --locked`: **150 passed** (was 139). The eleven new ones are the
  extended header, the box shapes that are not boxes, the media-data-first
  ordering, the wide `iloc` field, the overflowing extent, the item data box
  bounds, the range that is not allocated, the eligibility decision, and the split
  fixture read through both `decoder::probe` and `decoder::decode`.
- `tests/readalpha.vpy`: **431 checks pass** (8 new), against 423 before. The new
  section reads the split fixture and asserts the format the clip is created with,
  the size, and two pixels of the frame, then the yuv file it was cut from, then
  that the two formats together need `mismatch`. Against the before build it fails
  at `get_frame(0)` with the extents error above, which is what makes it a
  regression test rather than a description.
- `target/bench/frame-parity.py`: **120 of 120 data lines byte identical**. The
  split fixture is outside the parity sets, which select `alpha-*` under
  `tests/fixtures/`.
- `ab-sets.py`, 3 rounds, before against after:

  | set | probe | decode |
  | --- | --- | --- |
  | avif 35 | 1.6 → 1.8 ms (1.125) | 1848.1 → 1830.3 ms (0.990) |
  | heic 35 | 6.5 → 6.6 ms (1.015) | 6305.3 → 6146.2 ms (0.975) |
  | mixed 35 | 16.8 → 17.7 ms (1.054) | 3246.9 → 3244.6 ms (0.999) |
  | png 35 | 3.9 → 4.0 ms | 383.2 → 384.0 ms (1.002) |
  | hitokage 5 | 0.5 → 0.5 ms | 389.9 → 392.2 ms (1.006) |
  | fixtures 99 | 1.8 → 1.8 ms | 6.2 → 6.4 ms |

  the probe column is the one file metadata call per file the extent bound needs,
  0.046 → 0.051 ms per avif file, and the decode column is the same binary run
  twice: the widest row moves 2.5% and every other one is inside a percent.
  eight alternating rounds of the avif set alone separate the probe cost from the
  machine: best of eight is 1.6 ms before against 1.8 ms after for the probe, and
  1994.0 ms against 1996.5 ms for the decode, where the per-round paired
  difference swings ±100 ms in both directions with a mean of −18 ms. the decode
  path does gain the `read_range` guard's own metadata calls — two for a colour
  item, three when there is an alpha item beside it — and they are below what this
  machine resolves.

## deviations from the plan

- **a grid is refused by the fallback too, so it is not "handed to a decoder that
  joins it".** The plan's eligibility item assumes the `image` decoder can read
  the layouts this reader cannot. For several extents it can, and that is the fix
  above; for a grid it cannot, and the section above records what that costs. The
  alternative — keeping a grid on the native path so this reader reports it by
  name — was rejected because the probe only ever reaches that path for a grid
  that states its own `av1C`, which is a file no writer here produces, while
  describing a container as yuv is the thing this item exists to stop. The cost
  of that choice is stated plainly: such a file now reaches the `image` decoder's
  error instead of this reader's, which is a worse message for a container
  neither of them can read.
- **a malformed container is a fallback, not an error.** The plan asks malformed
  containers to be reported as useful errors and distinguished from valid but
  unsupported layouts. That distinction cannot be made through the probe's
  `Option`: a container this walker refuses is handed to the decoder, which
  reports its own error for it. What changed is that a range the file does not
  hold is no longer an allocation, and that the errors this reader does report
  name the container they were checked against.
- **no fuzz campaign.** The plan's validation asks for a bounded fuzz or property
  test over the parsing helpers. What landed instead is one test per defect that
  was found by reading the code, plus the shapes around each one, which is what
  the plan's own evidence list describes; a campaign over `read_iloc`,
  `read_ipma` and `read_references` is left over below.

## validation

this is the plan's own list, as written before the change; the measured answers
are in "what it did, measured" above.

- Add synthetic small boxes for normal, extended and zero sizes; partial headers;
  overflowing/out-of-file extents; multiple extents; and `idat` bounds. Test
  unsupported layouts through both probe and decode. Malformed inputs must return
  errors without panic, huge allocation, hangs or process abort. Release uses
  `panic = "abort"`, so checked error paths matter even if debug tests panic.
- Run a bounded fuzz/property-test campaign over pure parsing helpers, with a
  seed corpus of valid containers and recorded regressions. Use subprocess limits
  for native-decoder integration. Close [15](15-avif-decoder-progress.md) before
  running a broad malformed-file integration campaign.

## left over

- **a fuzz or property test over the parsing helpers.** `read_iloc`, `read_ipma`,
  `read_references` and `child_boxes` are pure functions over a byte slice and
  are the natural target; the seed corpus is the fixtures plus the synthetic
  containers the unit tests already build.
- **grid assembly.** A tiled avif is read by neither this reader nor the one it
  falls back to. Joining the tiles needs the `dimg` references, each tile's own
  `av1C` and `ispe`, and a picture of the grid's size; the probe already reads
  the references and the properties, so what is missing is the decode and the
  placement.
- **a malformed container is not reported as malformed.** See the deviation
  above: the probe's `Option` has no room for "this is an avif and it is broken",
  so such a file reaches the `image` decoder's error rather than this reader's.
  A `Result` from `formats::avif::image_info` would carry it, and
  [16](16-container-orientation.md)'s "a container whose item boxes the walker
  cannot read" has the same shape.
- **a construction method of two is refused, not read.** It locates an item in
  another file, which is a data reference this reader does not follow and which
  `mp4parse` does not either; such a file is handed over and fails there.
- **the extended-size path has no fixture.** The unit test writes one, and no
  container in any corpus states a box that way.
