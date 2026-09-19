# 10 — nominal bit depth

- status: implemented
- touches: `src/pixel.rs`, `src/formats/avif.rs`, `src/formats/heif.rs`,
  `src/formats/jxl.rs`, fixtures, `tests/readalpha.vpy`, `README.md`,
  `AGENTS.md`, `docs/BENCH.md`, `docs/IMPLEMENTATION.md`, and the
  cross-references to this plan in the index and in plans 06, 11 and 12
- depends on: [12](12-heif-avif-yuv-output.md), which has landed and took the
  avif and heic rows of this plan with it (a yuv page's depth is the format,
  which is why `sandbox/avif` reads as `YUV420P8` and a ten bit crop as
  `YUV444P10` today), leaving jxl, the rgb files that state 9 to 15 bits, and the
  monochrome cases here
- expected: a 10-bit avif is handed out as `Gray10`/`RGB30` instead of
  `Gray16`/`RGB48`, with its samples in the words a 10-bit frame holds, and a
  12-bit file as `Gray12`/`RGB36`
- result: the two deep `hitokage-sample` avifs moved from `RGB48` (`max=65472`
  and `max=65520`) to `RGB30` and `RGB36` (`max=1023` and `max=4095`), whose
  first samples are the old ones `>> 6` and `>> 4` exactly, with identical frame
  bytes; the other eight files of that set, the whole of both
  `frame-parity.py` dumps and every 8- and 16-bit fixture are unchanged, plane
  for plane. three hand-made jxl fixtures state ten, twelve and ten-with-alpha
  bits
  and are handed out as `Gray10`, `Gray12` and `RGB30`+`Gray10`, the last one
  proving an alpha plane of a deeper file moves with its colour plane; the
  `mono-alpha-10.avif` row moved from `Gray16` at scale 256 to `Gray10` at scale
  4 with its samples exact. the paired convert-stage A/B on the ten bit avif is
  112.0 → 112.1 ms/frame, so the shift is free. one deviation is recorded under
  "what it did, measured": jxl keeps asking for sixteen bit words and takes the
  same shift as every other reader rather than being the exception the plan
  describes. native-depth sources and encoded pages now cover monochrome HEIF at
  10 and 12 bits (`mono-10.heic`, `mono-12.heic`) and a 12-bit AVIF with alpha
  (`mono-alpha-12.avif`), with the expected right-aligned samples checked
  end-to-end.
- risk: medium — every 9-to-15-bit file changes the clip's format, and that is
  what a graph branches on

## problem

The format comes from `image`'s colour type, which knows 8 bits, 16 bits and
float and nothing in between, so a file whose nominal depth is 9 to 15 is handed
out as the 16-bit format. `docs/IMPLEMENTATION.md` records that as deliberate
("do not initially attempt to infer or preserve unusual nominal 10/12-bit
representations stored inside `u16`"), and the cost is two things at once:

- **the format is wrong.** A graph that wants `RGB30` or `YUV420P10` gets a
  conversion it did not need, and one that resizes from `RGB48` treats a 10-bit
  picture as a 16-bit one.
- **the samples are not full scale.** `image` hands a ten-bit avif back left
  aligned in sixteen bits — the monochrome row of `tests/readalpha.vpy` used to
  state its planes as "the same numbers times 256", within the rounding of the
  encode — so the largest value such a file can produce is 65280 where `Gray16`
  says the range ends at 65535. That is 0.4%
  short, 0.02% for 12 bits, and nothing in the frame admits it. This applies to
  the paths that go through `image`, which is rgb and the monochrome pages;
  `dav1d`'s own planes are right aligned, which is why the yuv hand-out needs no
  shift and why the row above is already gone for those files.

VapourSynth has the formats. R80 answers `GRAY9`, `GRAY10`, `GRAY12`, `GRAY14`,
`RGB30`, `RGB36`, `RGB42` and `YUV420P10`/`P12`/`P14`, and a 10-bit frame stores
its samples right aligned: `core.std.BlankClip(format=vs.GRAY10, color=1023)`
puts 1023 in the word, not 65472. So this is a format plus a shift, not a new
representation.

## why this is not 08

[08](08-color-metadata.md) writes properties and never touches a pixel or a
format. This one changes both: a frame's format is what the clip is, and the
samples have to move to match it. The only thing the two share is plumbing —
both want one more field out of the container at probe time — which is why they
read like siblings. They are not one change; 08 can land without this and this
without 08.

## change, as planned

- `PixelFormat` gains the variants the readers can actually produce: `Gray10`,
  `Gray12`, `Rgb10`, `Rgb12`, plus `Gray9`/`Gray14`/`Rgb14` if a reader can
  produce those (jxl can). Nothing in `clip::query_format` changes: it already
  passes the bit depth to `core.query_video_format`, so `(RGB, Integer, 10, 0,
  0)` answers `RGB30` by construction. `alpha_format` maps `Rgb10` to `Gray10`
  and so on, so the alpha clip of a 10-bit source is 10-bit as well.
- the probe learns the depth from the container, because the colour type cannot
  carry it: `av1C` already states it for avif (`formats::heif` reads it for the
  monochrome correction), libheif answers it for heic, jxl's image header states
  it, jp2's `prec` states it — [06](06-jpeg-2000-backend.md) would hand it over
  in the same `SIZ` walk it needs anyway. A file whose depth the probe cannot
  learn keeps the 16-bit format it has today, which is the honest fallback.
- the writer shifts. Every source the plugin decodes 10 or 12 bits from hands
  its samples left aligned in 16-bit words, so `pixel::write_planar` and the
  planar path of [03](03-webp-yuv-output.md) shift right by `16 - bits` before
  storing. The shift belongs to the pair (source depth, target depth), so one
  function beside the writer answers it rather than a branch per format. jxl is
  the exception and needs no shift: `JxlDataFormat::U16 { bit_depth }` asks that
  decoder for the depth the frame holds and it scales its f32 pipeline to
  `(1 << bit_depth) - 1`, so a ten bit jxl arrives already right aligned at
  `0..1023` and is copied.
- `_Range` and `_Matrix` say nothing new: a 10-bit frame is full range in the
  same sense an 8-bit one is, and the yuv the plugin hands out stays 8-bit.
- what does not move: `expected_bytes` (two bytes per sample either way), the
  lookahead budget, and every 8- and 16-bit file.

## what it did, measured

The two files in `sandbox/hitokage-sample` that state a depth between the eight
bits and the sixteen a colour type knows moved as planned. `format-summary.py`
with `--frame`, one line per file, before and after (`base10/hitokage-*.txt`):

| file | before | after | frame bytes |
| --- | --- | --- | --- |
| `avif-yuv420p.avif` | `RGB24` max=255 | `RGB24` max=255 | 72,192,000 |
| `avif-yuv422p.avif` | `RGB24` max=255 | `RGB24` max=255 | 72,192,000 |
| `avif-yuv444p.avif` | `RGB24` max=255 | `RGB24` max=255 | 72,192,000 |
| `avif-yuv444p10le.avif` | `RGB48` max=65472 | `RGB30` max=1023 | 144,384,000 |
| `avif-yuv444p12le.avif` | `RGB48` max=65520 | `RGB36` max=4095 | 144,384,000 |
| `jxl-rgb48le.jxl` | `RGB48` max=65535 | `RGB48` max=65535 | 144,384,000 |
| `png-rgb48be.png` | `RGB48` max=65535 | `RGB48` max=65535 | 144,384,000 |
| `tiff-rgb24.tiff` | `RGB24` max=255 | `RGB24` max=255 | 72,192,000 |
| `tiff-rgb48le.tiff` | `RGB48` max=65535 | `RGB48` max=65535 | 144,384,000 |
| `exr-gbrpf32le.exr` | `RGBS` max=255 | `RGBS` max=255 | 288,000,000 |

the first samples are the old ones moved down and nothing else: 18304, 18688,
19072, 19008, 18560, 18880 became 286, 292, 298, 297, 290, 295 (`>> 6`, ten bit)
and 18320, 18720, 19248, 19120, 18576, 18688 became 1145, 1170, 1203, 1195,
1161, 1168 (`>> 4`, twelve bit), which is the shift being exactly the difference
between the two depths and not a rounding. the frame bytes are identical because
a ten bit frame is two bytes per sample like the sixteen bit word it replaced, so
`expected_bytes`, the lookahead budget and the number of frames a budget holds
are all unchanged.

the three hand-made jxl fixtures, which are the files that state their own depth
in a codestream rather than in a container box:

| fixture | states | before | after |
| --- | --- | --- | --- |
| `jxl-gray10.jxl` | 10 bit gray | `Gray16` holding `1..12 << 6` | `Gray10` holding `1..12` |
| `jxl-gray12.jxl` | 12 bit gray | `Gray16` holding `1..12 << 4` | `Gray12` holding `1..12` |
| `jxl-rgba10.jxl` | 10 bit rgb + alpha | `RGB48`/`Gray16` | `RGB30`/`Gray10` holding `1..12`, `13..24`, `25..36`, `100..111` |

the two grayscale ones hold no alpha channel, so their alpha clip is also the
check that an opaque plane is filled with the largest sample of the frame's own
depth: 1023 for the ten bit one and 4095 for the twelve bit one, where both used
to be 65535 in a `Gray16` plane. that and the `mono-alpha-10.avif` row are what
`tests/readalpha.vpy` now checks, at 322 checks, alongside the ten bit avif's
planes being stated exactly (`round(value * 1023 / 255)`, the number `avifenc`
stored) rather than as "times 256, within the rounding".

pixels: 105 of the 106 lines of `frame-parity.py`'s nine sets are byte identical
to the baseline's, the odd line out being the plugin name it prints
(`base10/parity-*.txt`). same-batch interleaved A/B against
`vs_imageseqs-before-10.dll`, best of five rounds: `avif` 3149.2 → 3097.3 ms
(0.984), `heic` 7346.6 → 7258.9 (0.988), `hitokage` 1294.7 → 1345.4 (1.039),
`mixed` 4077.0 → 4077.3 (1.000), `png` 581.0 → 590.7 (1.017), with a second run
moving the same numbers by up to 4% in both directions, so the whole decode pass
is flat. the convert stage on its own, paired and interleaved four rounds of
eight frames of one 6000x4000 file (`ab-convert.py`): the ten bit avif 112.0 →
112.1 ms, the twelve bit one 105.0 → 104.1, and the two files the shift does not
touch — sixteen bit jxl 85.8 → 85.7 and sixteen bit png 87.7 → 88.5 — as controls.
one move per sample more per sample moved, and no measurable difference: the
load and the store were already there.

validation: `cargo test --locked` 120 passed (four new: the depth table, the
narrowing, the extraction at a narrower depth, and the exactness of the shift for
every pair of depths from one to sixteen bits), `cargo fmt`, `cargo clippy
--workspace --all-targets --locked -- -D warnings` clean, `cargo build --release
--locked`, `tests/readalpha.vpy` passes 278 checks, and `python -m build` still
produces the same `py3-none-win_amd64` wheel with the plugin under
`vapoursynth/plugins/` and no python package.

### deviations from the plan

- **jxl is not the exception.** the plan has jxl ask
  `JxlDataFormat::U16 { bit_depth }` for the file's own depth, so that its
  samples arrive right aligned at `0..(1 << bits) - 1` and are copied, and every
  other reader shifts. the implementation does the opposite: jxl keeps asking
  for `bit_depth: 16`, exactly as it did before this plan, and the writer moves
  its samples down by the same difference as everywhere else. the reason is that
  the shift is *provably* exact rather than approximate — scaling a sample of
  `bits` bits onto a word of `word` bits multiplies it by less than
  `2^(word - bits) + 1`, so the largest scaled sample stays below the smallest
  one of the value after it and dividing back down returns the sample it started
  as (a unit test states that for every pair of depths from one to sixteen bits,
  and the fixtures confirm it for ten and twelve) — while the plan's version
  needs the writer to know which alignment it is being handed: either a new
  per-module field through `Pixels` and `DecodedImage` or a second writer, for a
  result that is the same bytes. `formats/jxl::output_format` carries the reason
  where the request is built. the fixture that proves the depth is read is
  `jxl-gray10.jxl`; with `bit_depth: 16` it is the writer's shift that has to use
  the depth the codestream states, so the fixture tests the same thing either
  way.
- **the hand-made sources live in the fixture script, not the bench directory.**
  the plan suggests `target/bench/make-10bit-pgm.py`, which is inside the
  git-ignored `target/`: the pgm and pam writers are in
  `tests/make-alpha-fixtures.py` instead, beside the png writers, so the sources
  of the hand-made jxl fixtures are tracked and a changed source is re-encoded
  from a script that ships with the fixtures, which is how the other hand-made
  containers work.
- **a fourth fixture, and the whole depth range.** the plan asks for a ten and a
  twelve bit gray pgm. the implementation adds the four channel one as well
  (`jxl-rgba10.pam`, a `P7` `PAM` with `MAXVAL 1023`), because an alpha plane
  that has to move is the case the plan calls the likeliest bug and a fixture
  without an alpha channel can only check the opaque fill, not the move. the
  variants for nine, eleven, thirteen, fourteen and fifteen bits exist too: they
  cost four table entries each and VapourSynth names every depth from eight to
  sixteen in both families, which the plan's list of `Gray9`/`Gray14`/`Rgb14`
  only guessed at.
- **a colour heif that keeps the `image` path keeps sixteen bit samples.** a
  seven or ten bit heic whose matrix the frame properties cannot name is left to
  the registered libheif hook by `HeifHeader::format`, which answers no format
  for it; that hook reports a sixteen bit colour type, and the module could
  apply `.at_depth` in an `output_format` of its own (as avif does for its
  monochrome items) if the alignment of libheif's own r,g,b conversion for a
  deep page were known. nothing here could verify it, so the depth is not
  claimed for that path; [05](05-monochrome-heif.md)'s monochrome heif, which
  libheif hands over as planes, does claim it.

## validation

this is the plan's own list, as written before the change; the measured answers
are in "what it did, measured" above.

- **the existing fixture is the test.** `mono-alpha-10.avif` moves from
  `Gray16` at scale 256 to `Gray10` at scale 4 — `avifenc` scales the eight bit
  source onto the ten bit range, and that number, `round(value * 1023 / 255)`, is
  what a `Gray10` frame holds, so the row states it exactly rather than allowing
  256 either way. Its row in the monochrome table changes those two numbers, and
  nothing else in `tests/readalpha.vpy` is allowed to move.
- **`sandbox/hitokage-sample` is the set for this**: one 6000x4000 picture
  through `avif` at 8, 10 and 12 bits (4:2:0, 4:2:2 and 4:4:4), `jxl` and `png`
  at 16 bits, `tiff` at 8 and 16, and an `exr`. The samples agree with each
  other, so they are also the cross-format check. Today the plugin answers
  `RGB24` for the three 8-bit avif files, `RGB48` with `max=65472` for the ten bit
  one and `max=65520` for the twelve bit one, and `RGB48` with `max=65535` for
  `jxl`, `png` and `tiff` — the last three are full scale and must not move.
- **a 10-bit jxl fixture**, so the shift is proven to be `16 - bits` and not a
  hard-coded 6, and so the jxl row of this plan has a file that states its own
  depth. It does not need `cjxl --override_bitdepth`, which also exists and also
  works: a `P5` `PGM` whose header says `MAXVAL 1023` is read as a native ten bit
  image, so a 4x3 grid of `1..12` written by hand is 25 bytes of jxl under
  `cjxl -d 0`, and
  `jxlinfo` prints `10-bit Grayscale` for it. `djxl` round-trips it at `maxval`
  1023 with the same twelve values, and the plugin before this plan handed that
  file out as
  `Gray16` holding `[64, 128, 192, 256, 320, 384, 448, 512, 577, 641, 705,
  769]` — the ten bit range stretched onto sixteen, which is the bug in one line.
  A 12-bit counterpart is the same pgm with `MAXVAL 4095`. (the sources ended up
  in `tests/make-alpha-fixtures.py` rather than in `target/bench`, which is one of
  the deviations below.)
- **a 16-bit png and the 16-bit alpha fixtures stay `RGB48`/`Gray16`**, and so do
  jpeg, webp and dds. That is the check that the fallback did not swallow a
  depth it cannot read, and that a 16-bit sample is still full scale.
- **`ReadAlpha` on a 10-bit source** has to produce a `Gray10` alpha plane, and
  the opaque fill for a source without an alpha channel has to be 1023 rather
  than 65535. That is the likeliest bug this plan ships, so it gets its own
  check instead of riding on the colour plane's.
- **native-depth monochrome fixtures** cover the libheif path at 10 and 12 bits
  with `mono-10.heic` and `mono-12.heic`, including their opaque alpha fills, and
  the monochrome AVIF path at 12 bits with `mono-alpha-12.avif`, including its
  alpha plane. Their 16-bit PNG sources are generated by
  `tests/make-alpha-fixtures.py`; the gray channel is depth-aligned and AVIF's
  normalized alpha channel is full-range scaled before encoding.
- **`target/bench/frame-parity.py`** over every sandbox set: every dump must be
  byte identical except the 10-bit fixture's. (measured: its nine sets are the
  six sandbox ones, a fixed png subset and the alpha fixtures, and none of them
  holds a file whose depth changed, so the two dumps came out identical in full —
  105 of the 106 lines, the odd one being the plugin name it prints. the deep
  pages of `hitokage-sample` are outside them and were checked through
  `format-summary.py --frame` instead, which is the table above.)
- `cargo test --locked`, `cargo fmt`, `cargo clippy --workspace --all-targets
  --locked -- -D warnings`.

## left over

- **the other depths have a variant, a table entry and no fixture file.** the
  mapping covers every depth VapourSynth names in both families, eight to
  sixteen, and `tests/readalpha.vpy` checks ten and twelve bits end to end;
  nine, eleven, thirteen, fourteen and fifteen are checked by the unit tests
  (the narrowing table, and the shift's exactness for every pair of depths) and
  by nothing that a real file states. a `P5` `PGM` or a `P7` `PAM` with another
  `MAXVAL` and one line in `tests/make-alpha-fixtures.py` would make another
  fixture, which is what to do if a reader ever produces one.
- **`YUV420P10` is no longer a format without a source.** [12](12-heif-avif-yuv-output.md)
  hands out a yuv page's own planes, so the ten and twelve bit yuv formats are
  produced there, by the format and with no shift; this plan keeps the rgb and
  gray rows. the same is true of `Yuv420P14` and its siblings, which nothing
  states either: libheif can report fourteen bits for a page whose samples it
  decodes itself, and `yuv_format` has no arm for it, so such a file is left to
  the rgb path rather than handed out at a depth no reader in these tests has
  produced.
- **a 16-bit file whose decoder does not fill the range** is a different
  problem: the format claims the full 16 bits and the decoder hands what the
  file holds. Every other reader makes the same claim, so this plan does not
  touch it.
- **native-depth colour HEIF remains outside this fixture pass.** the 10- and
  12-bit HEIF files added here are monochrome, which exercises the libheif plane
  path without introducing an unsupported 12-bit YUV colour case. a colour
  HEIF fixture can be added when the corresponding YUV format is supported.
