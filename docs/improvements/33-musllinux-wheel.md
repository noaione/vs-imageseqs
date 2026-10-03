# 33 - a musllinux wheel

status: written and locally checked, 2026-10-03. The packaging suite passes 132
checks, the workflow parses with both new jobs wired into publication, and every
platform fact below was read from the artifact or policy it names. Nothing has
run in the container: this machine has no musl host and its Docker daemon cannot
be reached, so the first push that runs the musllinux job is the first proof of
the toolchain, the wheel and its own validation.

## why

the manylinux wheel cannot load on a musl host. musl is a different C library,
not a different version of the same one, so a wheel for it is built against musl
and tagged for the musl version it targets — `musllinux_1_2`, which
[PEP 656](https://peps.python.org/pep-0656/) defines. A host that installs
VapourSynth from PyPI there gets the core and not the plugin, and the plugin is
the whole distribution.

VapourSynth R79 does publish one: `vapoursynth-79-cp312-abi3-musllinux_1_2_x86_64.whl`
is on PyPI, so a musl host can load a plugin at all, and the validation job can
install the core the wheel will actually be loaded by. That is the same
arrangement the manylinux job already relies on.

## what the platform decides

Three things differ from the glibc build, and each one was read rather than
guessed.

**the tag and the policy.** auditwheel names the tag, and each policy states
which libraries it promises the host. Read from auditwheel 6.8.2's
`manylinux-policy.json` and `musllinux-policy.json`:

| policy | `lib_whitelist` |
| --- | --- |
| `manylinux_2_28` | 24 entries, including `libstdc++.so.6` and `libgcc_s.so.1` |
| `musllinux_1_2` | `libc.so`, `libz.so.1` |

The plugin links the C++ runtime: the published
`vapoursynth_imageseqs-0.2.1-py3-none-manylinux_2_28_x86_64.whl` has
`libstdc++.so.6` and `libgcc_s.so.1` among its `NEEDED` entries, because the
embedded libheif is C++. On glibc the policy lets the host supply them. On musl
it does not, so auditwheel bundles them into the wheel — beside the `libdav1d-`
and `libde265-` copies it bundles on both platforms. That is the intended shape
there, not a side effect: VapourSynth's own musllinux wheel carries
`libstdc++-5d72f927.so.6.0.33` and `libgcc_s-0cd532bd.so.1` in
`vapoursynth.libs/`, and scipy's carries the same pair beside libgfortran and
libquadmath. A user whose musl host has VapourSynth from PyPI and no system
`libstdc++` is exactly the user this wheel has to work for, so the runtime is
bundled rather than left to the host.

**static or dynamic musl.** rustc links musl statically for
`x86_64-unknown-linux-musl` by default. A cdylib built that way would carry a
second libc — its allocator and its thread-local storage — into a process that
already has one, which is what the `-C target-feature=-crt-static` builds in the
Rust forum's "statically compile (musl) a dynamic library" thread are about, and
what every musllinux Rust wheel avoids: the maturin-built `orjson` wheel's
extension names `libc.musl-x86_64.so.1` and nothing else, and scipy's musl
extensions name the same. The build script passes that flag, and the plugin is
one more shared object against the host's musl.

**the toolchain.** the pypa musllinux image is Alpine and carries no Rust; a
glibc `rustup-init` cannot run on it. `tools/build-musllinux.sh` installs the
musl one with rustup's own installer when `cargo` is missing, adds the musl
target if the host is not already musl, and takes the rest from what is already
reachable: `nasm` and `pkgconf` from `apk`, and cmake, meson and ninja from
PyPI, where cmake 3.29.5 and ninja 1.13.2 both have musllinux wheels and meson
is pure Python. auditwheel 6.8.2 does not depend on a `patchelf` binary (it
patches ELF itself), so nothing else is needed.

## the build

`tools/build-musllinux.sh` is `tools/build-manylinux.sh`'s counterpart: the same
three pinned archives with the same SHA-512 hashes, the same CMake options, the
same checker. What it names differently is the platform — the target triple, the
auditwheel `--plat`, the output directories under `target/musllinux/`, and the
runtime the wheel is allowed to bundle. The digest check is done with Python
rather than `sha512sum`, because the image's `sha512sum` is busybox's, which
does not read a checksum list from standard input, and because a silent
non-check is worse than no check.

The script is POSIX `sh` and the two CI jobs name the image's own shell, so
neither depends on which shell the Alpine image happens to install.

`hatch_build.py` needed no change: `is_x86_64` answers for
`x86_64-unknown-linux-musl` too, so the wheel carries the baseline library
beside the `.avx2` and `.avx512` ones and the manifest keeps naming the stem.

Around it:

- `tools/make-linux-source-bundle.sh` is the relinking source archive both
  builds ship, extracted from the tail of `build-manylinux.sh` so the two
  platforms cannot drift. Each build writes its own:
  `linux-musl-relink-source.tar.gz` for this one.
- `tools/check-linux-wheel.py` takes `--tag` and a repeatable
  `--allow-library`, and requires `LICENSES/gcc-runtime-COPYING.txt` in every
  Linux wheel. A dependency the build did not name still fails validation, which
  is the point of making the allowance explicit.
- `.github/workflows/build.yml` gains `linux-musl-wheel` and
  `linux-musl-validation`, both pinned to the image digest, and both publishing
  jobs wait for the second. The release job's asset list gains
  `linux-musl-x86_64-plugin.zip`, and `tools/create-changelog.py` names the two
  musl files in the release-notes table.
- `THIRD_PARTY_NOTICES` and `LICENSES/README.md` record the GCC runtime
  libraries and the exception they are conveyed under; the text is the
  `COPYING.RUNTIME` a GCC installation ships.

## validation

What this machine can check, and does:

- `tests/check-packaging-tools.py` passes 132 checks, 7 of them new. Six are in
  the Linux wheel checker: the manylinux wheel under its own tag, the same wheel
  refused under the musl one, a musl wheel carrying an undeclared runtime
  refused by name, the same wheel accepted once both names are given, a missing
  codec refused, and a wheel without the runtime exception text refused. The
  seventh is the release-notes check that both relink archives are named.
- the workflow parses as YAML, and the two jobs, their `needs`, their container
  digests, the release platform list and the sdist input list were read back
  from the parsed document.
- every Python tool compiles.

What it cannot check: the shell scripts have no syntax check here. MSYS2's
`bash -n` aborts in this sandbox with `NtCreateDirectoryObject ... 0xC0000022`,
so the scripts are reviewed by eye against their manylinux originals instead. A
container run is the first real test, and the things most likely to fail first
are, in order: `apk add nasm pkgconf` on the image's repositories; rustup's
installer on Alpine; and whether auditwheel bundles anything the checker was not
told about, which its own message names.

## what this page does not decide

- **aarch64.** both jobs are x86_64, like the manylinux pair. The image for the
  other architecture is `musllinux_1_2_aarch64`, and `is_x86_64` already answers
  correctly for it, so the wheel would carry one baseline library.
- **windows arm64.** asked for beside this and skipped: PyPI has no `win_arm64`
  VapourSynth wheel (`win-amd64` and the musl one both exist, `win_arm64` names
  nothing), so a wheel could be built and never loaded, let alone validated.
- **whether the CPU variants help on musl.** the flags are the same and the
  selection is VapourSynth's, but nothing here measured them: [23](23-cpu-variant-avx2.md)
  and [24](24-cpu-variant-avx512.md) measured on Windows.
- **peak memory and throughput of the musl build.** the plugin's own code is the
  same; a µs-level difference between two libc allocators would need a musl host
  to see, and none of the measurements in [BENCH.md](../BENCH.md) apply to one.
