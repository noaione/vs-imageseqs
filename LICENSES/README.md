# License bundle

This directory contains the license texts and notices that accompany the
statically linked native components used by the plugin.

## Native components

These files are copied verbatim from the repository's `vcpkg_installed`
`x64-windows-static-md` package installation:

- `dav1d-COPYING.txt` — dav1d 1.5.3, BSD 2-Clause.
- `libheif-COPYING.txt` — libheif 1.23.1, including its LGPLv3 library text
  and the upstream bundled GPL/MIT license text sections.
- `libde265-COPYING.txt` — libde265 1.1.1, including its LGPLv3 library text
  and the upstream bundled GPL/MIT license text sections.
- `libwebp-COPYING.txt` — libwebp 1.6.0, BSD 3-Clause, including its
  additional IP rights grant for patents.

The current manifest disables libheif default features, so x265 is not part
of the refreshed install or current native dependency set. If HEVC encoding
is enabled later, add x265's exact `COPYING` file and update the notices
before distributing the resulting binary.
