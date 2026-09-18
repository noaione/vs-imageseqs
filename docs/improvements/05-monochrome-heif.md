# 05 — monochrome heif and heic fail to decode

- status: implemented in `src/formats/heif.rs`
- touches: `src/formats/heif.rs` (new), `src/formats/mod.rs` (new), `src/decoder.rs`, `src/lib.rs`
- result: 35 of 35 `sandbox/heic` files decode, the 31 monochrome ones as `Gray8`
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

## left over

- **monochrome avif**: `sandbox/avif` is all colour, so nothing exercises it,
  but the same wall exists there (the `image` crate's own avif decoder is a
  different path). the `handles` check can cover the avif extensions once a
  monochrome sample exists.
- **monochrome heif with alpha**: the packing is unit tested, no file exercised
  it end to end, and no such file is in the corpus.
- **the probe cost** below is untouched: heif is still probed through the hook
  and then parsed again by libheif when a monochrome page is decoded.

## acceptance

- `target/bench/heic-validate.py` reports 35 of 35 decoding (this replaced the
  narrower `heic-survey.py`, which only counted verdicts).
- the monochrome files come out as `Gray8`, and `mismatch=True` is still needed
  for the mixed set.
- the four files that already worked are unchanged, because they still use the
  hook.
- `cargo test --locked` passes: 21 tests, 8 of them new, covering the extension
  check, the color type split, packed rows with padding, `La8` interleaving,
  16-bit copies, a plane wider than its stride, and a plane the probe disagrees
  with.
