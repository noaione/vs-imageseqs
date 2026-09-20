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
separate gray alpha clip that is opaque for files without an alpha channel.
`fpsnum` and `fpsden` default to `24/1`. `mismatch` defaults to false;
when true, variable dimensions and formats are allowed. `apply_rotation`
defaults to true and hands out the picture the file's exif orientation
describes, including the width and height swap of orientations 5 to 8; when
false the stored picture is handed out at the stored size and
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
- install the native library under `vapoursynth/plugins/`.
- preserve user changes and untracked files.
- keep `vcpkg.json`'s disabled libheif default features unless the dependency
  policy changes. this avoids selecting x265.
- update `THIRD_PARTY_NOTICES` and `LICENSES/` when native dependencies or
  linkage change.

## source layout

- `src/lib.rs`: plugin declaration and registration.
- `src/source.rs`: `Read` and `ReadAlpha` filter creation, validation, and frame requests.
- `src/prefetch.rs`: the lookahead pool, its window, and its byte budget.
- `src/clip.rs`: the clips a sequence hands out and the frames they cache, which
  the lookahead workers build.
- `src/decoder.rs`: image probing and lazy decoding.
- `src/formats/`: per-format paths for what the `image` crate cannot express or
  reports wrongly, one module per container and picked by extension (`heif.rs`
  for monochrome heif/heic and for the colour pages of both containers, which are
  decoded through libheif's own yuv planes and handed out as they are, `avif.rs`
  for every colour avif, which `dav1d` decodes and which also answers
  `decoder::probe` from the container boxes and the av1 sequence header so that
  probing an avif does not decode it, `jxl.rs` for every jpeg xl, which the `jxl`
  crate decodes directly because `image` has no jxl format of its own, `jp2.rs`
  for JPEG 2000 header probing and OpenJPEG decoding, `png.rs` for the `cICP`
  chunk, which `image` has no accessor for, and `webp.rs` for the libwebp decode
  and the lossy yuv format). a monochrome avif still goes through `image` and is
  corrected to `Gray8` here.
- `src/pixel.rs`: supported pixel formats, the format a nominal depth names, and
  planar frame writes, which move a wider word down to the frame's own depth.
- `src/color.rs`: frame properties, the optional raw `ICCProfile`, and the
  container's color metadata mapped onto `_Primaries`, `_Transfer`, `_Matrix`
  and `_Range`.
- `src/error.rs`: errors returned through the VapourSynth boundary.
- `hatch_build.py`: Cargo build, plugin staging, wheel tagging, and legal-file
  inclusion.
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
  that script's header. its orientation section reads the `tests/fixtures/orientation-*.png` files written by
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
  checks the opt-in `ICCProfile` property.
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

## python packaging

install the development extra and build both artifacts:

```powershell
C:\Python314\python.exe -m pip install ".[dev]"
C:\Python314\python.exe -m build
```

the wheel should contain:

```text
vapoursynth/plugins/vs_imageseqs.dll
LICENSE
THIRD_PARTY_NOTICES
LICENSES/
```

it should not contain `python/`, `vs_imageseqs/`, or a python module. the
wheel is platform-specific but does not depend on the python abi, so the
expected windows tag is `py3-none-win_amd64`.

## validation

after changes, run the narrowest relevant checks:

```powershell
cargo test --locked
.\.venv\Scripts\python.exe tests\readalpha.vpy
C:\Python314\python.exe -c "import pathlib, tomllib; tomllib.loads(pathlib.Path('pyproject.toml').read_text())"
C:\Python314\python.exe -m build
```

inspect the wheel as a zip archive. confirm the native file is under
`vapoursynth/plugins/`, the legal files are present, and no python package is
included.

## native licenses

the current native set is dav1d, libheif, libde265, libwebp, and the OpenJPEG
sources vendored by `openjpeg-sys`. dav1d and OpenJPEG use the bsd-2-clause
license and libwebp uses bsd-3-clause. libheif and libde265 are lgplv3 and are
statically linked. keep the exact upstream texts in `LICENSES/`.

on windows the vcpkg `x64-windows-static-md` triplet makes every vcpkg native
library static. `jpeg2k` compiles its vendored OpenJPEG sources on every
platform. on unix `build.rs` links libwebp from its archive when the
development package installs one (`libwebp-dev` and homebrew's `webp` both
do); on apple the archive is named instead of requested, because `ld` ignores
the `static=` hint and prefers `libwebp.dylib` in the directory homebrew puts
both forms in. the `embedded-libheif` feature builds libheif into the plugin,
so libheif is static there as well; dav1d and libde265 are the system shared
libraries. the static lgpl obligation below therefore applies to libheif on
every platform and to libde265 on windows only.

if x265 or another codec is enabled, inspect the actual linker output and add
its license text and distribution obligations before shipping a wheel. for
static lgpl linkage, notices alone are not enough; provide the applicable
corresponding source and relinking path for the exact binary.
