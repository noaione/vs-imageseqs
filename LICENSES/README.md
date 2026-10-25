# License bundle

This directory contains the license texts and notices that accompany the
native components used by the plugin, whether linked statically or bundled as
shared libraries. Linux and macOS release wheels and plugin ZIPs bundle
libheif 1.23.5, dav1d 1.5.4, and libde265 1.1.1 as shared libraries. The
musllinux wheel also bundles the C++ runtime shared libheif needs, which the
musl auditwheel policy does not promise the host provides. The accompanying
`linux-relink-source.tar.gz` and `linux-musl-relink-source.tar.gz` provide the
exact source inputs and rebuild instructions for the artifacts of each Linux
build.

## Native components

These files are copied verbatim from the dependency sources used by the build:

- `dav1d-COPYING.txt` — dav1d 1.5.4, BSD 2-Clause.
- `libheif-COPYING.txt` — libheif 1.23.5, including its LGPLv3 library text
  and the upstream bundled GPL/MIT license text sections.
- `libde265-COPYING.txt` — libde265 1.1.1, including its LGPLv3 library text
  and the upstream bundled GPL/MIT license text sections.

- `wpd-COPYING.txt` — wpd, pinned to the revision in `Cargo.toml`, BSD 2-Clause.
  It is a Rust crate rather than a system library, but its x86-64 decode
  routines are hand-written NASM sources the crate builds and links into the
  plugin, so its text travels with the binary the same way the others do.
- `gcc-runtime-COPYING.txt` — the FSF's GCC Runtime Library Exception, version
  3.1, the additional permission libstdc++ and libgcc_s are governed by. It is
  the `COPYING.RUNTIME` a GCC installation ships: that runtime is part of the
  toolchain rather than one of the pinned inputs above, and the musllinux image
  provides no license file for its copy of it.

## Ported source code

Some of the still readers under `src/formats/` are ports of the matching reader
in the `image` crate 0.25.10 rather than calls into a library, and `image`
carries its license once for the whole crate rather than in a header on each
file. These are those texts, verbatim:

- `image-LICENSE-APACHE.txt` — the Apache-2.0 text, the other of the two.
- `image-LICENSE-MIT.txt` — the MIT text, one of the two the crate is licensed
  under. `THIRD_PARTY_NOTICES` names the ported files and the upstream modules
  they came from.

The current manifest disables libheif default features, so x265 is not part
of the refreshed install or current native dependency set. If HEVC encoding
is enabled later, add x265's exact `COPYING` file and update the notices
before distributing the resulting binary.

The Windows static libheif build also uses the existing dav1d decoder for AVIF
sequences, built into libheif rather than loaded as a separate codec plugin.
The native license texts above cover that backend. `vcpkg-LICENSE.txt`
preserves Microsoft's MIT license for the repository-local libheif port's
build recipes and patches, originally copied from vcpkg commit
`40a9bd4ccdf5dc14ff76d4ed47d46a226ce84a83`.
