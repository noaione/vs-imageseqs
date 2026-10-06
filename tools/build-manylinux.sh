#!/usr/bin/env bash
# Run from the repository root inside pypa/manylinux_2_28_x86_64.
set -euo pipefail

export IMGSEQS_LINUX_PLATFORM=manylinux
. "$PWD/tools/setup-linux-build.sh"

# This script owns the wheels in these three directories, and the checkers below
# require exactly one of them: a second run in the same checkout has to start
# from what this run built rather than from what the last one left behind.
python tools/build_output.py clear --pattern '*.whl' "$work/unrepaired" "$work/repaired" dist

native_cached=false
if [[ "${IMGSEQS_NATIVE_CACHE_HIT:-false}" == true ]] && \
    python tools/linux-native-cache.py check "$prefix" "$IMGSEQS_NATIVE_CACHE_KEY"; then
    native_cached=true
    echo 'reusing cached native codec libraries'
fi

fetch() {
    local name="$1" url="$2" checksum="$3"
    if [[ ! -f "$archives/$name.tar.gz" ]]; then
        curl --fail --location --retry 3 "$url" -o "$archives/$name.tar.gz"
    fi
    printf '%s  %s\n' "$checksum" "$archives/$name.tar.gz" | sha512sum --check
    if [[ "$native_cached" != true ]]; then
        mkdir -p "$work/sources/$name"
        tar -xzf "$archives/$name.tar.gz" --strip-components=1 -C "$work/sources/$name"
    fi
}

# Same upstream versions and archive hashes as the repository's Windows inputs.
fetch dav1d-1.5.3 https://github.com/videolan/dav1d/archive/1.5.3.tar.gz \
    8d976b93135213d41385c20205475269a6826a68ebfd716c4d9a7a3ff2a79703e8df0573e43207c81b5db44807d2721db18ec84c0fc6bef98efab86a2cccb6cc
fetch libde265-1.1.1 https://github.com/strukturag/libde265/archive/v1.1.1.tar.gz \
    fb2207f5a3ba901853f61f345c72130f000134918febbc4f3529c3d289fc79ee7457b3e61660110f698bb4ac15d62426e284034bf870bfbd1859ab3feaa52be8

if [[ "$native_cached" != true ]]; then
    meson_options=()
    if [[ -f "$work/dav1d-build/build.ninja" ]]; then
        meson_options+=(--reconfigure)
    fi
    meson setup "${meson_options[@]}" "$work/dav1d-build" "$work/sources/dav1d-1.5.3" \
        --prefix "$prefix" --libdir lib --buildtype release --default-library shared \
        -Denable_tests=false -Denable_tools=false
    meson compile -C "$work/dav1d-build"
    meson install -C "$work/dav1d-build"

    cmake -S "$work/sources/libde265-1.1.1" -B "$work/de265-build" -G Ninja \
        -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
        -DCMAKE_INSTALL_LIBDIR=lib -DBUILD_SHARED_LIBS=ON -DENABLE_SDL=OFF
    cmake --build "$work/de265-build" --parallel
    cmake --install "$work/de265-build"

    python tools/linux-native-cache.py mark "$prefix" "$IMGSEQS_NATIVE_CACHE_KEY"
fi

python -m build --wheel --outdir "$work/unrepaired"
if readelf -d "$CARGO_TARGET_DIR/release/libvs_imageseqs.so" | grep -E 'NEEDED.*(libwebp|libsharpyuv|libx265)'; then
    echo 'unexpected shared codec dependency' >&2
    exit 1
fi
auditwheel show "$work"/unrepaired/*.whl
auditwheel repair --plat manylinux_2_28_x86_64 --only-plat \
    --wheel-dir "$work/repaired" "$work"/unrepaired/*.whl
python tools/package-linux-wheel.py "$work/repaired" dist
auditwheel show dist/*.whl
python tools/check-linux-wheel.py dist
python tools/stage-native.py dist native

# Include the exact dependency sources and application code needed to rebuild
# and relink the Linux binary, including libheif/OpenJPEG vendored by Rust crates.
sh tools/make-linux-source-bundle.sh linux-relink-source "$work" "$archives"
