# 12 — heif and avif hand out their own planes

- status: proposed
- touches: `Cargo.toml` (the `dav1d` crate becomes a direct dependency),
  `src/pixel.rs` (the format variants and their subsampling), `src/decoder.rs`,
  `src/formats/heif.rs`, `src/formats/avif.rs` (new), `src/color.rs`,
  `src/clip.rs`, `src/source.rs`, fixtures, `tests/readalpha.vpy`, `README.md`
- expected: a colour heic or avif page is handed out as the yuv its bitstream
  holds — `YUV420P8` for the sandbox sets, `YUV444P10` for a ten bit page —
  instead of r,g,b reconstructed into `RGB24`/`RGB48`, with `_Matrix` and
  `_Range` stating what the file said
- risk: high — every colour heic and avif frame changes format, bytes per frame,
  plane sizes, and the answer `clip.format` gives a graph, which is the largest
  output change this plugin has made

## problem

Both formats are stored as yuv, and both are handed out as rgb:

- **colour heif/heic** go through `image`'s decoder hook
  (`libheif_rs::integration::image::register_heic_decoding_hook`), whose
  `HeifDecoder` asks libheif for `ColorSpace::Rgb(RgbChroma::Rgb)` and passes no
  `DecodingOptions` at all, so libheif runs its own yuv to rgb conversion. The
  plugin then writes three planes out of that interleaved buffer.
- **colour avif** go through `image`'s own avif decoder (`mp4parse` + the
  `dav1d` crate), which converts to rgb in Rust, always reports four channels
  (`Rgba8`/`Rgba16`) whatever the file holds, and always decodes the alpha item
  when the file has one — even when the clip is `Read` and the alpha is dropped.
  It then left-aligns the samples into 16 bit words
  (`*item = (*item).rotate_left(16 - bit_depth)`), which is where the nominal
  depth is lost: a ten bit page measures `max=65472` in a frame that claims
  `RGB48`.

Measured on the samples in `sandbox/hitokage-sample` (6000x4000, one picture
through five formats), frame 0 through the current build:

| file | states | handed out as | frame bytes | max sample |
| --- | --- | --- | --- | --- |
| `avif-yuv420p.avif` | 4:2:0, 8 bit | `RGB24` | 72 MB | 255 |
| `avif-yuv422p.avif` | 4:2:2, 8 bit | `RGB24` | 72 MB | 255 |
| `avif-yuv444p.avif` | 4:4:4, 8 bit | `RGB24` | 72 MB | 255 |
| `avif-yuv444p10le.avif` | 4:4:4, 10 bit | `RGB48` | 144 MB | 65472 |
| `avif-yuv444p12le.avif` | 4:4:4, 12 bit | `RGB48` | 144 MB | 65520 |
| `jxl-rgb48le.jxl` | rgb, 16 bit | `RGB48` | 144 MB | 65535 |
| `png-rgb48be.png` | rgb, 16 bit | `RGB48` | 144 MB | 65535 |
| `tiff-rgb48le.tiff` | rgb, 16 bit | `RGB48` | 144 MB | 65535 |

The subsampling is thrown away entirely: a 4:2:0 page costs 72 MB of frame and a
whole conversion pass, where the file holds 36 MB of planes. On the sandbox sets
the same holds — `sandbox/avif` is 34 pages of `av1C … subsampling=(1, 1)` and
`sandbox/heic` has four colour pages among its 31 monochrome ones — and those
frames are what the lookahead budget is spent on.

## what the files state, which is what a hand-out has to repeat

| set | where the colour statement is | what it says |
| --- | --- | --- |
| `sandbox/heic` | a `colr` box of type `nclx` | primaries 1, transfer 13, matrix 6, full range |
| `sandbox/avif` | **no `colr` box in 34 of 35 files**; the value is in the AV1 sequence header's `color_config` | `avifdec --info` reports primaries 1, transfer 13, matrix 6, full range for the same pages |
| `hitokage-sample` avif | a `colr` box of type `nclx` | primaries 2, transfer 8, **matrix 2 (unspecified)**, limited range |

Three things follow.

1. The container is not always the source of the truth for avif, which is what
   plan [08](08-color-metadata.md)'s avif row assumed. The copy of the sequence
   header that `av1C` carries has the profile, the depth and the subsampling but
   **not** the CICP, so a probe that wants a matrix for a file with no `colr` has
   to read the sequence header OBU at the head of `mdat` — a bit read, not a
   decode — or take it from the decoder during the decode, which is too late for
   a probe-time property.
2. `matrix=6` with full range is the ordinary phone-photograph tagging (bt.601
   coefficients, jpeg-style full range), so the common case has a matrix that
   maps onto a VapourSynth `_Matrix` (6 is `VSC_MATRIX_SMPTE_170M`) and a range.
3. "Unspecified" exists and has to be handled: the `hitokage-sample` avif files
   state no matrix, and neither `image` nor libheif hands the samples over with
   that ambiguity intact today — `image` picks bt.709 for unspecified, with a
   comment saying the choice is arguable. A yuv hand-out must not guess a matrix
   and then label frames with it, so the rule this plan takes is **a file that
   states no usable matrix keeps the rgb path it has today**.

## change, as planned

Two stages, because the two formats have nothing in common but the shape of the
result, and because they carry very different risk.

### 12a — heif/heic through libheif's own planes

`formats::heif` learns to ask for `ColorSpace::YCbCr(Chroma::C420 / C422 /
C444)` instead of `ColorSpace::Rgb`, with a `DecodingOptions` that is
still the default except where it has to move. Nothing has to be parsed for the
colour: `ImageHandle::color_profile_nclx()` answers
`matrix_coefficients()`/`full_range_flag()`, `luma_bits_per_pixel()` and
`chroma_bits_per_pixel()` answer the depth, and `has_alpha_channel()` says whether
there is an alpha plane at all — all from the handle the monochrome path already
opens.

- the planes come back through `planes.y`/`cb`/`cr`/`a`, each a `Plane` with its
  own `width`, `height` and `stride`, which `src/formats/heif.rs` already reads
  for the monochrome path. Asking for `C420` where the file is 4:2:0 is a
  same-format copy inside libheif, so no chroma is invented — that is the one
  claim here to check first, with a 4:2:0 file whose planes must come back at
  half size and match the planes a raw decoder produces.
- only the planes the frame needs are handed over: `Read` skips the alpha plane,
  `ReadAlpha` reads it, and a monochrome file stays the `Gray8`/`Gray10` it is
  today (the 31 monochrome heic pages do not change at all).
- **the orientation comes with it.** libheif applies `irot`/`imir` unless
  `ignore_transformations` is set, so today a rotated heic is handed out
  display-oriented while `ImgSeqOrientation` reports 1 and `apply_rotation=False`
  cannot undo it — the same defect [11](11-jxl-direct.md) fixed for jxl, and it
  is in this code either way. `ImageHandle::width()`/`height()` are the
  transformed size and `ispe_width()`/`ispe_height()` the stored one, so the swap
  is visible from the handle, but the *code* needs the container's `irot`/`imir`
  boxes, which the walk `formats::heif` already runs for avif. The plan reports
  the code the pair means (angle and mirror map onto the eight exif codes), uses
  the identity transform when libheif applied them, and sets
  `ignore_transformations` for `apply_rotation=False`.

### 12b — avif through `dav1d`, without `image`

The `dav1d` crate is already in the tree as `image`'s avif backend
(`avif-native` = `mp4parse` + `dav1d` 0.11.1), so this adds no native code, only
a direct dependency and a module. It is needed because the libheif built here
has no av1 decoder (`libde265` is its only one), so `12a` cannot serve avif.

`src/formats/avif.rs`, in the shape of `formats/heif.rs` and `formats/jxl.rs`:

- **the probe** stays a box walk and grows the boxes it needs: `iinf` (item
  types), `iloc` (extents), `iref` (`auxl` for the alpha item) beside the
  `ispe`/`av1C`/`colr`/`auxC` it reads today. `av1C` states the depth
  (`high_bitdepth`, `twelve_bit`) and the subsampling (`chroma_subsampling_x/y`),
  `pixi` agrees, and `colr` states the matrix when it is there. The sequence
  header's `color_config` is parsed for the files that have no `colr`, which is
  the whole sandbox set.
- **the decode** feeds the primary item's OBUs to `dav1d::Decoder::send_data`,
  takes the `Picture`, and copies the planes the frame wants:
  `picture.plane(PlanarImageComponent::Y/U/V)`, whose layout is
  `picture.pixel_layout()` (`I400`/`I420`/`I422`/`I444`) and whose samples are
  **right aligned** — a ten bit picture holds `0..1023`, which is exactly what a
  VapourSynth ten bit frame holds, so there is no shift at all. (The proof that
  they are right aligned is the `rotate_left` above: that expansion exists
  precisely because `image`'s `Rgba16` output wants them left aligned.)
- the alpha item is decoded **only for `ReadAlpha`**, which is work `image` does
  unconditionally today, and a monochrome item is a single plane, so
  [05](05-monochrome-heif.md)'s correction becomes unnecessary on this path.
- a **grid** primary item (a tiled avif is several items referenced by `dimg`)
  is refused with a clear error rather than half-read; keeping `image` for those
  is the alternative if a real file ever shows up.

### both

- `PixelFormat` gains the yuv variants the two readers can produce: the existing
  `YUV420P8`, plus `YUV420P10`, `YUV422P8`, `YUV422P10`, `YUV444P8`,
  `YUV444P10`, `YUV444P12`, and the gray depths that come with them
  (`Gray10`/`Gray12`, which is [10](10-nominal-bit-depth.md)'s half of the same
  table). `sub_sampling()` answers the right thing per variant instead of `(1,1)`
  for one format and `(0,0)` for everything else, and `plane_dimensions` /
  `frame_plane_dimensions` / `plane_bytes` / `expected_bytes` follow it — the
  write path itself needs no new idea, because `write_decoded_planes` already
  takes plane buffers with their own geometry.
- `src/color.rs` writes `_Matrix` from the file's coefficients (1, 5, 6, 9, 10
  onto the VapourSynth enum), `_Range` from its full-range flag, and
  `_ChromaLocation` only when the file names a position (`chroma_sample_position`
  is 0 — unknown — in every sandbox file, so it stays unset, which is the honest
  answer). This is plan 08's rule applied to samples rather than only to
  labels, which is why 12 wants 08 first: 08 reads the value, 12 hands out the
  planes it describes.
- the alpha clip of a yuv source is the gray format of the same depth, so
  `ReadAlpha` over a `YUV420P10` page gives a `Gray10` alpha instead of the
  `Gray16` opaque fill it would produce today.

## what this unblocks

- [10](10-nominal-bit-depth.md)'s `YUV420P10` row stops being "a format no
  source produces", and its avif and heic rows shrink: on this path the depth is
  carried by the format, so no shift and no format correction is needed for those
  two formats at all. 10 keeps jxl, the rgb files that state 10 or 12 bits
  (`tiff`, and `jp2` when [06](06-jpeg-2000-backend.md) lands), and the
  monochrome cases.
- the heic set's colour pages drop from 55.4 MiB to 27.7 MiB and the avif set's
  55.4 MiB frames the same, which is what the lookahead budget was fighting: 192
  MiB holds seven of those frames instead of three and a half.
- two conversions disappear from every frame: libheif's, and `image`'s Rust one
  plus the fourth channel it materialises for a file that has no alpha.

## validation

- **the plane content is the same picture.** The frames change format, so the
  parity test is not a hash: convert each new yuv frame back to rgb with zimg
  (`core.resize.Point` with the same `_Matrix`/`_Range`) and compare it with the
  frame the current build hands out for the same file, allowing rounding. On the
  avif set that comparison is also the check that the matrix the plan writes is
  the one the old path used, because `image`'s conversion for `matrix=6`, full
  range is the same bt.601, full-range transform.
- **a format table per set**, before and after, on the same command: the format
  name, the frame bytes, and the plane sizes of every file in `sandbox/avif`,
  `sandbox/heic` and `sandbox/hitokage-sample`, with the subsampling and the
  depth each file states.
- **the files that must not move**: the 31 monochrome heic pages stay `Gray8`,
  `sandbox/png`, `sandbox/jpeg` and `sandbox/jxl` are untouched, the
  unspecified-matrix avif files stay `RGB24`/`RGB48`, and `sandbox/mixed` — which
  exists to mix formats — still needs `mismatch=True` for the same reason.
- **`tests/readalpha.vpy`**: small crops of the new samples become fixtures
  (`avif-yuv420p`, `avif-yuv422p`, `avif-yuv444p10`), checking `format.name()`,
  the chroma plane's own size, `_Matrix`/`_Range`, an alpha page from a 4:2:0
  source, and the `mismatch=0` rejection when a yuv page is mixed with an rgb
  one.
- **the bench rows**, same-batch interleaved A/B against a baseline DLL, per the
  standing protocol: the `heic` and `avif` sections of
  [BENCH.md](../BENCH.md), where the decode stage must lose the conversion and
  the frame bytes halve.
- `cargo test --locked`, `cargo fmt`, `cargo clippy --workspace --all-targets
  --locked -- -D warnings`, `cargo build --release --locked`.

## left over

- **decoding straight into the frame's planes.** `dav1d::Decoder` is generic over
a `PictureAllocator`, so a custom one could hand dav1d the planes the frame
already owns and skip the copy the plan does today — the same idea as the
"decode straight into the frame planes" note in
[11](11-jxl-direct.md), and it waits for the same reason, since the copy is one
pass over bytes the decoder just wrote.
- **tiled (grid) avif**, **gain maps**, **10-bit PQ/HDR avif** and **12-bit
  heic** all exist in the wild and none is in the sandbox: the first is refused
  explicitly, the rest are not read.
- **`ColorSpace::Custom` and `NonVisual`** images (depth maps, alpha-only files)
  are what libheif 1.23 added; they are planar by construction and never reach a
  colour frame here.
- **dropping `image`'s `avif-native` feature** becomes possible once 12b owns
  avif, which would take `mp4parse` out of the tree. `dav1d` stays as a direct
  dependency either way, so the native set does not change; the notices and
  `build.rs` are unaffected because `dav1d` is built by its own crate today.
- **camera RAW** is not this: RW2 and its relatives are packed sensor mosaics
  needing a demosaic and a colour matrix, which is a decoder, not a format
  correction.
