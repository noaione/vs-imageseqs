# Linux distribution builds

Linux release wheels are built twice, once per C library, in that platform's
pypa image: `quay.io/pypa/manylinux_2_28_x86_64` gives the glibc wheel,
repaired by auditwheel for `manylinux_2_28_x86_64` (glibc 2.28 and later), and
`quay.io/pypa/musllinux_1_2_x86_64` gives the musl one, repaired for
`musllinux_1_2_x86_64` (musl 1.2 and later, [PEP 656](https://peps.python.org/pep-0656/)).
The runner's Ubuntu version does not set the binary's compatibility baseline.
The ordinary Hatch build produces an intermediate `linux_x86_64` wheel; only
the repaired wheels are uploaded to PyPI or GitHub Releases. A glibc host
cannot load the musl wheel and the other way round, which is why both exist.

`tools/build-manylinux.sh` and `tools/build-musllinux.sh` each build dav1d
1.5.3 and libde265 1.1.1 as shared libraries inside their own container. Their
archive versions and SHA-512 hashes are pinned in both. Cargo.lock selects the
embedded libheif and OpenJPEG sources, the webp decoder is the `wpd` crate's
own assembler output, and the same CMake toolchain file disables
discovery of extra libheif codecs, including x265, on either platform. The musl
build adds what Alpine calls its toolchain: `nasm` and `pkgconf` from `apk`,
cmake, meson and ninja from PyPI, and the Rust toolchain the script installs
with rustup because the image carries none. It builds the plugin for
`x86_64-unknown-linux-musl` with `-C target-feature=-crt-static`, because a
cdylib that linked musl statically would carry a second libc, its allocator
and its thread-local storage into the process that loaded it.

CI prepares the build environment before restoring caches. Each platform
caches its installed native prefix and checked source archives separately from
Cargo's compiled dependencies. An exact native cache hit skips codec builds
only when its completion stamp and required installation files are present.
Compiler, container and native build-option changes invalidate that cache.
Wheel assembly, repair, source-bundle generation and clean-container validation
still run every time. The wheel jobs pin Rust to 1.99.0 through the workflow's
top-level `IMGSEQS_RUST_TOOLCHAIN` environment variable; musllinux also caches
that toolchain, and both jobs cache Python downloads. Direct invocations of
either build script still prepare their own environment. See
[37](improvements/37-linux-build-caching.md) for the cache boundaries and CI
timing checks.

Auditwheel checks the requested ABI baseline and bundles the non-system shared
libraries with rewritten names. What counts as a system library is the
platform's policy, and the two differ: on glibc the C++ runtime the embedded
libheif needs is part of the policy, and on musl only libc and libz are, so
auditwheel bundles libstdc++ and libgcc_s into the musl wheel rather than
leave the plugin needing one Alpine does not install by default.
`tools/package-linux-wheel.py` moves whatever was bundled into
`vapoursynth/plugins/imageseqs/lib/`, sets the plugin loader path to
`$ORIGIN/lib`, and repacks with regenerated RECORD hashes. The final wheel is
audited again. `tools/check-linux-wheel.py` verifies the tag the build was
given (`--tag`), the manifest, the layout, the codec libraries, the legal
files, and the hashes, while `--allow-library` names what that platform's
policy lets the build bundle: a dependency nobody expected still fails.

All platforms put the plugin and `manifest.vs` under
`vapoursynth/plugins/imageseqs/`. The manifest lists `libvs_imageseqs` on
Linux/macOS or `vs_imageseqs` on Windows; VapourSynth appends the extension.
The standalone ZIP is extracted from the final wheel, preserving the
`imageseqs/` subtree and root legal files. Copy the whole `imageseqs/` directory
into VapourSynth's plugins directory, including its manifest and any `lib/`
subdirectory. Installing the wheel handles this automatically. The macOS
release uses the same `imageseqs/lib/` layout for its bundled dylibs.

## Validation and publication

The build workflow runs both published layouts in a **fresh** container of the
platform that built them, manylinux or musllinux. Each installs VapourSynth
R79 — which publishes wheels for both, so the musl host is validated against
the core it will load — loads each plugin without `LD_LIBRARY_PATH`, and runs
`tests/readalpha.vpy` against it. No codec development packages or native
build prefix are present in either container.
`tests/check-autoload.py` exercises the installed manifest with autoloading
enabled. The full fixture validator disables autoloading to ensure it loads
each specified binary.
Both publishing jobs depend on this validation and the reusable Rust test
workflow. A container build or validation failure prevents publication.

Each build output is emptied of the artifacts a build writes before it writes
them, so a second run in the same checkout cannot hand a checker two wheels:
`tools/build_output.py` does that for `dist/`, the two wheel directories under
`target/manylinux/` or `target/musllinux/`, and the staged bundle, and
`tools/make-linux-source-bundle.sh` for the relink bundle it writes. The
release notes are generated by
`tools/create-changelog.py`, which is strict by default — the tag, the project
and crate versions and a non-empty `CHANGELOG.md` section have to agree — so a
release cannot be published with placeholder notes. `tests/check-packaging-tools.py`
covers both and runs in the `source` job.

## Rebuilding and relinking
Each Linux build also includes a relinking source archive,
`linux-relink-source.tar.gz` for the manylinux wheel and
`linux-musl-relink-source.tar.gz` for the musllinux one, containing the
application source and build scripts, the three checked native source archives,
all Cargo dependencies (including vendored libheif and OpenJPEG), Cargo.lock,
and the build tool versions. The source tree's `.cargo/config.toml` selects the
included Rust dependencies instead of downloading them from crates.io.

To rebuild, unpack that platform's bundle and use the container it was built in.
Install Rust (see `BUILD-ENVIRONMENT.txt` for the recorded compiler version) and
NASM, then run the script the bundle names from the unpacked directory:

```sh
export IMGSEQS_NATIVE_ARCHIVES="$PWD/native-archives"
sh tools/build-manylinux.sh   # in quay.io/pypa/manylinux_2_28_x86_64
sh tools/build-musllinux.sh   # in quay.io/pypa/musllinux_1_2_x86_64
```

The scripts install their Python build tools and produce `dist/*.whl` and
`native/`; the musl one installs its Rust toolchain as well. To relink with a
modified libheif, edit its sources under the vendored `libheif-sys` directory
and update Cargo's vendor checksum metadata for the changed files, or use a
local path override. To rebuild a modified libde265, update its input archive
and the corresponding checksum in the build script. The same commands then
rebuild and repair the plugin with that library. The standalone bundle's shared
libde265 can also be replaced by an ABI-compatible build retaining the bundled
filename and SONAME.

The container images are pinned by digest. Python build-tool versions may
the recorded build environment identifies the versions used for each release.
This workflow targets binary compatibility, not byte-for-byte reproducibility.

References: [manylinux](https://github.com/pypa/manylinux),
[musllinux](https://github.com/pypa/manylinux#musllinux),
[auditwheel](https://github.com/pypa/auditwheel).
