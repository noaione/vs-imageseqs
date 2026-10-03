#!/bin/sh
# Run from the repository root inside pypa/musllinux_1_2_x86_64.
#
#     sh tools/build-musllinux.sh
#
# The wheel this writes is the musl counterpart of tools/build-manylinux.sh's
# glibc one: same three pinned native inputs, same plugin-only layout, same two
# checkers. What differs is the platform, and so the parts that name it: the
# image's Alpine packages, the Cargo target, the auditwheel policy, and the
# libraries that policy lets auditwheel bundle. docs/LINUX-BUILD.md has both.
#
# The image carries no Rust, and a glibc rustup-init cannot run on it, so this
# installs a toolchain with rustup's own installer when cargo is missing. Every
# other build input is in the image or on PyPI, which is why the workflow has no
# setup step for this job: this script is the whole build.
set -eu

musl_target=x86_64-unknown-linux-musl

export PATH="/opt/python/cp312-cp312/bin:$PATH"
export CARGO_TARGET_DIR="$PWD/target/musllinux/cargo"
work="$PWD/target/musllinux"
prefix="$work/prefix"
archives="${IMGSEQS_NATIVE_ARCHIVES:-$work/archives}"
mkdir -p "$archives" "$work/sources" "$prefix" dist

# This script owns the wheels in these three directories, and the checkers below
# require exactly one of them: a second run in the same checkout has to start
# from what this run built rather than from what the last one left behind.
python tools/build_output.py clear --pattern '*.whl' "$work/unrepaired" "$work/repaired" dist

# dav1d's x86 assembly is assembled by nasm; pkgconf is what libwebp is found
# through, and is what the image's own glib development package already pulls in.
apk add --no-cache nasm pkgconf
python -m pip install build wheel 'cmake>=3.28,<4' meson ninja

if ! command -v cargo >/dev/null 2>&1; then
    echo "no rust toolchain in this image; installing one with rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi
if command -v rustup >/dev/null 2>&1; then
    # A no-op on a musl host, and the standard library this target needs when the
    # image already carried a glibc toolchain.
    rustup target add "$musl_target"
fi

export PKG_CONFIG=pkgconf
# libwebp is found through the prefix this script installed, which is the right
# answer whatever the toolchain's own host is; the crate refuses the query when
# the host and target triples differ unless this says otherwise.
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export CMAKE_PREFIX_PATH="$prefix${CMAKE_PREFIX_PATH:+:$CMAKE_PREFIX_PATH}"
export LD_LIBRARY_PATH="$prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export CMAKE_TOOLCHAIN_FILE="$PWD/tools/manylinux-toolchain.cmake"
export CARGO_BUILD_TARGET="$musl_target"

# The plugin is a cdylib loaded into a process that already runs musl, and rustc
# links musl statically by default: that would put a second libc, its allocator
# and its thread-local storage inside the plugin. Every musllinux wheel links
# the system's musl instead. The C++ runtime the embedded libheif needs is what
# auditwheel bundles here, because the musl policy allows only libc and libz,
# and it is what this wheel's own notices cover.
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=-crt-static"

fetch() {
    source_name=$1
    source_url=$2
    source_checksum=$3
    if [ ! -f "$archives/$source_name.tar.gz" ]; then
        curl --fail --location --retry 3 "$source_url" -o "$archives/$source_name.tar.gz"
    fi
    # This image's sha512sum is busybox's, which does not read a checksum list
    # from standard input the way coreutils' does, so the digest is checked with
    # the Python that builds the wheel.
    python - "$archives/$source_name.tar.gz" "$source_checksum" <<'PY'
import hashlib
import sys
from pathlib import Path

path, expected = Path(sys.argv[1]), sys.argv[2]
actual = hashlib.sha512(path.read_bytes()).hexdigest()
if actual != expected:
    raise SystemExit(f"{path}: sha512 mismatch\nexpected {expected}\nactual   {actual}")
PY
    mkdir -p "$work/sources/$source_name"
    tar -xzf "$archives/$source_name.tar.gz" --strip-components=1 -C "$work/sources/$source_name"
}

# Same upstream versions and archive hashes as the repository's Windows and
# manylinux inputs.
fetch dav1d-1.5.3 https://github.com/videolan/dav1d/archive/1.5.3.tar.gz \
    8d976b93135213d41385c20205475269a6826a68ebfd716c4d9a7a3ff2a79703e8df0573e43207c81b5db44807d2721db18ec84c0fc6bef98efab86a2cccb6cc
fetch libde265-1.1.1 https://github.com/strukturag/libde265/archive/v1.1.1.tar.gz \
    fb2207f5a3ba901853f61f345c72130f000134918febbc4f3529c3d289fc79ee7457b3e61660110f698bb4ac15d62426e284034bf870bfbd1859ab3feaa52be8
fetch libwebp-1.6.0 https://github.com/webmproject/libwebp/archive/v1.6.0.tar.gz \
    298e0ad4c09392213baf5abb69d330c6203b618800073fe2df91d01d35034197c5d3e29a74573b06971473c52c74514f0e6e0f6c8162f923e2dd15cb1a692aef

reconfigure=""
if [ -f "$work/dav1d-build/build.ninja" ]; then
    reconfigure=--reconfigure
fi
# shellcheck disable=SC2086
meson setup $reconfigure "$work/dav1d-build" "$work/sources/dav1d-1.5.3" \
    --prefix "$prefix" --libdir lib --buildtype release --default-library shared \
    -Denable_tests=false -Denable_tools=false
meson compile -C "$work/dav1d-build"
meson install -C "$work/dav1d-build"

cmake -S "$work/sources/libde265-1.1.1" -B "$work/de265-build" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DCMAKE_INSTALL_LIBDIR=lib -DBUILD_SHARED_LIBS=ON -DENABLE_SDL=OFF
cmake --build "$work/de265-build" --parallel
cmake --install "$work/de265-build"

cmake -S "$work/sources/libwebp-1.6.0" -B "$work/webp-build" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
    -DBUILD_SHARED_LIBS=OFF -DWEBP_BUILD_ANIM_UTILS=OFF -DWEBP_BUILD_CWEBP=OFF \
    -DWEBP_BUILD_DWEBP=OFF -DWEBP_BUILD_GIF2WEBP=OFF -DWEBP_BUILD_IMG2WEBP=OFF \
    -DWEBP_BUILD_VWEBP=OFF -DWEBP_BUILD_WEBPINFO=OFF -DWEBP_BUILD_WEBPMUX=OFF \
    -DWEBP_BUILD_EXTRAS=OFF
cmake --build "$work/webp-build" --parallel
cmake --install "$work/webp-build"

python -m build --wheel --outdir "$work/unrepaired"
# Every variant of the library targets the triple named above, so this is where
# hatch_build.py's last copy of the baseline build lands; see its comment on
# putting the baseline back.
plugin="$CARGO_TARGET_DIR/$musl_target/release/libvs_imageseqs.so"
if [ ! -f "$plugin" ]; then
    echo "the build did not produce $plugin" >&2
    exit 1
fi
if readelf -d "$plugin" | grep -E 'NEEDED.*(libwebp|libsharpyuv|libx265)'; then
    echo 'unexpected shared codec dependency' >&2
    exit 1
fi
auditwheel show "$work"/unrepaired/*.whl
auditwheel repair --plat musllinux_1_2_x86_64 --only-plat \
    --wheel-dir "$work/repaired" "$work"/unrepaired/*.whl
python tools/package-linux-wheel.py "$work/repaired" dist
auditwheel show dist/*.whl
# auditwheel's musl policy allows libc and libz and nothing else, so the C++
# runtime the plugin links through the embedded libheif is bundled beside it
# rather than left to a host that is not promised to have one.
python tools/check-linux-wheel.py \
    --tag py3-none-musllinux_1_2_x86_64 \
    --allow-library libstdc++ \
    --allow-library libgcc_s \
    dist
python tools/stage-native.py dist native

sh tools/make-linux-source-bundle.sh linux-musl-relink-source "$work" "$archives"
