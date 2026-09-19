# 10 — nominal bit depth

- status: proposed
- touches: `src/pixel.rs`, `src/decoder.rs` (the probe), `src/formats/heif.rs`,
  `src/formats/jxl.rs`, `src/clip.rs`, `src/source.rs`, fixtures,
  `tests/readalpha.vpy`, `README.md`
- expected: a 10-bit avif is handed out as `Gray10`/`RGB30` instead of
  `Gray16`/`RGB48`, with its samples in the words a 10-bit frame holds, and a
  12-bit file as `Gray12`/`RGB36`
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
- **the samples are not full scale.** dav1d hands avif's ten-bit samples back
  left aligned in sixteen bits — `tests/readalpha.vpy` says it in the monochrome
  table, "the same numbers times 256" — so the largest value a 10-bit file can
  produce is 65280 where `Gray16` says the range ends at 65535. That is 0.4%
  short, 0.02% for 12 bits, and nothing in the frame admits it.

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

## validation

- **the existing fixture is the test.** `mono-alpha-10.avif` moves from
  `Gray16` at scale 256 to `Gray10` at scale 4 — the file stores each 8-bit
  source sample four times over, and right aligned in ten bits that is what a
  `Gray10` frame holds. Its row in the monochrome table changes those two
  numbers, and nothing else in `tests/readalpha.vpy` is allowed to move.
- **a 10-bit jxl fixture**, so the shift is proven to be `16 - bits` and not a
  hard-coded 6, and so the jxl row of this plan has a file that states its own
  depth. It does not need `cjxl --override_bitdepth`, which also exists and also
  works: a `P5` `PGM` whose header says `MAXVAL 1023` is read as a native ten bit
  image, so a 4x3 grid of `1..12` written by hand (or by
  `target/bench/make-10bit-pgm.py`) is 25 bytes of jxl under `cjxl -d 0`, and
  `jxlinfo` prints `10-bit Grayscale` for it. `djxl` round-trips it at `maxval`
  1023 with the same twelve values, and the plugin today hands that file out as
  `Gray16` holding `[64, 128, 192, 256, 320, 384, 448, 512, 577, 641, 705,
  769]` — the ten bit range stretched onto sixteen, which is the bug in one line.
  A 12-bit counterpart is the same pgm with `MAXVAL 4095`.
- **a 16-bit png and the 16-bit alpha fixtures stay `RGB48`/`Gray16`**, and so do
  jpeg, webp and dds. That is the check that the fallback did not swallow a
  depth it cannot read, and that a 16-bit sample is still full scale.
- **`ReadAlpha` on a 10-bit source** has to produce a `Gray10` alpha plane, and
  the opaque fill for a source without an alpha channel has to be 1023 rather
  than 65535. That is the likeliest bug this plan ships, so it gets its own
  check instead of riding on the colour plane's.
- **`target/bench/frame-parity.py`** over every sandbox set: every dump must be
  byte identical except the 10-bit fixture's.
- `cargo test --locked`, `cargo fmt`, `cargo clippy --workspace --all-targets
  --locked -- -D warnings`.

## left over

- **9 and 14 bit** come along with the same readers (avif states 8, 10 or 12;
  jxl can state 9 through 16) but no fixture here states either, so they get a
  variant and no test until a real file shows up.
- **`YUV420P10` is not produced by anything.** The only yuv the plugin hands out
  is libwebp's 8-bit, and a 10-bit avif is still converted to rgb by the `image`
  hook before the plugin sees it. Format variant, no source; it stays unbuilt
  until [08](08-color-metadata.md) or a new decoder makes a 10-bit yuv clip
  possible.
- **a 16-bit file whose decoder does not fill the range** is a different
  problem: the format claims the full 16 bits and the decoder hands what the
  file holds. Every other reader makes the same claim, so this plan does not
  touch it.
