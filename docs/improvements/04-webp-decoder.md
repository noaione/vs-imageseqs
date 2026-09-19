# 04 — webp decoder

- status: implemented, landed with [03](03-webp-yuv-output.md)
- touches: `Cargo.toml`, `build.rs`, `vcpkg.json`, `THIRD_PARTY_NOTICES`,
  `LICENSES/`, `src/formats/webp.rs`, `src/decoder.rs`, `docs/IMPLEMENTATION.md`
- result: the webp set reads in 4.24 s instead of 5.66 s at the default
  `prefetch` and 15.60 s instead of 19.97 s with `prefetch=0`; the yuv output
  of 03 on top of it takes those to 3.39 s and 11.94 s
- risk taken: one more native dependency, with a linker and a licence
  obligation behind it

## problem

`image`'s `webp` feature is `image-webp` 0.2.4: pure rust, single threaded (no
rayon or thread use anywhere in the crate), and it decodes into an internal
canvas that it then copies into the caller's buffer. one 2903x4128 lossy frame
cost 265 ms of cpu in `read`, and there was no way to point it at a buffer we
own, which is why [02](02-frame-write-path.md) could not remove the extra copy.

bestsource uses ffmpeg, which slice-threads a single vp8 frame: 24 ms/frame
with the default threads on this set against 351 ms with one. per-thread ffmpeg
is not dramatically faster than `image-webp`, the difference is that it can put
all twelve cores on one frame, and libwebp cannot do that either (it is single
threaded per image, just with simd and a lower constant).

## what was chosen

**libwebp** through `build.rs`, not a `*-sys` crate and not bindgen:

- `vcpkg.json` gains `{ "name": "libwebp", "default-features": false,
  "features": ["simd"] }`. the manifest already builds dav1d, libheif and
  libde265, and the check that x265 stays disabled is unchanged.
- `build.rs` finds it: `vcpkg::find_package("libwebp")` on windows through the
  same local adapter the other native dependencies use; elsewhere `pkg-config`
  is asked for libwebp 1.2.0 or newer and its answer is turned into link flags
  by hand, preferring the `static` form of every library that has an archive on
  disk. `pkg_config::Config::probe` cannot be used for that: it keeps a library
  shared whenever its archive sits under a system prefix such as `/usr/lib`,
  which is where `libwebp-dev` installs `libwebp.a` (measured: the flags come
  out as `-lwebp` and the plugin ends up with `NEEDED libwebp.so.7`). A build
  that only finds the shared library still links, with a cargo warning.
  On apple the archive has to be named rather than asked for: `ld` reads no
  `-Bstatic` hint and looks for `libwebp.dylib` before `libwebp.a` in the one
  directory homebrew installs both into, so `+whole-archive` is used there,
  which rustc resolves to a path and passes on as `-force_load <path>`, plus
  `-Wl,-dead_strip_dylibs` so a dylib that a dependency's own link flags name
  (libheif's generated `libheif.pc` names libsharpyuv) does not stay a run time
  dependency after the archives have supplied its symbols. `build.rs` has the
  detail.  `vcpkg` and `pkg-config` are build dependencies of their platform only, under
  `[target.'cfg(windows)'.build-dependencies]` and its `not(windows)` twin.
- the four entry points used are declared by hand in `src/formats/webp.rs`:
  `WebPGetInfo`, `WebPDecodeRGBInto`, `WebPDecodeRGBAInto` and
  `WebPDecodeYUVInto`. they have kept their signatures since libwebp 0.4, so a
  binding generator would add a build dependency and a header search path for
  nothing.
- licences: `LICENSES/libwebp-COPYING.txt` (copied from the vcpkg install) and
  a `THIRD_PARTY_NOTICES` entry. bsd-3-clause asks for the notice and the
  licence text, not the lgpl corresponding-source treatment that libheif and
  libde265 need. `AGENTS.md` lists libwebp in the native set.
- the wheel expectation is unchanged: plugin only, `py3-none-win_amd64`,
  `hatch_build.py` untouched.

`image-webp` is still compiled in: `image`'s `webp` feature is what lets the
crate *probe* a webp file at all (`output_format` reads the container first,
and `probe` asks `image` for the colour type, the icc profile and the exif
orientation). only the pixel decode moved.

### lossless webp (the open question, answered)

one path, libwebp, for lossy and lossless alike. measured on the three uniform
lossless files of `target/bench/lossless` (3672x5274) against the same files
under a `.png` name, which routes them through `image-webp`, `prefetch=0`, six
passes each:

| decoder | best pass | range over six passes |
| --- | --- | --- |
| libwebp | 156.8 ms/frame | 157-209 ms |
| `image-webp` | 175.4 ms/frame | 172-210 ms |

the ranges overlap, so on three files that is not evidence to decide on, only
to stay with one decode path, one error type and one place where the bitstream
is read. libwebp is not behind on lossless and it is far ahead on lossy, so
routing both through it costs nothing measurable.

## what changed in the code

- `src/formats/webp.rs` is a new module: `handles`, `output_format` (the
  container walk behind [03](03-webp-yuv-output.md)), `decode`, the two decode
  paths and the extern declarations. `src/formats/` already held one module per
  container the `image` crate cannot express.
- `src/decoder.rs` keeps the same `ImageInfo`/`DecodedImage` shape, so
  `source.rs` and `pixel.rs` do not care which decoder ran. `DecodedImage`
  carries `Pixels`, either `Interleaved { color_type, buffer }` or
  `Planar(Vec<Vec<u8>>)`.
- the probe still comes from `image`, and the size `WebPGetInfo` reads from the
  bitstream is checked against it, the way the `image` path checks its own
  decoder. a mismatch is an error rather than a resize.
- `docs/IMPLEMENTATION.md` records the split. `tests/fixtures/lossy.webp` (a
  70 byte ffmpeg-encoded 16x16 frame) is what the module tests decode.

## results

sandbox webp set, 35 files, best pass of three:

| build | frames | `prefetch=0` | `prefetch=16` |
| --- | --- | --- | --- |
| `image-webp` | 5.66 s | 19.97 s | 4.12 s |
| libwebp | 4.24 s | 15.60 s | 2.62 zszzzzss |
| libwebp + yuv ([03](03-webp-yuv-output.md)) | 3.39 s | 11.94 s | 1.87 s |

the decoder alone (first to second row) is worth 1.33x on frames and 1.28x on
the serial row. it does not close the gap to bestsource on its own: libwebp
decodes a webp in one thread, and it still produced rgb until
[03](03-webp-yuv-output.md) landed.

per stage cost on the first four sandbox files with `debug=True` and
`prefetch=0`:

| stage | `image-webp` | libwebp | libwebp + yuv |
| --- | --- | --- | --- |
| decode | 520 ms | 265 ms | 185 ms |
| into the frame | 86 ms | 38 ms | 8 ms |
| total | 607 ms | 303 ms | 193 ms |

decode is 1.96x faster, which is the high end of the "~1.5-2x than pure rust"
in the options table this plan started from. the middle column also shows the
frame write dropping with it, because the decode no longer ends in a canvas
copy; the last column is [03](03-webp-yuv-output.md) taking the same path down
to 1.5 bytes per pixel and a plain row copy.

## acceptance (met)

- stage split on the webp set with `prefetch=0`: decode 520 → 265 ms per frame,
  and the total with it.
- the decoded pixels are identical to the `image` path for the same file. the
  module tests compare libwebp's own rgb output against the planes, and
  `target/bench/webp-yuv-check.py` compares the yuv planes against
  bestsource/ffmpeg (max delta 0).
- `cargo test --locked` (45 tests, six of them new for this module),
  `tests/readalpha.vpy`, and the png, jpeg, jxl, avif and heic sets: unchanged.
- licence files present in both the repository and the built wheel.
