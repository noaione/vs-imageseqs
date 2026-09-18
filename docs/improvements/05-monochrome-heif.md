# 05 — monochrome heif and heic fail to decode

- status: proposed
- touches: `src/decoder.rs` (and possibly `src/pixel.rs`)
- expected: 31 of 35 sandbox heic files start decoding; they are monochrome
  pages, so the natural output is `Gray8` rather than `RGB24`
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

decode heif and heic through `libheif-rs` directly in `src/decoder.rs` instead
of through the `image` hook, at least for the cases the hook cannot represent.
the crate is already a direct dependency (`libheif-rs = 3.0.0` with the `image`
and `v1_23` features), so this adds no native dependency and no licence change.

- build the context from the file rather than from memory:
  `HeifContext::read_from_file(path)` (or `read_from_reader`), then
  `primary_image_handle()`.
- pick the colour space from the handle the way the hook does, but accept the
  planar results as well: `ColorSpace::Monochrome` for a single plane,
  `ColorSpace::Ycbcr(YcbcrChroma::Chroma420)` for yuv sources,
  `ColorSpace::Rgb(RgbChroma::Rgb)` when a colour conversion is wanted.
- read the planes rather than an interleaved buffer. `write_planar` already
  writes planar frames from an interleaved decode buffer, so a planar source
  means copying plane by plane with libheif's stride instead of row slicing one
  buffer, and for monochrome the frame is `Gray8`/`Gray16` with one plane.
- keep the `image` hook registered for avif, which decodes fine (35 of 35 in
  `sandbox/avif`), or route avif through the same code path once it is proven.

the format mapping already handles this: `ColorType::L8` maps to
`PixelFormat::Gray8`, so a monochrome heif needs no change in `pixel.rs` beyond
the plane copy. `ReadAlpha` needs the same treatment for the alpha channel if a
heif file has one.

## also worth measuring while here

`sandbox/avif` shows a second cost in the same area: creating the clip probes
35 files in 6.10 s (174 ms each) and the decode then reports another ~150 ms of
`open` per frame, which is the container being parsed a second time. the same
question applies to heif. probing once and reusing that work, or making the
probe read only the header boxes, would remove seconds from every avif and heif
chapter.

## acceptance

- `target/bench/heic-survey.py` reports 35 of 35 decoding, and the four files
  that already worked must produce byte-identical frames.
- the monochrome files must come out as `Gray8` (single plane, no colour
  conversion), and `mismatch=True` must still be needed for the mixed set
  because it mixes monochrome and yuv sources.
- `tests/readalpha.vpy` and `cargo test --locked` unchanged.
