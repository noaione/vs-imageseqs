# agents

## project

`vs-imageseqs` is a rust VapourSynth source plugin. keep the public plugin
identity unchanged:

- project name: `vapoursynth-imageseqs`
- namespace: `xyz.n4o.imgseqs`
- namespace name: `imgseqs`
- filter: `Read`, `ReadAlpha`
- crate: `vs-imageseqs`
- python distribution: `vapoursynth-imageseqs`

the plugin accepts an ordered `files:data[]` list and returns one frame per
file. `Read` returns the color clip, `ReadAlpha` returns the color clip and a
separate gray alpha clip that is opaque for files without an alpha channel. a
still file contributes exactly one frame; an animated gif, apng, webp, jpeg xl,
avif or heif/heic sequence contributes its displayed timeline sampled onto the
clip's constant frame rate, and plays once whatever its loop count says.
`fpsnum` and `fpsden` default to `24/1`. `mismatch` defaults to false;
when true, variable dimensions and formats are allowed. `apply_rotation`
defaults to true and hands out the picture the file's orientation describes,
including the width and height swap of orientations 5 to 8; a file states it as
an exif tag, as a jxl codestream header, or as the `irot` and `imir` item
properties of an avif or heif, which is the normative statement for those two
containers. when false the stored picture is handed out at the stored size and
`ImgSeqOrientation` still reports the file's code. `debug` defaults to
false and emits VapourSynth log timings when enabled. `prefetch` selects the
number of background decode workers used for sequential reads; it defaults to
half the logical cores (capped at four) and `0` disables lookahead decoding.
`prefetch_memory` is the lookahead budget in MiB; it defaults to the larger of
192 MiB and one frame of the largest image per worker, and `0` is rejected
because `prefetch=0` is how lookahead is disabled.
`icc_profile` defaults to false; when true, a source's embedded ICC bytes are
copied to the binary `ICCProfile` frame property without changing pixels or
applying a color transform. `ImgSeqHasICC` is still set independently, and
`ReadAlpha` copies `ICCProfile` to both output clips.

a frame is not always rgb: a lossy webp is handed out as its own yuv planes, and
so is a colour heif/heic page (libheif's planes) and a colour avif (dav1d's),
whenever the container states a matrix VapourSynth names. a container that states
code 2 or nothing keeps the rgb the plugin builds, and a monochrome page of any
family is gray at its own depth.

a frame's depth is the one its container states, not the word the decoder hands
over: a file that states nine to fifteen bits is handed out as the format that
names that depth (`RGB30` for ten bit rgb, `Gray12`, and so on) and the writer
moves every sample down by `word_bits - frame_bits`, which recovers it exactly.
eight and sixteen bit files, float files, and a container that states no depth
(`png`, `jpeg`, `tiff`, `webp`, and a heif page on the rgb path) keep the word
they had. jxl asks for sixteen bit words like every other reader and takes the
same shift; `formats/jxl.rs` says why.

frames are tagged with the colour their container states: `_Primaries` and
`_Transfer` for every family, `_Matrix` and `_Range` for a yuv frame, from an
`nclx` box, a `cICP` chunk, a jxl codestream header or an av1 sequence header
(`color_config`), and `_ChromaLocation` only when a subsampled frame's container
names a sample position. a code VapourSynth has no name for, code 2
(`unspecified`), and a file that states nothing all leave those properties unset,
and an rgb frame keeps `_Matrix=0`/`_Range=1` whatever the file says.

## repository rules

- use `pyproject.toml` and hatchling. do not add `setup.py`.
- keep the wheel plugin-only. do not add a python stub package or helper
  package unless explicitly requested.
- install the native library under `vapoursynth/plugins/imageseqs/` with `manifest.vs`.
- preserve user changes and untracked files.
- keep `vcpkg.json`'s disabled libheif default features unless the dependency
  policy changes. this avoids selecting x265.
- update `THIRD_PARTY_NOTICES` and `LICENSES/` when native dependencies or
  linkage change.
- NEVER commit anything yourselves, this should be done by human maintainers only. if you need to commit something, please ask for permission first.
- ALWAYS try to update CHANGELOG.md when a change a user can see lands. a note
  or a plan under `docs/`, benchmark tooling under `target/`, and anything else
  that changes no frame, no format, no property, no argument and no wheel
  content are not user-facing, so they do not get an entry: `docs/improvements/`
  is the record for those and its `README.md` index is where they are listed.
  if you are unsure about whether a change is user-facing, or about what to
  write, please ask for help.

## writing

- Write the README for people using the project. Use direct wording and simple language.
- Prefer lowercase headings and labels where it reads naturally. Keep proper capitalization for names and technical terms that require it.
- Use full GitHub URLs for README links to repository files. Relative links do not work when the README is rendered on PyPI.
- Never end a list item with a semicolon. Use a full stop or no terminal punctuation.

## source layout

- `src/lib.rs`: plugin declaration and registration.
- `src/source.rs`: `Read` and `ReadAlpha` filter creation, validation, and frame requests.
- `src/animation.rs`: the output timeline and the animation decoders. `Segment`
  is one input path's contribution and `SegmentTable` resolves an output frame
  to a path and a presentation, with checked `i128` rational arithmetic and no
  decoding of its own: a still is one frame at any rate, a segment covers the
  ticks that start before it ends, a picture is first shown at the first tick at
  or after it, and a zero or absent delay is held for one output tick.
  `AnimationSource` wraps one file's decoder and keeps a bounded window of
  decoded presentations, which is both the memory bound and what stops a
  lookahead worker that has moved ahead of a consumer from making that
  consumer's next request replay the wrong picture.
- `src/animation/`: one module per animated format. `apng.rs` composes APNG
  frames at the file's own depth rather than at a fixed one. `gif.rs` reads the
  timeline with the frame decoding skipped and composes the canvas itself,
  keeping two deliberate departures from the specification: the background
  colour is never used, and `Any` disposal means `Keep`. `webp.rs`
  walks the RIFF container for the same timeline, hands each frame's own chunk
  sequence to the still decoder, and composes the canvas with this tree's own
  port of libwebp's integer alpha blending. `jxl.rs`
  scans frame headers without rendering and decodes each presentation from its
  own seek checkpoint. `sequence.rs` reads an avif or heif sequence's sample
  table and clean aperture from the container, because the embedded libheif
  1.23.1 build reports the wrong per-sample duration and `libheif-rs 3.0.0`
  exposes no sample count. `heif.rs` replays a visual track and applies the
  aperture per plane; a colour-only read of one still decodes its linked alpha
  track, which that wrapper exposes no way to skip.
- `src/prefetch.rs`: the lookahead pool, its window, and its byte budget.
- `src/clip.rs`: the clips a sequence hands out and the frames they cache, which
  the lookahead workers build. the demand one decode is asked for comes from the
  same clip list the frames are built from (`demand_of`), so a decode and the
  payload built from it cannot disagree about whether the alpha plane is there.
  the frames of a call are allocated before any of them is filled, which is what
  lets a decode that hands each row to the frame it belongs in fill all of them
  in one pass; see [`decoder::RowStream`].
- `src/decoder.rs`: image probing and lazy decoding. `Demand` is what a decode has
  to produce, and `probe` reads an embedded ICC profile either way — whether a
  file has one is the `ImgSeqHasICC` fact — but keeps its bytes only when the
  caller asked to export them, because a sequence whose files each carry a large
  profile would otherwise hold one copy per file for the life of the clip.
  [`Input`] is the one open a probe makes: it holds the file and the leading
  bytes read from it, `head` grows that window from where it stops, and `reader`
  hands the same handle out rewound to the front for the container walk that
  finds a timeline, so the route, the module that describes the file and the
  animation adapters all read one open. a module that reads past the window takes
  the same handle -- which every arm of `describe` now takes, including the ones
  that read past the window. two opens remain and cannot be shared, because
  `libheif` and the `exr` crate each open the path themselves.
  [`Pixels`] is what a buffered decode produced, and `RowStream` is the decode
  that has not read its picture yet because it can write each row into the frame;
  a format answers with one only when it can fill every frame of the call.
- `src/formats/`: one module per container, chosen by `identify.rs` rather than
  by the file's name (`heif.rs`
  for monochrome heif/heic and for the colour pages of both containers, which are
  decoded through libheif's own yuv planes and handed out as they are, `avif.rs`
  for every colour avif, which `dav1d` decodes and which also answers
  `decoder::probe` from the container boxes and the av1 sequence header so that
  probing an avif does not decode it, `jxl.rs` for every jpeg xl, which the `jxl`
  crate decodes directly, `jp2.rs` for JPEG 2000 header probing and OpenJPEG
  decoding, `png.rs` for the `cICP` chunk and for the png files whose rows it
  walks straight into the frame instead of buffering the picture whole — which
  includes expanding a palette page's indices itself — and
  `bmp.rs` for Windows bitmaps, whose palette, run-length and bitfield paths are a
  port of the `image` reader and whose alpha rule is the format's rather than the
  obvious one -- and which reads both of the format's headers, the twelve byte
  `BITMAPCOREHEADER` with its three byte palette entries as well as the
  information header, `ico.rs` for Windows icons, which picks a directory entry and hands
  its payload to `bmp.rs` or to the `png` crate,
  `tga.rs` for Truevision Targa, whose eleven image types, three run-length forms
  and two descriptor directions are a port of the `image` reader, and whose
  thirty-two bit rule is the opposite of the bitmap one's, `dds.rs` for
  DirectDraw surfaces, whose DXT1, DXT3 and DXT5 blocks and their DX10
  equivalents are a port, and whose five and six bit channels widen by
  truncating division where the bitmap and targa readers round to nearest,
  `hdr.rs` for Radiance pictures, whose three scanline encodings and eight
  resolution spellings are written here rather than ported, because the reader
  this replaces accepted only one spelling of the resolution line,
  `tiff.rs` for the tagged format, whose decompressors are the crate's default
  features and whose planar files arrive as planes that have to be reordered into
  a frame,
  `pnm.rs` for the netpbm family, whose seven subtypes, ASCII and binary rasters
  and MAXVAL rescale are a port,
  `exr.rs` for OpenEXR, read through the `exr` crate, whose channels are
  selected by name rather than position, whose header is read without the
  picture, and whose named channels go into one plane-major buffer handed to
  the frame with its strides attached rather than an interleaved picture,
  `jpeg.rs` for every jpeg, whose headers one `zune-jpeg` pass answers without
  the pixels and whose picture is decoded from the file read whole, because a
  probe over a stream stops where the raster begins -- creating a clip over a
  35 page corpus reads headers rather than 213 MB of pictures, and went from
  105 ms to 3 ms,
  `qoi.rs` for the quite
  ok image, whose fourteen byte header is read without a
  sample and whose decoder is the `qoi` crate's, `farbfeld.rs` for the format
  that is a magic and a size and nothing else, whose samples are big-endian on
  disk and native in the frame, read one row at a time straight into the frame,
  and `webp.rs` for the still webp decode, which the `wpd` decoder does
  (`wpd` is a git dependency pinned to an exact revision in `Cargo.toml`, and
  it is created inside the one fill that reads the file because it is not
  `Send`), and for the lossy yuv format; an animated webp's rectangle is the
  same decode, which is what `animation/webp.rs` calls). An avif this tree's
  walk
  decodes itself is the yuv
  its container states, and everything else is `heif.rs`'s: an r,g,b container,
  a monochrome one, and one the walk refuses. both readers name the same
  library the probe did. an avif alpha item is a coded item of its own and a
  heif alpha plane is a buffer this module packs, so both readers take the
  `decoder::Demand` of the call and read only what a clip actually hands out: a
  call that hands out no alpha clip must not decode one, and a webp or a jxl is
  not told because its alpha arrives inside the buffer its decoder wrote. an avif
  item is decoded at low latency, because one
  item is one frame, so an item that holds no picture ends the frame request with
  a path-qualified error instead of a decode loop that never returns; do not put
  that loop's exit behind an iteration count or a sleep, and do not call
  `dav1d_flush` to drain it, because it discards the delayed frame rather than
  handing it over. `avif.rs` also holds the ISO base media file format item
  metadata walker both containers share, which `heif.rs` reads the container's
  `irot`/`imir` orientation through because the `libheif-rs` wrapper exposes no
  getter for it; a second parser is what that avoids, not what it adds. every
  range that walker hands out is bounded by the file, or by the `idat` box for a
  construction method of one, before the caller allocates it, and every offset,
  length and field width is checked against the type it is read into: a malformed
  container is refused rather than read past its own bounds, because a release
  build aborts on a panic. `Meta::native_eligible` is what decides whether this
  reader decodes the primary item at all, and an item written as several extents
  is read rather than refused, because those extents are one payload split
  across the container. A container it still refuses — a grid of tiles, a
  construction method it does not follow — is described and decoded by `libheif`
  instead, which reads both.
  `avif.rs` is the one module that decides which library owns an avif, and it
  decides it from the same walk the decode needs for the pixels: a container the
  walk refuses, an r,g,b one and a monochrome one all go to `libheif`, and only
  the yuv its own walk reads is decoded here. The probe asks the same question
  the same way, so the two cannot disagree about which library owns a file; a
  probe must never promise a frame a decode would refuse to produce.
- `src/pixel.rs`: supported pixel formats, the format a nominal depth names, and
  planar frame writes, which move a wider word down to the frame's own depth.
- `src/color.rs`: frame properties, the optional raw `ICCProfile`, and the
  container's color metadata mapped onto `_Primaries`, `_Transfer`, `_Matrix`
  and `_Range`.
- `src/error.rs`: errors returned through the VapourSynth boundary.
- `hatch_build.py`: Cargo build, plugin staging, wheel tagging, and legal-file
  inclusion. it builds one library per x86-64 level — the baseline with no
  `-C target-cpu`, `.avx2` for `x86-64-v3` and `.avx512` for `x86-64-v4` — which
  is the shape VapourSynth's manifest looks for, so `manifest.vs` names the
  stem alone and the core picks the build the host CPU supports; see
  `docs/improvements/23-cpu-variant-avx2.md`. the musl target is x86-64 as
  well, so it carries the same three; every other target gets the one baseline
  library.
- `tests/readalpha.vpy`: the VapourSynth validator, against the fixtures written
  by `tests/make-alpha-fixtures.py`; the heif and avif fixtures are encoded from
  that script's `mono-alpha.png` and `alpha-rgba8.png` with `heif-enc` and
  `avifenc` (`mono-alpha-10.avif` is the 10 bit one), and `alpha-rgba8.jxl` from
  the last with `cjxl`, so a changed source needs them re-encoded by hand. its
  depth section reads `jxl-gray10.jxl`, `jxl-gray12.jxl` and `jxl-rgba10.jxl`,
  encoded with `cjxl -d 0` from the `P5` pgm and the `P7` pam the same script
  writes (`MAXVAL` 1023, 4095 and 1023 with four channels), so those are
  re-encoded by hand the same way. its yuv avif section reads the three crops `avif-yuv420p.avif`, `avif-yuv422p.avif`
  and `avif-yuv444p10.avif` cut out of the `sandbox/hitokage-sample` yuv avifs,
  plus `alpha-yuv420p.avif`, and those four are likewise described by hand in
  that script's header. its split-extent section reads `avif-split-extents.avif`,
  which that script writes by hand from the coded item `avif-yuv420p.avif` holds,
  so that fixture has to exist before the script runs. That section joins nothing
  itself: it checks the plugin's joined planes against the whole file's, plane by
  plane. its grid section reads `avif-grid.avif`, a 2x2 grid of tiles encoded
  once by hand from the `avif-grid-source.png` the same script writes
  (`avifenc --lossless -g 2x2`, with the command in that script's header). A
  grid is refused by the plugin's own walk and read by `libheif`, so that
  section checks the joined picture's four cells -- 8, 10, 15 and 17, taken at
  each cell's middle -- rather than a refusal. its orientation section reads the `tests/fixtures/orientation-*.png` files written by
  `tests/make-orientation-fixtures.py`, plus `orientation-6.jxl`, which that
  script documents and `cjxl` makes, and its yuv orientation section reads the
  `orientation-{2,6,8}.webp` files that script cuts out of
  `orientation-split.webp`, whose bitstream is likewise made once by hand
  (`magick … -quality 90 -define webp:method=4`). its colour section reads the
  `cicp-rgb8.png` written by `tests/make-cicp-fixtures.py` and the
  `cicp-rgb8.avif` encoded from it with `avifenc --lossless --cicp 9/18/0 -r
  full`, which that script documents, beside the `alpha-rgb8.png` that holds the
  same picture and states nothing. its ICC section reads `icc-rgb8.png` and
  `icc-rgba8.png`, generated with the embedded `icc-srgb.icc` profile, and
  checks the opt-in `ICCProfile` property. its animation section reads the
  `animation.{png,gif,webp,jxl,avif,heic}` and `animation-rgba16.png` fixtures
  written by `tests/make-animation-fixtures.py`, which needs Pillow, `cjxl`,
  `avifenc` and `heif-enc` on `PATH`; the gif, apng, webp, jpeg xl and avif
  fixtures all state the same four 80/170/110/240 ms pictures over a 16x12
  canvas, and the heic fixture holds four equal 150 ms samples because `heif-enc`
  accepts one duration for a whole sequence.
- `tests/routing.py`: the phase 1 acceptance check for plan 34. It copies one
  fixture per format under a wrong extension and under an uppercase one and
  requires every copy to decode to the same bytes, alpha and properties as its
  source, so it measures whether a file's *content* decides how it is read rather
  than its name. It reports no wrong copies, which is the state routing through
  `src/formats/identify.rs` reaches and must stay in. It
  needs a release build and the plugin, like `tests/readalpha.vpy`.
- `tests/check-packaging-tools.py`: the checks for `tools/`, run with any Python
  3.12 or later. it builds its own tree under `target/check-packaging-tools`, so
  it needs no wheel and no network; `IMGSEQS_CHECK_TMP` moves that tree, and a
  tree named that way is only cleared when it is empty, so the check cannot
  delete a directory it did not make.
- `tools/`: the release and packaging scripts, which are also what the CI
  workflows call. `create-changelog.py` generates the GitHub release notes and is
  strict by default (the tag, both version files and a non-empty changelog
  section have to agree; `--preview` is the permissive mode).
  `build_output.py` is the one place that empties a build output of the artifacts
  a build writes, and it is the only tool whose name has an underscore, because
  the others import it. `stage-native.py` stages the standalone bundle from the
  final wheel, `package-linux-wheel.py` moves auditwheel's libraries into
  `imageseqs/lib/`, `check-linux-wheel.py` checks the repaired wheel for the
  platform tag and the dependencies that platform's policy lets it bundle,
  `make-linux-source-bundle.sh` writes the relinking source archive both Linux
  builds ship, and `build-manylinux.sh` and `build-musllinux.sh` build and
  repair a wheel in their own pinned container.
- `tools/macos-libheif-toolchain.cmake`: the macOS wheel build disables unused
  embedded libheif codec backends; the plugin decodes HEIC with libde265 and
  AVIF with dav1d directly. `tools/package-macos-wheel.py` bundles and checks
  the macOS runtime dylibs before the final wheel is staged.
- `docs/IMPLEMENTATION.md`: design notes and deferred ideas.
- `docs/improvements/`: one plan per change, with its status; its `README.md` is
  the index of what is open, what the landed work left over and what was decided
  against.
- `docs/HANDOFF.md`: the state of the tree and the first steps for each open
  item.

## local build

the development python executable is `C:\Python314\python.exe`.
`vcpkg_installed/` is the ignored repository-local install. the generated
`target/vcpkg-root/` adapter exposes it in the layout expected by vcpkg-rs.

`wpd` assembles its x86 and x86-64 decode routines with NASM, so `nasm` has to
be on `PATH` for a build on those architectures; its ARM sources are assembled
by the C compiler and need nothing extra. On Windows the archiver `nasm-rs`
calls is MSVC's `lib.exe`, which a normal "x64 Native Tools" prompt already
puts on `PATH`.

an ELF build also gets `-Wl,-Bsymbolic` on the plugin's own link, from
`build.rs`. NASM reaches wpd's gamma tables with a rip-relative load, and
rustc exports every `#[no_mangle]` symbol of the crate graph from a cdylib, so
without it both link editors refuse the link (`relocation R_X86_64_PC32 cannot
be used against symbol ...; recompile with -fPIC`). PE and Mach-O do not need
the flag, and `-Bsymbolic-functions` does not replace it.

from powershell, use the local native paths when running cargo directly:

```powershell
$env:VCPKG_ROOT = (Resolve-Path .\target\vcpkg-root).Path
$env:VCPKGRS_TRIPLET = "x64-windows-static-md"
$env:PKG_CONFIG_PATH = (Resolve-Path .\vcpkg_installed\x64-windows-static-md\lib\pkgconfig).Path

cargo build --release --locked
cargo test --locked
```

refresh the local manifest install with:

```powershell
C:\vcpkg\vcpkg.exe install `
  --triplet x64-windows-static-md `
  --x-manifest-root="$PWD" `
  --x-install-root="$PWD\vcpkg_installed"
```

if the local vcpkg adapter is missing, recreate it before building or use the
repository's established cargo-vcpkg setup. do not commit generated
`vcpkg_installed/` or `target/` contents.

the manifest selects `tools/vcpkg-ports/libheif/`, whose `dav1d` feature builds
libheif's AVIF sequence decoder into the library. the upstream vcpkg port
disables that backend, so installing dav1d alone only fixes the direct still
reader. keep libheif's default features disabled. after replacing the installed
libheif archive, run `cargo clean --release --package libheif-sys` before the
release build so Cargo does not reuse the old native link configuration.

## python packaging

install the development extra and build both artifacts:

```powershell
C:\Python314\python.exe -m pip install ".[dev]"
C:\Python314\python.exe -m build
```
`python -m build` runs the hook with the environment it is given. When
`VCPKG_ROOT` is already set — a machine with vcpkg installed system-wide has
it — the hook keeps that tree instead of the repository-local one, and a build
then fails at `libheif-sys` with "package libheif is not installed for vcpkg
triplet x64-windows-static-md". Set the three variables of the local build
section above before building a wheel on such a machine.

the wheel should contain:

```text
vapoursynth/plugins/imageseqs/vs_imageseqs.dll
vapoursynth/plugins/imageseqs/vs_imageseqs.avx2.dll
vapoursynth/plugins/imageseqs/vs_imageseqs.avx512.dll
vapoursynth/plugins/imageseqs/manifest.vs
LICENSE
THIRD_PARTY_NOTICES
LICENSES/
```

the two suffixed libraries are x86-64 only, one per microarchitecture level,
and the manifest names the stem rather than any of them; a unix wheel spells
the same three `libvs_imageseqs{,.avx2,.avx512}.so`.

it should not contain `python/`, `vs_imageseqs/`, or a python module. the
wheel is platform-specific but does not depend on the python abi, so the
expected windows tag is `py3-none-win_amd64`, and auditwheel writes the Linux
ones: `py3-none-manylinux_2_28_x86_64` or `py3-none-musllinux_1_2_x86_64`.

## validation

after changes, run the narrowest relevant checks:

```powershell
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
.\.venv\Scripts\python.exe tests\readalpha.vpy
C:\Python314\python.exe tests\check-packaging-tools.py
C:\Python314\python.exe -c "import pathlib, tomllib; tomllib.loads(pathlib.Path('pyproject.toml').read_text())"
C:\Python314\python.exe -m build
```

the packaging check is the one to run for anything under `tools/`, the CI
workflows or `pyproject.toml`; it needs no wheel and no network, and it runs on
every push in the `source` job. `python -m build` is not required for a change
that touches neither the wheel contents nor the legal files.

inspect the wheel as a zip archive. confirm the native file is under
`vapoursynth/plugins/imageseqs/`, its manifest and legal files are present, and no python package is
included.

## testing and benching

before doing any changes, make sure you run the validator then make a release build and do the following:
- run `tests/readalpha.vpy` against the release build, and inspect the log for any errors or warnings.
- look at `docs/BENCH.md` and see how to run the benchmarks.
- run the benchmarks and record the results. this will give you a baseline to compare against after your changes.

after everything is done, do the same thing. what you need to make sure is:
- the validator passes with no errors or warnings.
- the benchmarks are run and the results are compared against the baseline.
  the code changes should not introduce any **significant** regressions in speed or memory usage.
  if there is a regression, you need to investigate and fix it before submitting your changes.

## native licenses

Linux release wheels are built twice, in manylinux_2_28 and in musllinux_1_2,
and repaired with auditwheel for that platform. `tools/build-manylinux.sh` and
`tools/build-musllinux.sh` build the native inputs, and
`tools/check-linux-wheel.py` checks the repaired wheel and stages the standalone
bundle. Both Linux layouts include shared dav1d/libde265 with relative loader
paths, and a fresh container validates them before publishing. Each Linux build
includes a relinking source archive; see `docs/LINUX-BUILD.md`.

the current native set is dav1d, libheif, libde265, the `wpd` decoder
the crate builds and links into the plugin, and the OpenJPEG
sources vendored by `openjpeg-sys`. dav1d, OpenJPEG and wpd use the
bsd-2-clause license. libheif and libde265 are lgplv3 and are
statically linked. keep the exact upstream texts in `LICENSES/`.

the musllinux wheel also carries the gcc runtime libraries (`libstdc++`,
`libgcc_s`) the embedded libheif links, because auditwheel's musl policy
promises the host only libc and libz; `LICENSES/gcc-runtime-COPYING.txt` is the
exception they are conveyed under.

on windows the vcpkg `x64-windows-static-md` triplet makes every vcpkg native
library static. `jpeg2k` compiles its vendored OpenJPEG sources on every
platform. every webp is decoded by the `wpd` crate, which the plugin links
and which needs no library found at build time; `build.rs` locates nothing any
more. the `embedded-libheif` feature builds libheif into the plugin,
so libheif is static there as well; dav1d and libde265 are the system shared
libraries. the static lgpl obligation below therefore applies to libheif on
every platform and to libde265 on windows only.

if x265 or another codec is enabled, inspect the actual linker output and add
its license text and distribution obligations before shipping a wheel. for
static lgpl linkage, notices alone are not enough; provide the applicable
corresponding source and relinking path for the exact binary.
