"""Fingerprint and validate the installed codec prefix cached by Linux CI."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STAMP = ".imgseqs-native-cache"
REQUIRED = (
    "include/dav1d/dav1d.h", "include/libde265/de265.h", "include/webp/decode.h",
    "lib/libdav1d.so", "lib/libde265.so", "lib/libwebp.a", "lib/libsharpyuv.a",
    "lib/pkgconfig/dav1d.pc", "lib/pkgconfig/libde265.pc", "lib/pkgconfig/libwebp.pc",
)


def fingerprint(platform: str) -> str:
    # The workflow hash includes the pinned container and the setup policy.
    # A source bundle has no .github directory, and never restores a CI cache.
    inputs = [
        ".github/workflows/build.yml", f"tools/build-{platform}.sh",
        "tools/setup-linux-build.sh", "tools/linux-native-cache.py",
        "tools/manylinux-toolchain.cmake",
    ]
    data: dict[str, object] = {
        "platform": platform,
        "files": {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
                  for name in inputs if (ROOT / name).is_file()},
        "environment": {name: os.environ.get(name, "") for name in (
            "CC", "CXX", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS",
            "PKG_CONFIG_PATH", "CMAKE_PREFIX_PATH", "CMAKE_TOOLCHAIN_FILE",
        )},
        "tools": {},
    }
    tools = data["tools"]
    assert isinstance(tools, dict)
    for name, command in {
        "cc": [*shlex.split(os.environ.get("CC", "cc")), "--version"],
        "cxx": [*shlex.split(os.environ.get("CXX", "c++")), "--version"],
        "cmake": ["cmake", "--version"], "meson": ["meson", "--version"],
        "ninja": ["ninja", "--version"], "nasm": ["nasm", "-v"],
    }.items():
        result = subprocess.run(command, check=True, capture_output=True, text=True)
        tools[name] = result.stdout.strip()
    digest = hashlib.sha256(json.dumps(data, sort_keys=True).encode()).hexdigest()
    return f"linux-native-v1-{platform}-{digest}"


def complete(prefix: Path) -> bool:
    # is_file follows symlinks: restoring just the .so link without its target
    # is a cache miss too. Archives are checked separately by both builders.
    return all((prefix / name).is_file() for name in REQUIRED)


def reusable(prefix: Path, key: str) -> bool:
    stamp = prefix / STAMP
    return complete(prefix) and stamp.is_file() and stamp.read_text().strip() == key


def github_env() -> None:
    names = (
        "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", "RUSTFLAGS", "PKG_CONFIG",
        "PKG_CONFIG_ALLOW_CROSS", "PKG_CONFIG_PATH", "CMAKE_PREFIX_PATH",
        "LD_LIBRARY_PATH", "CMAKE_TOOLCHAIN_FILE", "IMGSEQS_LINUX_SETUP",
        "IMGSEQS_NATIVE_CACHE_KEY",
    )
    with Path(os.environ["GITHUB_ENV"]).open("a", encoding="utf-8") as output:
        for name in names:
            if name in os.environ:
                value = os.environ[name]
                if "\n" in value or "\r" in value:
                    raise SystemExit(f"{name} contains a newline")
                output.write(f"{name}={value}\n")
    with Path(os.environ["GITHUB_PATH"]).open("a", encoding="utf-8") as output:
        output.write("/opt/python/cp312-cp312/bin\n")
        cargo_bin = Path.home() / ".cargo" / "bin"
        if cargo_bin.is_dir():
            output.write(f"{cargo_bin}\n")
    with Path(os.environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as output:
        output.write(f"native-key={os.environ['IMGSEQS_NATIVE_CACHE_KEY']}\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    key = commands.add_parser("key")
    key.add_argument("platform", choices=("manylinux", "musllinux"))
    for name in ("check", "mark"):
        command = commands.add_parser(name)
        command.add_argument("prefix", type=Path)
        command.add_argument("key")
    commands.add_parser("github-env")
    args = parser.parse_args()
    if args.command == "key":
        print(fingerprint(args.platform))
    elif args.command == "check":
        raise SystemExit(0 if reusable(args.prefix, args.key) else 1)
    elif args.command == "mark":
        if not complete(args.prefix):
            raise SystemExit(f"{args.prefix}: native install is incomplete")
        (args.prefix / STAMP).write_text(args.key + "\n", encoding="utf-8")
    else:
        github_env()


if __name__ == "__main__":
    main()
