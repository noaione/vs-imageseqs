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
when true, variable dimensions and formats are allowed. `debug` defaults to
false and emits VapourSynth log timings when enabled. `prefetch` selects the
number of background decode workers used for sequential reads; it defaults to
half the logical cores (capped at four) and `0` disables lookahead decoding.
`prefetch_memory` is the lookahead budget in MiB; it defaults to the larger of
192 MiB and one frame of the largest image per worker, and `0` is rejected
because `prefetch=0` is how lookahead is disabled.

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
- `src/decoder.rs`: image probing and lazy decoding.
- `src/formats/`: per-format decode paths that the `image` crate cannot express,
  one module per container and picked by extension (`heif.rs` for monochrome
  heif/heic).
- `src/pixel.rs`: supported pixel formats and planar frame writes.
- `src/color.rs`: frame properties and color metadata.
- `src/error.rs`: errors returned through the VapourSynth boundary.
- `hatch_build.py`: Cargo build, plugin staging, wheel tagging, and legal-file
  inclusion.
- `docs/IMPLEMENTATION.md`: design notes and deferred ideas.

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

the current native set is dav1d, libheif, libde265, and libwebp. dav1d uses
the bsd-2-clause license and libwebp uses bsd-3-clause. libheif and libde265
are lgplv3 and are statically linked. keep the exact upstream texts in
`LICENSES/`.

on windows the vcpkg `x64-windows-static-md` triplet makes every native
library static. on unix `build.rs` links libwebp from its archive when the
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
