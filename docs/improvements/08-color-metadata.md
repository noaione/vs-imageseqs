# 08 — color metadata

- status: implemented
- touches: `src/decoder.rs`, `src/formats/heif.rs`, `src/formats/png.rs` (new),
  `src/formats/mod.rs`, `src/formats/jxl.rs`, `src/color.rs`, fixtures,
  `tests/make-cicp-fixtures.py` (new), `tests/readalpha.vpy`
- expected: `_Primaries` and `_Transfer` are written for a file whose container
  states them, `_Matrix`/`_Range` follow the file for a yuv frame instead of
  the family default, and a file that carries only an icc profile still changes
  nothing
- result: 9 of the 33 files in `tests/fixtures`, all 35 heic pages, 1 of the 35
  avif pages, all 35 jxl pages and 12 of the 35 mixed pages gained
  `_Primaries`/`_Transfer`; the 35 jxl pages also lost the `ImgSeqHasICC`
  [11](11-jxl-direct.md) was claiming for them. No file anywhere moved a pixel,
  a format, a size, a `_Matrix` or a `_Range`: the only properties that changed
  are the two this plan writes and that stale `ImgSeqHasICC`. The probe pays
  0.15 ms per
  heic file and 0.04 ms per png file for it — 5.1 ms and 1.4 ms on a 35 page
  set against the 7.0 s and 0.5 s those pages take to decode — and the two fresh
  fixtures state 9/18 through a `cICP` chunk and through an `nclx` box with
  `ffprobe` reading both the same way
- risk: low to medium — a wrong colour claim is worse than an absent one, so
  every rule below ends in "unset"

## problem

`# Color Metadata` in `docs/IMPLEMENTATION.md` asks for `_Primaries`,
`_Transfer` and `_ChromaLocation` in the property list and then narrows it: "set
primaries and transfer characteristics when reliable metadata is available", and
"Do not invent sRGB/BT.709 metadata when the file only supplies an ICC profile
that has not been interpreted". `# Probing` names "CICP/color information where
available" among the fields a probe should read.

`src/color.rs` writes `_Matrix`, `_Range` and `_FieldBased=0`, and `ImageInfo`
carries no colour description beyond `has_icc_profile`. So a bt.2020/pq avif, or
a display-p3 png that states its primaries, is handed out exactly like an
untagged file: the graph downstream has to guess, and the first thing it guesses
wrong is the conversion it was going to do anyway. The `_Matrix`/`_Range` pair
already follows the samples where the decoder forces them (yuv from libwebp),
which is the same class of problem one step earlier: the plugin knows the codes
and does not pass them on.

Two neighbours look like this plan and are not.
[09](09-exif-orientation.md) moves samples rather than describing them, and
[10](10-nominal-bit-depth.md) changes the frame's format and the alignment of
the samples in it. All three want one more field out of the container at probe
time, and that is the whole of the resemblance: 08 changes properties only, so
it is the one that cannot make a frame wrong.

## where the codes are

| container | what states them | how to read it |
| --- | --- | --- |
| avif | a `colr` box of type `nclx` among the item properties | the box walk `formats::heif` already had for `prof`/`rICC`, extended to parse the payload — it is `formats::avif::image_info` now, by [12](12-heif-avif-yuv-output.md). **34 of the 35 files in `sandbox/avif` have no `colr` box at all** — `av1C`'s copy of the sequence header carries the depth but not the CICP — so the useful value for that set lives in the AV1 sequence header's `color_config` at the head of `mdat`: `avifdec --info` reports primaries 1, transfer 13, matrix 6, full range for those pages. Reading it is a bit read of one OBU, which is what that module does now |
| heif/heic | the same box, inside libheif | `libheif_rs`'s `ColorProfile` from a handle — `Nclx{..}` or `Icc(..)`; the module already opens handles for the monochrome path |
| png | a `cICP` chunk: four bytes, the H.273 codes for primaries, transfer, matrix and the full range flag | a new `src/formats/png.rs` chunk walk; `image` has no accessor for the chunk |
| jpeg | nothing — an APP14 marker and an `ICCP` chunk at most | — |
| webp | nothing — `ICCP` only, and vp8 codes no colour description | the current bt.470bg/limited default stands |
| jxl | the codestream's `ColourEncoding` | it exists: `embedded_color_profile()` answers `Simple(JxlColorEncoding { primaries, transfer_function, white_point, rendering_intent })` or `Icc(..)`. The `image` adapter that used to be in the way exposed only the *converted* output profile; [11](11-jxl-direct.md) dropped it, so `src/formats/jxl.rs` already opens the header this needs |

## change, as planned

`ImageInfo` gains `cicp: Option<Cicp>`, with
`Cicp { primaries: u8, transfer: u8, matrix: u8, full_range: bool }` holding the
H.273 code points as the file stores them. The probe fills it from the walk the
format correction already does — the avif boxes, the libheif handle, the png
chunks — so this is not a second pass over the file. `src/color.rs` maps it to
the frame.

**the mapping.** VapourSynth's enums are the H.273 code points, so the mapping is
the identity where it is defined:

```text
_Primaries   ffi::VSColorPrimaries            1, 4..=12, 22
_Transfer    ffi::VSTransferCharacteristics   1, 4..=11, 13..=16, 18
_Matrix      ffi::VSMatrixCoefficients        0, 1, 4..=10, 12..=14
_Range       ffi::VSRange                     from the full range flag
```

and where it is not, the property stays unset: VapourSynth defines no primaries
3 or 18 and above, no transfer 3, 12 or 17, and no matrix 3, 11 or 15 and above.
A file that states one of those is described the way an untagged file is, which
is honest and cheap to keep honest. Code 2 is missing from all three rows for
the reason rule 1 gives: VapourSynth names it, but it says "unspecified" rather
than describing anything, so it leaves the property unset like a file with no
box at all. A unit test walks 0..=255 against each of these rows, which is what
keeps them true as the enums change.

**the rules.**

1. the file states it or the property is not written. Code 2 (`unspecified`) is
   a statement but not a description, so it leaves the property unset exactly
   like a file with no `nclx` box at all.
2. `nclx` beats the icc profile when a file carries both: the profile describes
   an output space, `nclx` describes the samples.
3. an icc profile on its own changes nothing. `ImgSeqHasICC` stays the whole
   story about it, which is the doc's own rule.
4. `_Matrix` and `_Range` follow the file only for yuv frames. A lossy webp or a
   `sYCC` jp2 whose container states a matrix and a range uses them; otherwise
   the family default stands, which is `identity`/`full` for rgb and
   `bt470bg`/`limited` for the yuv the webp path hands out. An rgb frame keeps
   full range whatever the flag says, because the flag describes the coded yuv
   the file may not even have.
5. `_Primaries` and `_Transfer` are written for every family, not only yuv: a
   png that states sRGB primaries is describing the rgb it hands out, and that
   is what a graph converting from it needs.

## result

**what the sets state.** The prediction above that the existing files would most
likely say 2/2/2 was wrong for two formats out of five and right for the rest:

| set | files that gained a property | what they state |
| --- | --- | --- |
| `tests/fixtures` | 9 of 33 | the avif, heic and jxl fixtures state bt.709/sRGB, the two `cicp-rgb8` ones bt.2020/hlg |
| `sandbox/png` | 0 of 35 | no `cICP` chunk anywhere in the set |
| `sandbox/heic` | 35 of 35 | every page states bt.709 primaries with the sRGB transfer |
| `sandbox/avif` | 1 of 35 | `snek - p003.avif` carries a `colr` box, and the other 34 state nothing, so the av1 sequence header stays the only place a colour sits for them |
| `sandbox/jxl` | 35 of 35 | the sRGB transfer on every page, sRGB primaries on 4 of them |
| `sandbox/webp`, `sandbox/jpeg` | 0 of 35 | vp8 and jpeg state no code, so the family default stands |
| `sandbox/mixed` | 12 of 35 | its heic and jxl pages, and nothing else |

so the control the plan asked for held for png, webp and jpeg, and the files that
did gain a property are the ones a real encoder wrote a colour for. Nothing else
moved with them: the property diff over all seven sets and the fixtures shows
`_Primaries`, `_Transfer` and the `ImgSeqHasICC` correction [11](11-jxl-direct.md)
left behind as the only changes, and a per-frame hash of every plane of every set
(`frame-parity.py`) is identical between the two builds line for line — same
formats, same sizes, same samples, `_Matrix` and `_Range` included.

**the two fixtures.** `tests/make-cicp-fixtures.py` writes `cicp-rgb8.png` with a
hand-built `cICP` chunk, and `avifenc --lossless --cicp 9/18/0 -r full` encodes
`cicp-rgb8.avif` from it. Both hold the same picture as `alpha-rgb8.png` sample
for sample, and `ffprobe` reads them as:

| file | ffprobe | frame |
| --- | --- | --- |
| `cicp-rgb8.png` | bt2020 / arib-std-b67 / gbr / pc | `_Primaries=9 _Transfer=18 _Matrix=0 _Range=1` |
| `alpha-rgb8.png` | unknown / unknown / gbr / pc | no `_Primaries`, no `_Transfer`, `_Matrix=0 _Range=1` |
| `cicp-rgb8.avif` | bt2020 / arib-std-b67 / bt2020nc / pc | `_Primaries=9 _Transfer=18 _Matrix=0 _Range=1` |
| `alpha-rgba8.heic` | bt709 / iec61966-2-1 / smpte170m / pc | `_Primaries=1 _Transfer=13 _Matrix=0 _Range=1` |

the two readers agree on primaries, transfer and range everywhere and differ on
the matrix in the two files whose container describes the yuv it codes: `ffprobe`
reports the file's `nclx` matrix — 9 for the avif, 6 for the heic — where the
frame reports 0, because rule 4 hands those files out as rgb and an rgb frame's
matrix is the identity. The `_Matrix=9` the plan expected for the avif frame was
written before rule 4 was applied to that case; what a file's own matrix and
range describe is what [12](12-heif-avif-yuv-output.md) will write when it hands
those pages out as the yuv they are. `ffprobe` also names its four fields
`color_primaries`, `color_transfer`, `color_space` and `color_range` rather than
the ffmpeg argument names, and it reports the unchanged fixtures the same way
this build does.

**the png chunk.** The payload is four one-byte fields — primaries, transfer,
matrix, then the video full range flag — and two details of it decide what a
reader does with the rest. The matrix "shall be" 0, because rgb is the only
colour model a png has: the first fixture written with a matrix of 9 was refused
outright by the `png` crate inside `image` and never decoded at all. The flag is
a whole byte that conforming files write as 0 or 1 — `09 12 00 01` is the
specification's own example — and not the top bit of a bit field the way an
`nclx` box stores it, so `src/formats/png.rs` reads a nonzero byte as the full
range and a unit test pins both the 0/1 pair and the `0x80` a writer carrying the
other convention over would set. Neither detail can be seen in a frame
property — a png always hands out rgb, whose range is full whatever the flag says
— which is why the unit test is the only thing holding them.

**the rest.** `cargo test --locked` (96 tests), `cargo fmt`, `cargo clippy
--workspace --all-targets --locked -- -D warnings` and `tests/readalpha.vpy` all
pass, and the validator's colour section asserts the codes above plus the three
cases the rules are about: an rgb frame keeping matrix 0 and full range beside a
heic that states smpte 170m, a jxl that states the transfer function but not the
primaries writing one property and not the other, and a lossy webp keeping
bt470bg and the limited range.

**the cost.** Both new reads happen once per file at probe time, so the probe is
the only thing that can pay for them. `target/bench/probe-cost-08.py` times clip
creation over 35 pages per format, five rounds each, the baseline and this build
alternating inside one batch:

| set | probe, before | probe, after | decode of the same 35 pages |
| --- | --- | --- | --- |
| `sandbox/png` | 1.8 ms | 3.2 ms | 506 ms |
| `sandbox/heic` | 4.7 ms | 10.1 ms | 7017 ms |
| `sandbox/jxl` | 2.1 ms | 1.8 ms | 5749 ms |

which is 0.04 ms per png file for the chunk walk, 0.15 ms per heic file for the
second handle open, and nothing measurable for jxl, whose codes come out of the
header [11](11-jxl-direct.md) already reads. As a share of reading the same pages
it is 0.3% and 0.08%. The per-frame stages are the control (`stage-split.py`,
eight pages each way, four rounds, `prefetch=0`): png 61.50 → 61.88 ms/frame and
heic 682.75 → 683.11 ms/frame at each build's best, both inside the spread of the
same binary run twice, and the `open` stage of the heic run measured *lower* with
this build, which is the page cache the probe warmed rather than anything the
decode path does. Nothing here is a trade a frame-rate plan would recognise.

## left over

- **`_ChromaLocation` is not set.** Among the containers read here, only av1's
  `chroma_sample_position` states it, it sits two bits deep inside the sequence
  header behind `color_config()`, and files that store 4:2:0 usually leave it
  `unknown` anyway. A guess there is exactly the mistake rule 3 forbids, so the
  property stays unset and a graph that needs it passes the location into its
  own resize. Worth revisiting when a real file states one.
- **the icc bytes are still read and dropped.** Preserving them needs a custom
  property that nothing in this workspace reads, and turning an uninterpreted
  profile into primaries is forbidden by rule 3. `docs/IMPLEMENTATION.md` defers
  full icc management; this plan does not move that line.
- **png's `cHRM`, `gAMA` and `sRGB` chunks** state the same thing less directly.
  `cICP` is the one that carries the H.273 codes the properties use, so the
  others stay unread rather than converted, which keeps one mapping table in the
  code instead of three.
