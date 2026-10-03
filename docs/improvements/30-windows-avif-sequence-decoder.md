# 30 - Windows AVIF sequence decoder

status: implemented and validated locally on Windows, 2026-10-03. This is the
separate `animation.avif` repair requested before the research-only image-rs
removal goal in [26](26-remove-image-rs.md). No Rust decoder or timeline code was
changed, and image-rs remains in use.

## cause and repair

The release validator failed when `tests/fixtures/animation.avif` reached
libheif's track decoder, with
`PluginLoadingError(NoMatchingDecoderInstalled)`. Still AVIF uses dav1d directly
in `src/formats/avif.rs`, but sequences use libheif's `decode_next_image` in
`src/animation/heif.rs`. Having dav1d linked for the still reader does not give
libheif an AV1 backend.

The pinned vcpkg libheif 1.23.1 port forces `WITH_DAV1D=OFF` and offers no dav1d
feature. The project's disabled default features also leave AOM disabled, so
libheif had no AV1 decoder. The baseline native CMake cache confirmed dav1d and
AOM were both off. Enabling all defaults would select x265 and does not address
the missing dav1d feature.

`tools/vcpkg-ports/libheif/` copies the official port and its patches from the
manifest's exact vcpkg baseline,
[40a9bd4ccdf5dc14ff76d4ed47d46a226ce84a83](https://github.com/microsoft/vcpkg/tree/40a9bd4ccdf5dc14ff76d4ed47d46a226ce84a83/ports/libheif).
It adds a dav1d feature and dependency, maps that feature to `WITH_DAV1D` and its
package-discovery switch, and sets `WITH_DAV1D_PLUGIN=OFF`. This embeds the
decoder instead of requiring an external codec plugin in the wheel. The five
copied patch/build-support files are byte-identical to upstream.

The root manifest selects the overlay and requests only that feature while
keeping `default-features: false`. The rebuilt cache confirms dav1d on, its
plugin mode off, AOM off and x265 off. Cargo's native link output lists heif,
dav1d and libde265. Inspection of the release DLL's imports found only Windows
and C/C++ runtime libraries, with no external codec DLL requirement.

The source distribution includes the complete overlay. The Windows dependency
cache key hashes it as well as the root manifest, and CI checks that the source
archive contains its manifest and recipe. The installed-wheel smoke test now
decodes all 14 color and alpha outputs of `animation.avif` and checks its four
distinct presentations. Its autoload assertion accepts the baseline, AVX2 or
AVX512 library from the installed manifest directory: this host correctly
selects AVX512, which the old baseline-only assertion rejected.

`CHANGELOG.md`, `THIRD_PARTY_NOTICES` and `LICENSES/README.md` record the change.
The existing dav1d license covers this backend; no new native codec or version
was added. Microsoft's exact MIT license is retained in
`LICENSES/vcpkg-LICENSE.txt` for the copied port inputs. Existing static LGPL
source and relinking obligations still apply. `.gitattributes` exempts those
upstream patch files from whitespace checking because their blank context
lines require a single space.

## local rebuild

The local native install was refreshed with the repository's established vcpkg
command. After replacing its archive, Cargo's libheif consumer must be rebuilt:

```powershell
C:\vcpkg\vcpkg.exe install `
  --triplet x64-windows-static-md `
  --x-manifest-root="$PWD" `
  --x-install-root="$PWD\vcpkg_installed"

$env:VCPKG_ROOT = (Resolve-Path .\target\vcpkg-root).Path
$env:VCPKGRS_TRIPLET = 'x64-windows-static-md'
$env:PKG_CONFIG_PATH = (Resolve-Path .\vcpkg_installed\x64-windows-static-md\lib\pkgconfig).Path
cargo clean --release --package libheif-sys
cargo build --release --locked
```

Cargo does not notice an already installed native archive being replaced.
Cleaning that consumer avoids retaining the old native link configuration.

## validation

Before edits, the existing validator reproduced the missing decoder, a release
build passed, and the release validator reproduced it again. The old DLL was
saved as `target/animation-avif-baseline.dll` before refreshing dependencies.

After the repair:

- The full release validator passes all 580 checks, including AVIF playback,
  timeline sampling and shuffled reads, with no warning or failure lines.
- All 177 Rust tests pass. An initial test run hit an unrelated temporary-folder
  permission error; rerunning with `TEMP` and `TMP` in the repository's ignored
  `target/animation-avif-tmp` passes.
- Clippy with warnings denied, Rust formatting, Python AST, manifest JSON,
  project TOML, workflow YAML and `git diff HEAD --check` pass.
- `tests/check-packaging-tools.py` passes.
- `python -m build` produces the source archive and a `py3-none-win_amd64`
  wheel. The archive contains every overlay input and the MIT license. The wheel
  has all three DLLs, the stem-only manifest and legal files, and no Python
  package.
- That wheel was installed in a fresh isolated environment. Manifest autoload
  and AVIF color/alpha playback pass. The full validator also passes all 580
  checks with each installed DLL loaded explicitly: baseline, AVX2 and AVX512.
- The existing pixel-parity probe produces 244 identical records before and
  after the repair, covering still formats, both output clips and PNG parity
  cases. The broken baseline cannot supply animated AVIF pixels for comparison;
  those are covered by the now-passing validator.

Linux/macOS builds and remote CI were not run in this repair session. Their
embedded libheif builds already retain dav1d; the new installed-wheel animation
check also runs in those CI environments.

## baseline benchmarks

This fixes a missing decoder, not an image-rs performance migration. Animated
AVIF could not decode in the baseline, so a before/after animation speedup
cannot be claimed. Still AVIF, HEIC and PNG are the regression controls. Future
image-rs replacements must take a working baseline and demonstrate an actual
performance improvement against it, as required by [26](26-remove-image-rs.md)
and [BENCH.md](../BENCH.md).

The existing benchmark ran before the dependency change and after the repair,
on all 35 `sandbox/avif` files, three repetitions per configuration:

```powershell
.venv\Scripts\python.exe tests/bench-imgseqs-vs-bestsource.vpy `
  --imgseqs-only --plugin target/animation-avif-baseline.dll `
  --reps 3 --extra --prefetch 16 --dir sandbox/avif `
  --pattern 'snek - p%03d.avif'
```

The after command uses `target/animation-avif-fixed.dll` instead.

| configuration | before frame totals (s) | after frame totals (s) |
| --- | --- | --- |
| default | 3.423, 2.430, 2.648 | 3.950, 2.444, 2.537 |
| `prefetch=0` | 7.179, 5.530, 5.502 | 6.436, 5.533, 5.790 |
| `prefetch=16` | 3.461, 2.697, 2.582 | 3.180, 2.683, 2.633 |

A separate fresh-process probe alternated baseline/candidate order each round,
disabled autoload, read all 35 files, and measured open time, sequential frame
wall time, process CPU, peak working set and peak committed memory. `Read` used
`mismatch=True`, `apply_rotation=False` and the stated prefetch count. The first
study ran three rounds for each cell. Medians below are before → after;
CPU includes open plus frames, and memory is the process peak rather than only
plugin-owned buffers. This Windows host has an i5-11400H with 12 threads.

| corpus | prefetch | frame wall (s) | CPU (s) | peak RSS (MiB) | peak commit (MiB) |
| --- | ---: | --- | --- | --- | --- |
| AVIF | 0 | 6.774 → 8.227 | 18.578 → 20.531 | 698.9 → 699.1 | 710.9 → 710.7 |
| AVIF | 4 | 3.233 → 2.995 | 22.828 → 23.344 | 850.1 → 873.9 | 972.3 → 1000.7 |
| AVIF | 16 | 3.800 → 3.480 | 23.703 → 24.250 | 1107.7 → 1244.8 | 1627.7 → 1799.7 |
| HEIC | 0 | 23.546 → 21.913 | 23.453 → 22.016 | 538.1 → 538.1 | 562.7 → 562.8 |
| HEIC | 4 | 7.631 → 7.430 | 29.656 → 29.906 | 694.8 → 695.2 | 823.0 → 823.5 |
| HEIC | 16 | 4.926 → 5.245 | 42.156 → 42.641 | 1102.8 → 1131.4 | 1653.9 → 1681.8 |
| PNG | 0 | 0.872 → 0.879 | 0.891 → 0.875 | 574.6 → 574.6 | 565.9 → 565.8 |
| PNG | 4 | 0.329 → 0.322 | 1.094 → 1.016 | 650.6 → 631.9 | 642.4 → 623.4 |
| PNG | 16 | 0.257 → 0.268 | 1.453 → 1.375 | 818.6 → 818.4 | 813.7 → 813.8 |

The initial serial AVIF and high-prefetch memory deltas needed investigation.
All three AVIF cells were repeated for five alternating rounds against the
actual wheel's baseline DLL, rebuilt from the same source distribution:

| prefetch | frame wall (s) | CPU (s) | peak RSS (MiB) | peak commit (MiB) |
| ---: | --- | --- | --- | --- |
| 0 | 5.507 → 5.758 | 15.484 → 16.094 | 699.2 → 699.2 | 711.1 → 711.0 |
| 4 | 2.559 → 2.571 | 22.609 → 22.969 | 857.9 → 853.8 | 993.8 → 998.5 |
| 16 | 2.665 → 2.790 | 23.781 → 24.063 | 1123.0 → 1199.4 | 1628.2 → 1627.7 |

The repeat wall/CPU deltas are within overlapping run ranges: serial wall time
is 5.387–6.117 s before and 5.370–6.615 s after, while CPU is 15.031–16.953 s
and 15.109–18.031 s. At the default four workers, median wall time changes
0.5% and peak committed memory 0.5%. At 16 workers, committed memory is
effectively unchanged; median RSS is 6.8% higher, with overlapping ranges
1096.1–1253.1 MiB before and 1073.1–1246.4 MiB after. The earlier large deltas
did not establish a regression beyond the repeated measurements' variation.
Do not hide the RSS caveat or describe this correctness fix as a speedup.
Any significant regression reproduced by future controlled runs must be
investigated and fixed before accepting further decoder changes.

## retained evidence

All logs and scratch probes are under ignored `target/`; none are release
inputs. `animation-avif-{validator-before,validator-release-before,validator-after}.log`
record the reproduction and repair. The Rust, clippy, packaging and wheel logs
use the same prefix. The installed validators are
`animation-avif-wheel-{baseline-validator,avx2-validator,validator}.log`.
The standard benchmark is `animation-avif-bench-{before,after}.log`; the paired
study is `animation-avif-paired-{bench.log,results.json}`, and the repeat is
`animation-avif-repeat-{bench.log,results.json}`. Its scratch runner is
`animation-avif-benchmark.py`.

DLL SHA-256 values identify the actual compared artifacts:

- Original baseline: `e10ed35ee38828cf387eb97ef1039e3b5fc9cd54981ff51c32762abcd91cefdd`.
- First fixed release: `74f9f2c5322621e125d5ee56d239a30d767239f74b22c724a93df6ace5b1e031`.
- Wheel baseline used for the repeat: `a28aefd9fed6f59d5974661403a4c6a6f7bcaa9a3240a2150fc5af2c3dcafae2`.

The fixed baseline DLL is 8,925,184 bytes versus 8,919,552 before: 5,632 bytes
larger. The wheel and source archive are in `target/animation-avif-dist/`.
