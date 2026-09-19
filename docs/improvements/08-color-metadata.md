# 08 — color metadata

- status: proposed
- touches: `src/decoder.rs`, `src/formats/heif.rs`, `src/formats/png.rs` (new),
  `src/formats/mod.rs`, `src/color.rs`, fixtures, `tests/readalpha.vpy`
- expected: `_Primaries` and `_Transfer` are written for a file whose container
  states them, `_Matrix`/`_Range` follow the file for a yuv frame instead of
  the family default, and a file that carries only an icc profile still changes
  nothing
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
| avif | a `colr` box of type `nclx` among the item properties | the box walk in `formats::heif::avif_header`, already there for `prof`/`rICC`, extended to parse the payload |
| heif/heic | the same box, inside libheif | `libheif_rs`'s `ColorProfile` from a handle — `Nclx{..}` or `Icc(..)`; the module already opens handles for the monochrome path |
| png | a `cICP` chunk: four bytes, the H.273 codes for primaries, transfer, matrix and the full range flag | a new `src/formats/png.rs` chunk walk; `image` has no accessor for the chunk |
| jpeg | nothing — an APP14 marker and an `ICCP` chunk at most | — |
| webp | nothing — `ICCP` only, and vp8 codes no colour description | the current bt.470bg/limited default stands |
| jxl | the codestream's `ColourEncoding` | only if the integration exposes it; check that before promising it, and skip jxl if it does not |

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
_Primaries   ffi::VSColorPrimaries            1, 2, 4..=12, 22
_Transfer    ffi::VSTransferCharacteristics   1, 2, 4..=11, 13..=16, 18
_Matrix      ffi::VSMatrixCoefficients        0, 1, 2, 4..=10, 12..=14
_Range       ffi::VSRange                     from the full range flag
```

and where it is not, the property stays unset: VapourSynth defines no primaries
3 or 18 and above, no transfer 3, 12 or 17, and no matrix 3, 11 or 15 and above.
A file that states one of those is described the way an untagged file is, which
is honest and cheap to keep honest.

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

## validation

- **first print what the current sets state.** The avif fixtures were encoded
  with `avifenc` and no `--cicp`, so they most likely say 2/2/2 — in which case
  every existing set stays exactly as it is, and that is the control this change
  has to pass: no sandbox set and no `tests/readalpha.vpy` clip gains a property
  it did not have. The heic set and the png set are the same check.
- **two fixtures that do state one**, encoded for the purpose:
  `avifenc --cicp 9/18/9 --range full` (bt.2020 primaries, hlg transfer, bt.2020
  non-constant matrix) from an existing png, and a png with a hand-written
  `cICP` chunk — four bytes, then the chunk crc, about fifteen lines of python
  beside the fixture script. Then one frame's property map has to read
  `_Primaries=9`, `_Transfer=18`, `_Matrix=9`, `_Range=1`, and the untagged file
  beside it has to read none of them.
- **the same two files through `ffprobe`.** Its png decoder reads `cICP` and its
  avif decoder reads `nclx`, and it prints the four fields under the names
  `color_primaries`, `transfer_characteristics`, `matrix_coefficients` and
  `color_range`. The numbers must agree with the frame properties; where they
  disagree, one of the two readers is wrong about the box, which is the bug this
  plan exists to find.
- **a unit test for the mapping**, walking 0..=255 and asserting that the codes
  VapourSynth defines map to their own value and every other code maps to
  `None`. That is what keeps rule 1 true as the enum list changes.
- `cargo test --locked`, `cargo fmt`, `cargo clippy --workspace --all-targets
  --locked -- -D warnings`, and one `Read` over each sandbox set to confirm the
  property dumps moved only where a file states something.

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
