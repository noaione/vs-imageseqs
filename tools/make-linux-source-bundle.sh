#!/bin/sh
# Bundle the sources a published Linux wheel can be rebuilt and relinked from.
#
#     sh tools/make-linux-source-bundle.sh <name> <work-directory> <archives-directory>
#
# Run from the repository root, with a Rust toolchain, cmake and the Python the
# build itself uses on PATH: the bundle records their versions and vendors every
# Cargo dependency, so a wheel can be rebuilt from it without a network. Each
# Linux build calls this once, because the libheif it statically links is what
# makes the corresponding source a distribution obligation; the license section
# of THIRD_PARTY_NOTICES is where that reasoning lives.
set -eu

if [ "$#" -ne 3 ]; then
    echo "usage: sh tools/make-linux-source-bundle.sh <name> <work-directory> <archives-directory>" >&2
    exit 2
fi

name="$1"
work="$2"
archives="$3"

# The bundle directory is removed before it is written, so a name or a work
# directory that is not there is refused rather than expanded into the root.
if [ -z "$name" ] || [ ! -d "$work" ]; then
    echo "refusing to clear '$work/$name': the work directory has to exist and the name has to be non-empty" >&2
    exit 2
fi
bundle="$work/$name"

# The bundle is a build output of this script, so it starts empty: a name that
# is already there is what a previous run, of a previous tree, left behind.
rm -rf "$bundle"
mkdir -p "$bundle/.cargo" "$bundle/native-archives" source-bundle

cp -a src tools tests docs LICENSES "$bundle/"
cp Cargo.toml Cargo.lock build.rs pyproject.toml hatch_build.py README.md CHANGELOG.md \
    LICENSE THIRD_PARTY_NOTICES "$bundle/"
cp "$archives"/*.tar.gz "$bundle/native-archives/"
cp docs/LINUX-BUILD.md "$bundle/BUILDING.md"

(
    cd "$bundle"
    cargo vendor --locked --versioned-dirs vendor > .cargo/config.toml
    { rustc -Vv; cmake --version; python -m pip freeze; } > BUILD-ENVIRONMENT.txt
)
tar -czf "source-bundle/$name.tar.gz" -C "$work" "$name"
echo "wrote source-bundle/$name.tar.gz"
