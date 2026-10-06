# removing libwebp

Status: **implemented** — nothing links libwebp any more. An ordinary still and
an animated container's rectangle are both decoded by `wpd`, the tests hold it
against reference payloads captured from libwebp before it was unlinked, and
`build.rs`, `vcpkg.json`, both Linux builders and the notice bundle no longer
name it. Investigated and implemented on 2026-10-06 against the wpd revision
[35](35-wpd-webp-decoder.md) pinned, whose still decode this completes.

The goal is one decoder for every webp: no libwebp in the link, no libwebp in
`vcpkg.json`, no libwebp in either Linux native build, no `libwebp-COPYING.txt`,
and no test that needs the library to say what wpd should have produced.

## what reached libwebp

These are the five places that had to change, and all five did: what follows
is the state before the removal.

| place | entry point | what it did before the removal |
| --- | --- | --- |
| `src/animation/webp.rs` | `WebPDecodeRGBAInto` | one `ANMF` rectangle, wrapped in a RIFF container when it carries `ALPH` |
| `src/formats/tiff.rs` | `WebPDecodeRGBInto` | one webp-compressed strip, written into the page's buffer |
| `src/formats/webp.rs` `decode` | `WebPGetInfo` | a second opinion on the size the container was probed for |
| `src/formats/webp.rs` tests | `WebPDecodeRGBInto`, `WebPDecodeYUVInto` | the oracle the still decoder is held against |
| `src/formats/webp.rs` tests | `WebPEncodeLosslessRGB`/`RGBA`, `WebPFree` | writes the round trip's fixtures at test time |

Nothing else in this tree linked it. libheif did not: the installed
`vcpkg_installed/x64-windows-static-md/lib/heif.lib` holds no `WebPDecode`,
`WebPEncode`, `WebPFree` or `sharpyuv` symbol at all, and `vcpkg.json`'s only
name for libwebp was this repository's own dependency. The Linux plugin's
`nm -D --undefined-only` names no WebP entry point now, and the release
`vs_imageseqs.dll` carries no `libwebp`, `WebPDecodeYUVInto` or `WebPGetInfo`
string at all, which is what the Windows binary can be checked for.

## the blend is already ours

`over()` and `div_by_255` in `src/animation/webp.rs` are a port of libwebp's
integer routine, and the canvas, the clipping, the blend decision
(`frame.blend && payload_has_alpha`) and the disposal policy are this
repository's code. Removing libwebp therefore changes no blended pixel and no
disposal decision, and the plan keeps the port as it is. That is what makes the
fixtures' expected bytes survive the removal, and it is why the animation parity
below is a check of the *decoder* and not of the compositor.

Adopting wpd's own `Animation` compositor stays rejected for the reason
[35](35-wpd-webp-decoder.md#animation-parity-is-an-explicit-choice) gives: it
honours disposal, and this reader deliberately does not.

## the yuv path, checked before its oracle goes

The yuv output is the part of this removal with the least room for error,
because the only thing that has ever said it is right is the entry point the
removal deletes: a lossy file with no alpha channel is handed out as
`YUV420P8` rather than as rgb, so its pixels are three planes this plugin
writes itself, and `WebPDecodeYUVInto` in this module's tests is what they are
held against. The path was therefore measured against the last build that
decoded a still with libwebp, `target/bench/vs_imageseqs-before-wpd.dll` (SHA256
`02C64D00894ACF0CB5509944348CF1F9058451E4A23074F2D5EC15F6CB216452`), before anything else:

| check | result |
| --- | --- |
| plane parity, 160 frame lines over 66 files, both clips | identical, 0 differences |
| handed out as `YUV420P8` | every lossy page of the 35 page `sandbox/webp` set, the 10 page `bs-seq` set, 6 lossy fixtures and 10 generated sizes |
| stays `RGB24` | the lossless pages, and every file that carries alpha |
| the alpha clip | `Gray8` for all of them |

The generated sizes are the ones a 4:2:0 frame has the least room in: one, two
and three pixels, 9x17, 17x9, 35x21, 63x1 and 1x63, written by
`target/p36/make-odd-webp.py` as a lossy encode of a cheap gradient, beside two
lossless files of the same shape as the control.

### the crash this check found

The one pixel wide files aborted the process. A 4:2:0 frame of a picture
narrower than the chroma step has no chroma plane at all: VapourSynth hands it
over as no bytes and a stride of zero, `frame_plane_dimensions` gives the plane
no columns either, so the check in `clip.rs` (`stride < row_bytes`) is `0 < 0`
and passes, and `write_planes` then divided the length by that stride. The
release profile is `panic = "abort"`, so a one pixel lossy webp took the whole
process down on its first frame request -- and it did so in the committed tree
too, which is why the check ran against both builds.

`PlaneRows` now answers `rows()` with a checked division, which is zero for a
plane with no stride, and `write_planes` walks that rather than dividing.
`tests/fixtures/lossy-1x1.webp` (44 bytes, written by
`tests/make-tiny-webp-fixture.py`, and reproduced byte for byte by it) with the
validator's `test_tiny_lossy_webp` is the guard: five checks, and the luma of
the single pixel rather than its chroma, because a plane VapourSynth will not
hand out has no samples to read.

## what the prototype proved

Three files changed, and the two decoders below are the whole of it:
`src/formats/webp.rs` gains `decode_rectangle`, `decode_packed`, `raw_input` and
one shared `decode_into_buffer`; `src/animation/webp.rs` loses `container` and
`push_chunk` and calls `decode_rectangle`; `src/formats/tiff.rs` loses its
per-strip `libwebp` call.

### the animation rectangle

A frame's payload is its own chunk sequence with no RIFF wrapper, and wpd takes
two shapes of it directly:

| `ANMF` payload | the slice wpd reads |
| --- | --- |
| `ALPH` followed by `VP8 ` | the sequence as it stands: both chunk headers and both bodies, padding included |
| `VP8 ` or `VP8L` alone | the coding chunk's body, `payload[8..8 + declared length]` |

The second row is the adaptation libwebp's reader did not need: a bare `VP8`
bitstream handed over with its chunk header is refused as `not a WebP file`,
while the body decodes. That is also why the old code wrapped only the `ALPH`
case: libwebp's simple entry points wanted a file, so a frame with alpha was
given a `VP8X` header, and a frame without one was handed the sequence and
happened to be understood. With wpd the wrapper goes, and with it the per-frame
RIFF allocation `container` made.

`Options::frame_size_limit` is the rectangle's own pixel count from the `ANMF`
header, so a bitstream asking for more than the container states is refused
before anything that size is allocated; the decoded size is still compared with
the rectangle in `Source::presentation`, which is what keeps a lying header an
error rather than a canvas drawn somewhere else.

Measured over the fixtures, the 48 files of the upstream `wpd-test-data`
corpus, and three synthetic 512x512 and 384x384 animations:

| check | result |
| --- | --- |
| pixel parity, every frame of both clips | 809,121 + 259 lines identical (before vs after) |
| `tests/readalpha.vpy` | 941 checks, 0 failures, no warnings |
| animation read, `prefetch=0`, 510 frames | 0.5392 s → 0.4649 s (**1.16x**) |
| animation read, `prefetch=4`, 510 frames | 0.4611 s → 0.4002 s (**1.15x**) |
| `cargo test --locked` | 341 passed |

The corpus matters as much as the numbers: of its 204 animated rectangles, 160
are `VP8L`, 15 are bare `VP8`, and 29 are `ALPH` + `VP8` — the branch
`tests/fixtures/animation.webp` never reaches, because all four of its frames
are `VP8L`. The three synthetic files (`quality=80` rgb, `quality=80` rgba and
`lossless` rgba, all full-canvas rectangles) were built for the speed side,
since the shipped fixtures are 16x12 and would measure the sampler.

The gain includes the removed container allocation, so it is an integrated read
and not a decoder-only figure. Both builds share the compositor, the canvas and
the frame write, which is the rest of the work.

### the tiff webp strip

A strip holds a whole bitstream of its own, so it is a still from wpd's point of
view: `decode_packed` asks for `Rgb` for a three channel page and `Rgba` for a
four channel one and copies the rows out. The validator's own oracle covers it —
it decodes `tiff-webp.tiff` and `tiff-webp-uncompressed.tiff` and requires every
sample to match — and all 57 tiff fixtures are byte identical before and after.

It also fixes a latent bug. The old call was `WebPDecodeRGBInto`, three samples
a pixel, with a stride of `width * channels`: for a page that states four
channels libwebp wrote three samples into a four sample row, which left the
fourth sample of every pixel zero and the picture interleaved wrongly.
`tiff-webp-alpha.tiff` and `tiff-webp-alpha-uncompressed.tiff` are that page and
libtiff's own decode of it, added once the removal had landed; the risks section
below has the two decodes side by side.

## what landed

### the tests hold wpd against libwebp's own output

The oracle could not survive the removal, so it was frozen first.
`tests/fixtures/lossy-planes.bin` and `lossy-rgb.bin` are libwebp's
`WebPDecodeYUVInto` and `WebPDecodeRGBInto` output for `lossy.webp`, captured
while the library was still linked, and `the_planes_are_the_ones_libwebp_wrote`
and `the_planes_rebuild_the_rgb_libwebp_wrote` read them rather than calling it.
A payload captured from wpd after the fact would have been a restatement rather
than evidence.

The encoder went the same way. The round trip used to write two 3x2 lossless
streams at test time through `WebPEncodeLosslessRGB` and
`WebPEncodeLosslessRGBA`; those two streams are now
`tests/fixtures/lossless-rgb.webp` and `lossless-rgba.webp`, committed, and the
round trip reads a fixture instead of one it just encoded. The `RGB`/`RGBA`
sample arrays are still what the expectations are derived from, so the check
keeps its meaning: a decode has to hand back the samples the fixture was
written from.

With that, the `libwebp` module is gone entirely, `WebPFree` included.

### the still path's header check went with it

`decode` called `WebPGetInfo` on the whole file as a second opinion on the size
the container was probed for and refused a mismatch before the `Still` was
built. `header_dimensions`, its call site and the `metadata` stage it fed are
gone. `Options::frame_size_limit` still bounds the allocation, and
`check_picture` still compares the decoded picture against the probed size and
reports `changed after probing`, so nothing is unchecked: the refusal happens
when the frame is filled rather than when the still is built, which is the same
frame request because a clip fills the stream it has just been handed.

Two tests moved with it. `data_that_is_not_a_bitstream_has_no_dimensions` went
with the function it named, and `a_file_that_is_not_a_bitstream_is_reported`
became `the_stream_refuses_a_file_that_is_not_a_bitstream`, which fills a sink
the way the row-write check already does and pins wpd's `not a WebP file` in
place of libwebp's `did not recognise`.
`a_size_that_changed_after_probing_is_reported` fills one too, because that
refusal is the decode's now.

### the build, packaging and legal surface

- `build.rs` is two link arguments now, one per platform, and locates nothing:
  `bind_definitions_locally`, the ELF `-Wl,-Bsymbolic` wpd's gamma tables need,
  and `drop_unused_dylibs`, the Apple `-Wl,-dead_strip_dylibs` that keeps a
  dependency's link interface from carrying a library no symbol comes from.
  `MINIMUM_VERSION`, `SYSTEM_LIBRARIES`, `link_unix_libwebp`,
  `archive_directive`, `is_apple`, `library_names`, `archive_exists` and the
  two probes are gone, and so are the `vcpkg` and `pkg-config` build
  dependencies they existed for.
- `vcpkg.json` no longer names libwebp, so a Windows build installs no webp
  package.
- `tools/build-manylinux.sh` and `tools/build-musllinux.sh` no longer fetch or
  build libwebp 1.6.0. `tools/linux-native-cache.py`'s required-install list
  drops its header, its archive and its `.pc` file with it, or a real prefix
  would never look complete and the native cache would never hit again. The
  `readelf -d` guard keeps `libwebp|libsharpyuv` as names that must not appear
  as a `NEEDED`.
- `tools/check-linux-wheel.py` and `tests/check-packaging-tools.py` drop
  `LICENSES/libwebp-COPYING.txt` from their required lists, and the packaging
  check's "a missing library invalidates a stamped prefix" case deletes
  `lib/libde265.so` instead of the archive that no longer exists.
- `.github/workflows/build.yml` stops installing `webp` by brew and drops
  `/opt/homebrew/opt/webp/lib/pkgconfig` from `PKG_CONFIG_PATH`. Its "links
  libwebp statically" step becomes the same `otool -L`/`readelf -d` read with
  the sense flipped: a webp library among the dependencies is now the failure.
- `.github/workflows/rust-tests.yml` drops `libwebp-dev` from its apt line, and
  `README.md` and `AGENTS.md` stop naming the webp development package.
- The embedded libheif had one more dependency than the notices said, and the
  two CI jobs that build it from source said so. `libheif-sys` asks libheif for
  its `libsharpyuv` colour transforms, and libheif links whatever it finds: on
  Linux that was the sharpyuv the test job got from `libwebp-dev`, and on macOS
  it is homebrew's `webp`, which the runner image already had. Both
  `tools/manylinux-toolchain.cmake` and
  `tools/macos-libheif-toolchain.cmake` now set `WITH_LIBSHARPYUV` off, which is
  what the Windows vcpkg port already did, so the dependency is gone rather than
  satisfied. Without it the Linux test job could not resolve `-lsharpyuv` once
  `libwebp-dev` came off its apt line, and the macOS bundle carried
  `libsharpyuv.0.1.2.dylib` with no symbol of the plugin's referring to it. The
  ELF `--as-needed` and the Apple `-Wl,-dead_strip_dylibs` are what the two
  platforms do about a leftover like that; not emitting the dependency at all is
  what the toolchain files added.
  `libheif-sys` does not treat that configuration as a build input, so
  `rust-tests.yml` also clears the cached `libheif-sys` build, the way the macOS
  wheel job already did, or a cache from a run with the old toolchain would
  replay the flags that name the library.
- `docs/LINUX-BUILD.md` and `docs/IMPLEMENTATION.md` describe what is left, the
  link argument included.
- `LICENSES/libwebp-COPYING.txt` and its line in `LICENSES/README.md` are gone,
  and `THIRD_PARTY_NOTICES` drops the libwebp component, its bsd-3-clause
  attribution, the `libsharpyuv` mention and the license URL.
- `docs/BENCH.md` gains the animation table and `CHANGELOG.md` gets the entry.

## risks and decisions

- **a decoder per rectangle.** Every rectangle builds and drops one wpd
  decoder, which was the setup cost the animation path paid with libwebp too.
  It measured *faster*, not slower, so the persistent-decoder designs recorded
  in [35](35-wpd-webp-decoder.md#optional-persistent-decoder-and-thread-budget)
  stay unnecessary.
- **no fallback was removed.** The two libraries were selected by container and
  never retried against each other, so a file wpd refuses is refused now too;
  the removal deleted an unused second opinion rather than a safety net.
- **`frame_size_limit` is a bound, not a policy**, exactly as in
  [35](35-wpd-webp-decoder.md): the limit is the probed pixel count, so a
  bitstream that asks for more is refused before anything that size is
  allocated, and `check_picture` refuses one that decodes to a size the probe
  did not record.
- **the alpha tiff strip** is verified rather than assumed now.
  `tiff-webp-alpha.tiff` is a four sample page whose strip is libtiff's webp
  bitstream and `tiff-webp-alpha-uncompressed.tiff` is libtiff's own decode of
  it, so the pair reads identically. The build before the removal read that
  strip as `[9, 62, 104, 132, 185, 228]` where libtiff's own decode holds
  `[9, 50, 82, 132, 173, 206]`, with alpha `[50, 95, 0, 173, 218, 0]` against
  `[40, 80, 120, 160, 200, 240]`: the zeroed fourth sample and the shifted rows
  a three sample call leaves. `tests/make-tiff-webp-fixtures.py` has the
  commands that make the pair.
- **the asm and the link argument stay.** x86 and x86-64 builds still need
  `nasm`, ARM still assembles with the C compiler, and the ELF link still needs
  `-Wl,-Bsymbolic` for wpd's gamma tables.
- **what is not decided here**: whether to drop `libwebpdecoder`/`libwebpdemux`
  from anything downstream, and whether the tiff reader should widen its strip
  support now that the decoder behind it reads four channels.

## acceptance

Everything the plan asked for holds, measured against the build immediately
before the removal:

- `tests/readalpha.vpy` passes all 954 checks with no warnings, on the release
  build and on the DLL taken from the built wheel.
- Frame parity is byte identical for every webp fixture, the upstream
  `wpd-test-data` corpus, the animation corpus above and the 57 tiff fixtures
  that existed then, for both clips of `Read` and `ReadAlpha`. The four sample
  tiff fixtures added afterwards are the one exception by construction: the
  build before the removal reads one of them wrongly, which is the fix the risks
  section records.
- The animation read is faster than before on the corpus above, at
  `prefetch=0` and `prefetch=4`.
- `cargo test --locked`, `cargo clippy --locked --all-targets -- -D warnings`
  and `cargo fmt --all -- --check` are clean, and the frozen reference payloads
  are what the tests compare.
- `tests/check-packaging-tools.py` and `python -m build` pass, with no libwebp
  name anywhere in the wheel, its notices or its relink source bundle. Both
  Linux wheels are built in their pinned containers by CI: nothing local can
  build them, and what changed there is one fetch, one build and one name off
  two required lists.

## what was measured, and what was not

Baseline `p36-before.dll` SHA256
`C587E6058E8DAD4F2A5EC9EB93155DC6CC8D0413289801A8EF7734229D6B6D4C`, 9,039,872
bytes, built from `2d0d77f`; final `vs_imageseqs.dll` SHA256
`2373FD9A42D64459A2D5ECB0A8D5BC54212265506948101827293f8c7ce71518`, 8,902,656
bytes, built from this change. Windows x86-64, Intel i5-11400H, release
profile, three and five rounds of alternating fresh processes, medians
reported.

The removal is 137,216 bytes, 134 KB. The prototype `p36-after.dll` of the
section above measured 8,916,480: it had stopped *calling* libwebp but still
linked it for the paths whose `WebPGetInfo` the module kept, so the 13,824
bytes between it and the final build are the archive object files the linker
kept for a check that had stopped being the only one.

Frame parity over every set of `target/bench/p36-parity-anim.py`, both clips
and every displayed frame, is 809,492 sampled frame lines with no difference:
the 11 remaining webp fixtures, the upstream `wpd-test-data` tree's 48 files,
the 57 tiff fixtures that existed then, the three synthetic animations, the
35 page `sandbox/webp` set, the 10 page `bs-seq` set and the 10 generated odd
sizes. The baseline predates the divide-by-zero fix the commit before the
removal added, so it aborts on a lossy file one pixel wide and contributes no
rows for `tests/fixtures/lossy-1x1.webp` or `target/p36/odd/lossy-1x63.webp`;
those two are excluded from both sides of the comparison, and the validator's
`test_tiny_lossy_webp` and the unit test beside `PlaneRows::rows` are what
cover them. The four sample tiff pair added afterwards is the other exception,
and deliberately so: it is the fixture the removal's tiff fix needed, and the
baseline reads it wrongly.

After the tiff pair landed the validator runs 954 checks, all of which pass on
the release build and on the DLL taken from the built wheel.

Not measured: the still path's speed, which is the only thing in it that
changed beyond the header check's removal and only removes work, and the
musllinux and manylinux builds, which only their pinned containers can run.
