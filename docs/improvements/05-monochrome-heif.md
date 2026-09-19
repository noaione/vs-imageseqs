# 05 — monochrome heif and heic fail to decode

- status: implemented in `src/formats/heif.rs`; both leftovers below are closed
- touches: `src/formats/heif.rs` (new), `src/formats/mod.rs` (new), `src/decoder.rs`, `src/lib.rs`, `src/pixel.rs`
- result: 35 of 35 `sandbox/heic` files decode, the 31 monochrome ones as `Gray8`,
  and so do the monochrome avif pages of `sandbox/avif` and of `tests/fixtures`;
  an avif is also probed without being decoded, 6.43 s of clip creation for the
  35 page set down to 0.002 s
- risk: low, it is a decode path that currently always fails

## problem

`sandbox/heic` holds 35 files. four decode, 31 fail with:

```text
failed to decode image 'snek - p003.heic': Format error decoding `heif`:
Image is not interleaved.
```

the correlation is exact, from `target/bench/heic-survey.py` (reads the `ftyp`
brands and the `hvcC` record, then decodes every file):

| brand | HEVC profile | chroma | files | decode |
| --- | --- | --- | --- | --- |
| `heic` | 3 (Main Still Picture) | 1 (4:2:0) | 4 | 4 of 4 |
| `heix` | 4 (Format Range) | 0 (4:0:0) | 31 | 0 of 31 |

the failing files are monochrome, which is also what `mediainfo` reports
(`Color space: Y`, against `YUV / Chroma subsampling 4:2:0` for the files that
work). `chroma_format_idc = 0` is the same fact from the bitstream.

## cause

the message is not from libheif. it is `libheif-rs`'s own check in the `image`
integration that this crate registers from `src/decoder.rs`:

```rust
// libheif-rs-3.0.0/src/integration/image.rs:227
let img = LibHeif::new().decode(&self.image_handle, color_space, None)?;
if !matches!(img.color_space(), Some(c) if c == color_space) {
    return Err(image_error("Color space mismatch."));
}
let planes = img.planes();
let Some(plane) = planes.interleaved else {
    return Err(image_error("Image is not interleaved."));
};
```

for a monochrome image `get_color_type` returns `ColorType::L8`/`L16`, so the
hook asks libheif for `ColorSpace::Monochrome`. that decodes successfully, but
monochrome is a single planar plane, so `planes.interleaved` is `None` and the
hook rejects it. the decode itself worked: the error is raised *after*
`decode(...)?` returned, and the colour space check passed, so neither libheif
nor libde265 is at fault. libde265 handles these profile 4 streams fine.

in other words, the `image` hook cannot decode monochrome heif at all, which is
why the failure tracks chroma 4:0:0 and not the container brand.

## change

decode heif and heic through `libheif-rs` directly, for the cases the `image`
hook cannot represent. the crate is already a direct dependency (`libheif-rs =
3.0.0` with the `image` and `v1_23` features), so this adds no native dependency
and no licence change.

the path lives in `src/formats/heif.rs` rather than in `src/decoder.rs`, because
decoding one container is not the job of the shared decoder; `decoder::decode`
asks `formats::heif::handles(info)` first, and the module takes over when the
file has a heif extension **and** the probe reported a monochrome color type
(`L8`, `La8`, `L16`, `La16`) — which is exactly the case the hook cannot
represent. everything else, colour heif and avif included, keeps the hook and
its colour conversion.

what the module does:

- `HeifContext::read_from_file(path)`, then `primary_image_handle()`, then the
  same dimension check against the probe that the `image` path performs, so a
  file that changed on disk is still reported.
- decode with `ColorSpace::Monochrome` and copy the planes row by row, since
  libheif pads every row to the plane stride; `La8` and `La16` interleave the
  luma and alpha planes into pixel pairs, and 16-bit samples are copied byte for
  byte because both sides are native endian.
- the buffer is packed into the same interleaved layout the rest of the pipeline
  already accepts, so `write_planar` and the probe need no change at all: the
  probe already reports `ColorType::L8` for these files, and `L8`/`La8` already
  map to `PixelFormat::Gray8`.
- an alpha plane is only packed when the probe reported alpha. if such a file
  turns out to have no alpha plane, the decode fails with a specific error
  instead of leaving a half filled buffer for the frame writer to misread.

## also worth measuring while here

`sandbox/avif` shows a second cost in the same area: creating the clip probes
35 files in 6.10 s (174 ms each) and the decode then reports another ~150 ms of
`open` per frame, which is the container being parsed a second time. the same
question applies to heif. probing once and reusing that work, or making the
probe read only the header boxes, would remove seconds from every avif and heif
chapter.

that guess at the cause was wrong — the ~150 ms of `open` is the decoder's
constructor, which decodes the picture and its alpha item, and the container
parse is half a millisecond of it — but the conclusion held, see *probing an avif
without decoding it*. it is worth the whole paragraph here anyway, because it is
the measurement this plan went on to act on.

## result

`target/bench/heic-validate.py` opens `sandbox/heic` as one clip with
`mismatch=True`, decodes all 35 frames, and compares a sparse mean of every
monochrome page against the same page re-saved as png:

- 35 of 35 decode: 31 as `Gray8` 3672x5274, three as `RGB24` 3672x5274 and one
  as `RGB24` 3312x4717, with no failures.
- the monochrome planes match the png mean exactly, 0.0 difference on every
  page, so the rows are not shifted and the padding is not off by a byte.
- the set reads in 7.50 s at the default prefetch (206 ms median frame) against
  24.11 s with `prefetch=0`, so the lookahead is worth 3.21x here, and
  `prefetch=16` does not improve on the default (7.92 s). the per frame table is
  in `target/bench/sandbox-heic-fixed.txt`, and the set is now one of the rows
  in [BENCH.md](../BENCH.md).
- the four files that already decoded still go through the hook, untouched.

## monochrome heif with alpha

closed by a fixture rather than by a change, because the packing path already
worked: `tests/fixtures/mono-alpha.heic` is encoded from the seven by five
`mono-alpha.png` that `tests/make-alpha-fixtures.py` writes, and `ReadAlpha`
returns it as a `Gray8` colour clip and a `Gray8` alpha clip with every sample
equal to the source formula
(`8 + 11x + 23y` grey, `240 - 9x - 13y` alpha, so a shifted row or a byte of
`La8` interleaving shows up instead of cancelling out).
`tests/readalpha.vpy` now asserts both planes, and the colour files that share
the container (`tests/fixtures/alpha-rgba8.heic`, `alpha-rgba8.avif`) are
asserted to stay `RGB24` with their own alpha, so the take over cannot widen.

## monochrome avif

closed, by a different route than the one this plan guessed. the guess was that
`handles` could take over the avif extensions too, but on windows the libheif
build has no AV1 decoder at all — `LibHeif::decoder_descriptors(16, None)`
reports `["libde265"]`, and vcpkg's libheif has no `dav1d` feature, so taking the
decode over would have meant a new native dependency (libaom) and a
platform-dependent code path. the `image` crate's avif decoder stays, and only
the *format* is corrected:

- `src/formats/heif.rs` gained `output_format(path, color_type)`, which the probe
  calls beside the webp one whenever it builds a decoder; the container probe
  below reaches the same format on its own. it returns `None` unless the file is
  an avif whose `av1C` sequence header says `mono_chrome`; for those it returns
  `Gray8`/`Gray16`, plus the alpha reported by the item's `auxl`/`auxC` entry, so
  a monochrome avif with alpha becomes `La8`/`La16`.
- the pixels are the ones the `image` decoder already produced and always had the
  right value: it converts the monochrome bitstream into rgb, where all three
  channels hold the luma. the result of `Read` on
  `sandbox/avif/snek - p003.avif` is 98.97% identical to the same page decoded
  from `sandbox/png/snek - p003.png` (mean absolute difference 0.0347, and the
  avif is a lossy encode), against 55.4 MiB of `RGB24` before and 18.5 MiB of
  `Gray8` now.
- `src/pixel.rs` needed one change for this: `write_planar` used to write every
  plane the frame has, from the interleaved buffer the decoder filled. a gray
  frame filled from an `Rgba8` buffer has one plane and three channels, so
  `planes_to_write` now takes the smaller of the two counts and fails only when
  the frame wants more planes than the buffer has channels. the unit test
  `a_gray_frame_can_be_filled_from_a_wider_buffer` covers it.

`tests/fixtures/mono-alpha.avif` is encoded from the same `mono-alpha.png` with
`avifenc`, so it exercises the whole path — container probe, format override,
writer — and its planes come back equal to the source like the heic ones.
`tests/fixtures/mono-alpha-10.avif` is the same source at ten bits per sample,
the one fixture that reaches a 16-bit avif: its planes come back as the source
numbers times 256, within the rounding of that encode.
`sandbox/avif` has exactly one monochrome page out of 35 (`p003`), which is the
file the paragraph above measures, and the other 34 still read as `RGB24`.
`target/bench/container-survey.py` prints the container facts of a set beside
what the plugin returns for each file, `target/bench/avif-probe-check.py` does
the same for every file of the avif corpus with an independent python parse of
its boxes, `target/bench/avif-survey.py` counts the `av1C` flags of the whole
corpus, and `target/bench/avif-mono-parity.py` compares one page with the png it
was made from.

## probing an avif without decoding it

closed as well, and it is what the plan called the probe cost. the `image` avif
decoder decodes the whole picture — and the alpha item beside it — inside its
constructor, because that is what gives it a size to report, so every avif was
decoded once to probe it and again when a frame asked for it. measured on four
pages of `sandbox/avif`, before the change:

| step | cost |
| --- | --- |
| `leading_boxes`, the container read this module already did | 0.27 to 0.55 ms |
| the probe, through the decoder | 42 to 106 ms |
| the frame request, another decode plus the copy out | 120 to 167 ms |

everything the probe records is in the container, so `src/formats/heif.rs` now
answers the probe itself in `image_info`: `ispe` holds the size, `av1C` the bit
depth and whether the samples are monochrome, `colr` whether an ICC profile is
attached, and `auxC` whether an alpha item exists. `decoder::probe` asks for that
first and only builds a decoder when the answer is `None`. a file is decoded
once again: when a frame asks for it.

`image_info` answers `None` — and the file keeps the decoder, the old cost and
the format override below — when anything about the container is not something
this reader can predict from: a file type other than `avif` or `avis` (an `mif1`
file under an `.avif` name can decode through the heif hook, whose color type
this module cannot predict), two `ispe` boxes that disagree, coded records of
different bit depths, a size of zero, and the `clap`, `irot` and `imir` boxes
that move the picture the decoder reports. the boxes are walked rather than
searched, so a name that appears inside a payload is not read as a box.

on the whole set, `open` — the time to create the clip — went from **6.43 s to
0.002 s** for the 35 pages, and the wall clock of `Read` plus that `open` from
11.92 s to 4.99 s at the default prefetch, with all 35 files reporting the same
format and size as before. `target/bench/avif-probe-check.py` confirms the
plugin's answer against its own parse of the same file's boxes, and the frame
request compares the size and color type the probe recorded with what the
decoder reports, so a container this reader gets wrong fails loudly instead of
writing a wrong frame.

## left over

- **heif and heic keep the hook probe**: it parses the header with libheif and
  costs 5 ms for all 35 pages of `sandbox/heic`, and a monochrome page is parsed
  a second time by libheif when it is decoded. that is small enough to leave.
- **a heif container is still not probed here**: the same `ispe`/`av1C` read
  would apply to a `.heic` that stores av1, but nothing in the corpus does.

## acceptance

- `target/bench/heic-validate.py` reports 35 of 35 decoding (this replaced the
  narrower `heic-survey.py`, which only counted verdicts).
- the monochrome files come out as `Gray8`, and `mismatch=True` is still needed
  for the mixed set.
- the four files that already worked are unchanged, because they still use the
  hook.
- `cargo test --locked` passes: 58 tests. the heif side covers the extension
  check, the color type split, packed rows with padding, `La8` interleaving,
  16-bit copies, a plane wider than its stride and a plane the probe disagrees
  with. the avif side covers the container reader (a monochrome record with its
  depth, a colour record, an alpha item by either auxiliary type, the metadata
  boxes, a payload that must not be read as a box, the boxes stopping at the
  media data, a header past the read limit) and the probe built on it (a
  container it declines, a file type that is not `avif`/`avis`, and, when the
  fixtures are present, that `mono-alpha.avif` and `alpha-rgba8.avif` are
  described from their own containers while a declined file keeps the decoder
  and its format correction), plus the writer's
  `a_gray_frame_can_be_filled_from_a_wider_buffer`.
- `tests/readalpha.vpy` asserts `mono-alpha.heic` and `mono-alpha.avif` as
  `Gray8` colour plus `Gray8` alpha with the exact source samples,
  `mono-alpha-10.avif` as `Gray16` with the source samples times 256, and
  `alpha-rgba8.heic` and `alpha-rgba8.avif` as `RGB24` with their own alpha.
- `target/bench/sandbox-per-file.py` opens all six sandbox sets file by file:
  the only row that changed is `avif`'s monochrome page, `Gray8` 18.5 MiB where
  it was `RGB24` 55.4 MiB, and the decoded total of that set went 1928 → 1892
  MiB. it still reports those numbers with the container probe in place, which
  is what makes the `ispe` sizes trustworthy: a size the probe got wrong is
  rejected when the frame is decoded.
- `target/bench/avif-probe-check.py` agrees with the plugin on all 37 avifs of
  the sandbox and fixture sets: the same `ispe` size, the same format, and the
  same `ImgSeqOriginalColorType` for each one.
- a ten bit avif with an ICC profile, which is not in the corpus, was encoded by
  hand and decoded: `Gray16` colour and alpha, and `ImgSeqHasICC=1` where the
  `colr` box holds a `prof` payload.
