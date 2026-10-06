#!/bin/sh
# Run directly before restoring CI caches, or source from either Linux builder.
# IMGSEQS_LINUX_PLATFORM selects manylinux or musllinux. The exported environment
# is identical in setup and build, including Cargo's compiler flag cache inputs.
set -eu

case "${IMGSEQS_LINUX_PLATFORM:-}" in
    manylinux|musllinux) ;;
    *) echo 'IMGSEQS_LINUX_PLATFORM must be manylinux or musllinux' >&2; exit 2 ;;
esac

export PATH="/opt/python/cp312-cp312/bin:$PATH"
work="$PWD/target/$IMGSEQS_LINUX_PLATFORM"
prefix="$work/prefix"
archives="${IMGSEQS_NATIVE_ARCHIVES:-$work/archives}"
export CARGO_TARGET_DIR="$work/cargo"
mkdir -p "$archives" "$work/sources" "$prefix" dist

if [ "${IMGSEQS_LINUX_SETUP:-0}" != 1 ]; then
    if [ "$IMGSEQS_LINUX_PLATFORM" = musllinux ]; then
        apk add --no-cache nasm pkgconf
    else
        dnf install --assumeyes nasm
    fi
    python -m pip install build wheel 'cmake>=3.28,<4' meson ninja
fi

if [ "$IMGSEQS_LINUX_PLATFORM" = musllinux ]; then
    # rustup's installer selects a runnable musl host toolchain. CI pins its
    # version and restores both rustup and its Cargo shims before this step.
    if [ -f "$HOME/.cargo/env" ]; then
        . "$HOME/.cargo/env"
    fi
    if [ "${IMGSEQS_LINUX_SETUP:-0}" != 1 ]; then
        if ! command -v rustup >/dev/null 2>&1 && ! command -v cargo >/dev/null 2>&1; then
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- \
                -y --profile minimal --default-toolchain "${IMGSEQS_RUST_TOOLCHAIN:-stable}"
            . "$HOME/.cargo/env"
        fi
        if command -v rustup >/dev/null 2>&1; then
            rustup toolchain install "${IMGSEQS_RUST_TOOLCHAIN:-stable}" --profile minimal
            rustup default "${IMGSEQS_RUST_TOOLCHAIN:-stable}"
            rustup target add x86_64-unknown-linux-musl
        fi
    fi
    export PKG_CONFIG=pkgconf
    export PKG_CONFIG_ALLOW_CROSS=1
    export CARGO_BUILD_TARGET=x86_64-unknown-linux-musl
    # One process must have one musl libc. Do not add the flag again when the
    # build sources this after CI setup has already exported it.
    case " ${RUSTFLAGS:-} " in
        *' -C target-feature=-crt-static '*) ;;
        *) export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=-crt-static" ;;
    esac
fi

# Preserve caller search paths without duplicating the prefix on the build step.
case "${PKG_CONFIG_PATH:-}" in
    "$prefix/lib/pkgconfig"|"$prefix/lib/pkgconfig":*) ;;
    *) export PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" ;;
esac
case "${CMAKE_PREFIX_PATH:-}" in
    "$prefix"|"$prefix":*) ;;
    *) export CMAKE_PREFIX_PATH="$prefix${CMAKE_PREFIX_PATH:+:$CMAKE_PREFIX_PATH}" ;;
esac
case "${LD_LIBRARY_PATH:-}" in
    "$prefix/lib"|"$prefix/lib":*) ;;
    *) export LD_LIBRARY_PATH="$prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" ;;
esac
export CMAKE_TOOLCHAIN_FILE="$PWD/tools/manylinux-toolchain.cmake"
IMGSEQS_NATIVE_CACHE_KEY=$(python tools/linux-native-cache.py key "$IMGSEQS_LINUX_PLATFORM")
export IMGSEQS_NATIVE_CACHE_KEY
export IMGSEQS_LINUX_SETUP=1

if [ "${IMGSEQS_WRITE_GITHUB_ENV:-0}" = 1 ]; then
    python tools/linux-native-cache.py github-env
fi
