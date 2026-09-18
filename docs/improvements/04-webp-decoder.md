# 04 — webp decoder

- status: proposed
- touches: `Cargo.toml`, `vcpkg.json`, `THIRD_PARTY_NOTICES`, `LICENSES/`,
  `src/decoder.rs`, `docs/IMPLEMENTATION.md`
- expected: decode 265 → ~150 ms per frame on the 12 Mpx set, plus the memory
  path improvements that only a decoder with a destination buffer can give
- risk: medium, it adds a native dependency and a licence obligation

## problem

`image`'s `webp` feature is `image-webp` 0.2.4: pure rust, single threaded (no
rayon or thread use anywhere in the crate), and it decodes into an internal
canvas that it then copies into the caller's buffer. one 2903x4128 lossy frame
costs 265 ms of cpu in `read`, and there is no way to point it at a buffer we
own, which is why [02](02-frame-write-path.md) cannot remove the extra copy
today.

bestsource uses ffmpeg, which slice-threads a single vp8 frame: 177 ms/frame
with `threads=1` and 15 ms/frame with the default threads on the same files.
per-thread ffmpeg is not dramatically faster than `image-webp`, the difference
is that it can put all twelve cores on one frame, and libwebp cannot do that
either (it is single threaded per image, just with simd and a lower constant).

## options

| option | decode | integration | licensing |
| --- | --- | --- | --- |
| `libwebp` through a `*-sys` crate or bindgen | ~1.5-2x faster than pure rust, single threaded per image | small: one branch in `open_decoder`, plus yuv and destination-buffer entry points | bsd-3-clause, notice only |
| ffmpeg | matches bestsource (slice threading) | large: build weight, format registration, yuv plumbing | lgpl-2.1+ with the same static-linking obligations already documented for libheif |
| stay pure rust, add threading | not available | would need decode support upstream in `image-webp` | none |

recommended first step: **libwebp**, because it is permissively licensed, small
to integrate, and it is the only one of the three that also unlocks
[03](03-webp-yuv-output.md) and the decode-into-frame path in
[02](02-frame-write-path.md).

## what libwebp changes beyond speed

`WebPDecodeRGBInto`, `WebPDecodeRGBAInto` and `WebPDecodeYUVInto` take a
destination pointer, a destination size *and a destination stride*. a
VapourSynth plane is a contiguous buffer with a padded stride, so:

- decode directly into the frame plane, dropping both intermediate copies, when
  the frame already exists (`prefetch=0`, or once the pool can hold frames),
- otherwise decode into a per-worker buffer with a tight stride, dropping the
  canvas copy and the zero fill,
- and with `WebPDecodeYUVInto`, write yuv planes with correct strides for plan
  03.

none of that is possible with `image-webp`, so this plan is a prerequisite for
the best version of 02 and all of 03.

## work

- `vcpkg.json`: add libwebp. the manifest already builds dav1d, libheif and
  libde265, so this is another entry, with the existing check that x265 stays
  disabled.
- `src/decoder.rs`: route webp inputs through libwebp behind the existing
  `ImageInfo`/`DecodedImage` shape so `source.rs` and `pixel.rs` do not have to
  care which decoder ran. keep the `image` path for every other format.
- licences: `LICENSES/libwebp-COPYING.txt` and a `THIRD_PARTY_NOTICES` entry,
  as AGENTS.md requires when linkage changes. bsd-3-clause needs the notice and
  the licence text, not the lgpl corresponding-source treatment.
- `docs/IMPLEMENTATION.md`: record the second decoder, why the format is split
  between two libraries, and which entry points are used.
- wheel: unchanged expectation, plugin only, `py3-none-win_amd64`. verify the
  DLL still contains nothing extra and that `hatch_build.py` needs no change.

## open question — lossless webp

lossy and lossless webp share one path today. libwebp handles both, but
`image-webp` may still be competitive (or better) on lossless files, which are
often small. measure both decoders on a lossless set before deciding whether to
route one mode, both, or to pick per file.

## acceptance

- stage split on the webp set with `prefetch=0`: `read` 265 ms → ~150 ms per
  frame, and the total with it. this measurement needs no other plan to land.
- the decoded pixels must be identical to the current path for the same file
  (compare plane bytes in a test or a throwaway script; this catches a wrong
  colour profile or alpha handling in the new path).
- `cargo test --locked`, `tests/readalpha.vpy` and the png/jpeg/jxl bench sets
  unchanged.
- licence files present in both the repository and the built wheel.
