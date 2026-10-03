# libheif overlay

The port and its patches are copied from Microsoft's vcpkg commit
`40a9bd4ccdf5dc14ff76d4ed47d46a226ce84a83`, the baseline in the root manifest:
[upstream port](https://github.com/microsoft/vcpkg/tree/40a9bd4ccdf5dc14ff76d4ed47d46a226ce84a83/ports/libheif).
The build recipes retain Microsoft's MIT license in
[vcpkg-LICENSE.txt](https://github.com/noaione/vs-imageseqs/blob/master/LICENSES/vcpkg-LICENSE.txt).

The upstream port disables dav1d and offers no feature to enable it. Still AVIF
uses dav1d directly in the plugin, but AVIF sequence tracks are decoded by
libheif and need an AV1 backend there as well.

This overlay adds a `dav1d` feature, declares that dependency before building
libheif, and sets `WITH_DAV1D_PLUGIN=OFF` so the decoder is built into the
library. The root manifest requests it while keeping libheif's default features
disabled. No AOM or x265 backend is enabled. The source version and patches are
otherwise those of the pinned port.

The root manifest's `configuration.overlay-ports` selects this recipe for both
local installs and CI. Its files are included in the source distribution, and
the Windows dependency cache key includes them. Refresh the local native install
with the command in `AGENTS.md`, then rebuild Cargo's libheif consumer because
Cargo does not track changes to an already installed native archive.
