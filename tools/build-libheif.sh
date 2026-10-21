#!/usr/bin/env bash
# Build the pinned shared libheif used by Unix wheels and CI tests.
set -euo pipefail

if [[ "$#" -ne 3 ]]; then
    echo "usage: bash tools/build-libheif.sh <source-dir> <build-dir> <prefix>" >&2
    exit 2
fi

source_dir="$1"
build_dir="$2"
prefix="$3"

cmake_args=(
    -S "$source_dir"
    -B "$build_dir"
    -G Ninja
    -DCMAKE_BUILD_TYPE=Release
    "-DCMAKE_INSTALL_PREFIX=$prefix"
    -DCMAKE_INSTALL_LIBDIR=lib
    "-DCMAKE_PREFIX_PATH=$prefix"
    "-DCMAKE_INSTALL_RPATH=$prefix/lib"
    -DBUILD_SHARED_LIBS=ON
    -DBUILD_TESTING=OFF
    -DENABLE_PLUGIN_LOADING=OFF
    -DWITH_EXAMPLES=OFF
    -DWITH_GDK_PIXBUF=OFF
    -DWITH_LIBDE265=ON
    -DWITH_LIBDE265_PLUGIN=OFF
    -DWITH_DAV1D=ON
    -DWITH_DAV1D_PLUGIN=OFF
    -DWITH_LIBSHARPYUV=OFF
    -DWITH_UNCOMPRESSED_CODEC=OFF
    -DWITH_X265=OFF
    -DWITH_KVAZAAR=OFF
    -DWITH_UVG266=OFF
    -DWITH_VVDEC=OFF
    -DWITH_VVENC=OFF
    -DWITH_X264=OFF
    -DWITH_OpenH264_DECODER=OFF
    -DWITH_AOM_DECODER=OFF
    -DWITH_AOM_ENCODER=OFF
    -DWITH_SvtEnc=OFF
    -DWITH_RAV1E=OFF
    -DWITH_JPEG_DECODER=OFF
    -DWITH_JPEG_ENCODER=OFF
    -DWITH_OpenJPEG_DECODER=OFF
    -DWITH_OpenJPEG_ENCODER=OFF
    -DWITH_OPENJPH_ENCODER=OFF
    -DWITH_OPEN_JPH_ENCODER=OFF
    -DWITH_FFMPEG_DECODER=OFF
)

if [[ -n "${CMAKE_TOOLCHAIN_FILE:-}" ]]; then
    cmake_args+=("-DCMAKE_TOOLCHAIN_FILE=$CMAKE_TOOLCHAIN_FILE")
fi

if [[ "$(uname -s)" == Darwin ]]; then
    cmake_args+=("-DCMAKE_INSTALL_NAME_DIR=$prefix/lib")
    if [[ -n "${MACOSX_DEPLOYMENT_TARGET:-}" ]]; then
        cmake_args+=("-DCMAKE_OSX_DEPLOYMENT_TARGET=$MACOSX_DEPLOYMENT_TARGET")
    fi
fi

cmake "${cmake_args[@]}"
cmake --build "$build_dir" --parallel
cmake --install "$build_dir"
