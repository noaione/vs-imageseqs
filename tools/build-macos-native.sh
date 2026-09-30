#!/usr/bin/env bash
# Build the two shared codec libraries the macOS release wheel bundles.
set -euo pipefail

if [[ "$(uname -m)" != "arm64" ]]; then
    echo "macOS release dependencies must be built on arm64" >&2
    exit 1
fi

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
work="$PWD/target/macos-native"
prefix="$work/prefix"
temporary=$(mktemp -d "${TMPDIR:-/tmp}/imgseqs-macos-native.XXXXXX")
trap 'rm -rf "$temporary"' EXIT
mkdir -p "$work" "$prefix"

fetch() {
    local name="$1" url="$2" checksum="$3"
    local archive="$temporary/$name.tar.gz"
    curl --fail --location --retry 3 "$url" -o "$archive"
    printf '%s  %s\n' "$checksum" "$archive" | shasum -a 512 --check
    mkdir "$temporary/$name"
    tar -xzf "$archive" --strip-components=1 -C "$temporary/$name"
}

fetch dav1d-1.5.3 https://github.com/videolan/dav1d/archive/1.5.3.tar.gz \
    8d976b93135213d41385c20205475269a6826a68ebfd716c4d9a7a3ff2a79703e8df0573e43207c81b5db44807d2721db18ec84c0fc6bef98efab86a2cccb6cc
meson setup "$temporary/dav1d-build" "$temporary/dav1d-1.5.3" \
    --prefix "$prefix" --libdir lib --buildtype release --default-library shared \
    -Denable_tests=false -Denable_tools=false
meson compile -C "$temporary/dav1d-build"
meson install -C "$temporary/dav1d-build"

fetch libde265-1.1.1 https://github.com/strukturag/libde265/archive/v1.1.1.tar.gz \
    fb2207f5a3ba901853f61f345c72130f000134918febbc4f3529c3d289fc79ee7457b3e61660110f698bb4ac15d62426e284034bf870bfbd1859ab3feaa52be8
cmake -S "$temporary/libde265-1.1.1" -B "$temporary/libde265-build" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_OSX_DEPLOYMENT_TARGET="$MACOSX_DEPLOYMENT_TARGET" \
    -DBUILD_SHARED_LIBS=ON -DENABLE_SDL=OFF
cmake --build "$temporary/libde265-build" --parallel
cmake --install "$temporary/libde265-build"
# CMake's default @rpath install ID is not resolvable from the plugin at build
# time. Use the staged prefix as the link-time ID; Delocate rewrites this path
# to @loader_path/lib/... in the final wheel.
install_name_tool -id "$prefix/lib/libde265.0.dylib" "$prefix/lib/libde265.0.2.1.dylib"

echo "built dav1d and libde265 for macOS $MACOSX_DEPLOYMENT_TARGET in $prefix"
