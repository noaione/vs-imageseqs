# 14 — Linux wheel distribution and plugin manifests

Status: implemented; final Linux and Windows layouts validated locally.
macOS and the complete GitHub Actions run remain to be validated in CI.

## problem

The Linux build runs on Ubuntu and assigns a manylinux tag from the host's
supported platform tags. The published `manylinux_2_43_x86_64` wheel still needs
unbundled `libdav1d.so.7` and `libde265.so.0`. Matching glibc alone therefore
does not guarantee that the installed plugin can load. The standalone ZIP
copies only the plugin, so it has the same runtime dependency problem.

## design

- Build all Linux native inputs and the plugin in the pinned
  `manylinux_2_28_x86_64` container, targeting glibc 2.28+.
- Build the existing dav1d 1.5.3, libde265 1.1.1, and libwebp 1.6.0 from
  checksum-verified sources. Keep libwebp static and prevent discovery of
  unrequested libheif codecs. Preserve public plugin/filter identities.
- Hatch produces a generic Linux intermediate wheel. Auditwheel checks the
  baseline, bundles shared dependencies, and assigns the final manylinux tag.
- All platforms install their native library under
  `vapoursynth/plugins/imageseqs/`. That directory also contains `manifest.vs`,
  whose first line is `[VapourSynth Manifest V1]` and whose second line is the
  platform's plugin filename without its extension. Only the plugin is listed;
  dependency libraries must not be treated as VapourSynth plugins.
- Keep the Linux dependency libraries inside the same `imageseqs/` directory
  tree in `lib/`, with the plugin using `$ORIGIN/lib`. Repack with `wheel` to
  regenerate RECORD hashes, then re-audit. Wheel and standalone ZIP installations
  preserve that whole directory. Legal files accompany both artifacts.
- Publish only repaired wheels. Gate both PyPI and GitHub Releases on the
  reusable Rust tests and validation of the final Linux distribution.
- Ship a Linux relinking archive containing application source, locked and
  vendored Cargo sources, exact native source archives, and rebuild instructions.

## validation

- Verify wheel metadata, plugin/manifest placement, bundled libraries, legal
  files, and the absence of a Python helper package.
- Re-audit the final wheel after any layout changes.
- Install into a fresh baseline container with VapourSynth R79 and no codec
  build dependencies. Test actual manifest autoloading and all fixture checks.
- Test the standalone bundle after relocation, without `LD_LIBRARY_PATH`.
- Build and inspect the Windows wheel and test its manifest. macOS packaging
  follows the same Hatch path; the macOS binary is validated by CI.
- Check workflow syntax and ensure failed validation blocks publishing.

Verified locally on 2026-09-27:

- Built the final Linux wheel in the pinned manylinux container. Auditwheel
  accepted `manylinux_2_28_x86_64` before and after moving the codec libraries.
  The checker verified the final layout, manifest, legal files and RECORD hashes.
- In a fresh baseline container with VapourSynth R79 and no build prefix,
  manifest autoloading decoded a frame. Both the installed wheel and relocated
  standalone bundle passed all 328 fixture checks without `LD_LIBRARY_PATH`.
- Generated `linux-relink-source.tar.gz` with the exact native archives and
  vendored Cargo dependencies. A full rebuild from that archive is not yet tested.
- Built the Windows source distribution and wheel with explicit repository-local
  vcpkg paths. An isolated source build cannot discover the checkout's adapter
  automatically; set the documented native environment variables first.
- In a separate Windows environment with VapourSynth R79, the installed wheel
  passed manifest autoloading and frame decoding. Its standalone bundle passed
  all 328 fixture checks. The wheel remains `py3-none-win_amd64`.
- Python/TOML syntax and workflow lint passed. The local actionlint runner list
  predates `ubuntu-26.04`, so only that existing label warning was suppressed.

The macOS build and the complete GitHub Actions run remain CI validation items.
No decoder code changed and no performance benchmark was needed.

## remaining work

- Observe the first complete GitHub Actions run, including macOS autoloading.
- Exercise a full rebuild from the generated Linux relinking archive.
- macOS still needs its existing system dav1d/libde265 installation; bundling
  those libraries is separate work. Windows static-LGPL source/relinking
  obligations also remain as documented in `THIRD_PARTY_NOTICES`.

Build/relink instructions: [LINUX-BUILD.md](../LINUX-BUILD.md).
Upstream references: [manylinux](https://github.com/pypa/manylinux),
[auditwheel](https://github.com/pypa/auditwheel), and
[VapourSynth manifests](https://www.vapoursynth.com/2026/04/30/r75-sanding-of-the-r74-edges-and-plugin-manifests/).
