"""Bundle and relink the macOS runtime libraries inside a plugin wheel."""

from __future__ import annotations

import argparse
import base64
import csv
import hashlib
import os
import stat
import tempfile
import zipfile
from pathlib import Path, PurePosixPath

from delocate import delocate_path
from delocate.tools import set_install_id

PLUGIN = PurePosixPath("vapoursynth/plugins/imageseqs/libvs_imageseqs.dylib")
LIBRARY_DIRECTORY = PurePosixPath("vapoursynth/plugins/imageseqs/lib")
SYSTEM_PREFIXES = ("/usr/lib/", "/System/")
EXPECTED_LIBRARIES = {"libdav1d.7.dylib", "libde265.0.2.1.dylib"}


def wheel_file(directory: Path) -> Path:
    wheels = sorted(path for path in directory.glob("*.whl") if path.is_file())
    if len(wheels) != 1:
        names = ", ".join(path.name for path in wheels) or "nothing"
        raise SystemExit(
            f"expected exactly one macOS wheel in {directory}, found: {names}"
        )
    return wheels[0]


def checked_member(name: str) -> PurePosixPath:
    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
        raise ValueError(f"invalid wheel path: {name}")
    return path


def unpack_wheel(wheel: Path, destination: Path) -> None:
    with zipfile.ZipFile(wheel) as archive:
        for info in archive.infolist():
            path = checked_member(info.filename)
            if info.is_dir():
                continue
            mode = info.external_attr >> 16
            if stat.S_ISLNK(mode):
                raise ValueError(
                    f"wheel contains an unsupported symlink: {info.filename}"
                )
            target = destination.joinpath(*path.parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(archive.read(info))
            if mode:
                target.chmod(stat.S_IMODE(mode))


def record_wheel(root: Path) -> None:
    records = list(root.glob("*.dist-info/RECORD"))
    if len(records) != 1:
        raise ValueError(f"expected one wheel RECORD file, found {len(records)}")
    record = records[0]
    rows = []
    for path in sorted(candidate for candidate in root.rglob("*") if candidate.is_file()):
        relative = path.relative_to(root).as_posix()
        if path == record:
            rows.append((relative, "", ""))
            continue
        digest = base64.urlsafe_b64encode(hashlib.sha256(path.read_bytes()).digest())
        digest = digest.rstrip(b"=").decode("ascii")
        rows.append((relative, f"sha256={digest}", str(path.stat().st_size)))
    with record.open("w", encoding="utf-8", newline="") as stream:
        csv.writer(stream, lineterminator="\n").writerows(rows)


def repack_wheel(root: Path, wheel: Path) -> None:
    record_wheel(root)
    with zipfile.ZipFile(wheel, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(candidate for candidate in root.rglob("*") if candidate.is_file()):
            relative = path.relative_to(root).as_posix()
            info = zipfile.ZipInfo(relative, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (stat.S_IFREG | stat.S_IMODE(path.stat().st_mode)) << 16
            archive.writestr(info, path.read_bytes())


def command_output(*arguments: str) -> str:
    import subprocess

    return subprocess.run(arguments, check=True, capture_output=True, text=True).stdout


def minimum_macos_version(binary: Path) -> tuple[int, int]:
    lines = command_output("otool", "-l", str(binary)).splitlines()
    for index, line in enumerate(lines):
        if "cmd LC_BUILD_VERSION" in line:
            for field in lines[index : index + 8]:
                if " minos " in field:
                    parts = field.split(" minos ", 1)[1].split(".")
                    return int(parts[0]), int(parts[1])
        if "cmd LC_VERSION_MIN_MACOSX" in line:
            for field in lines[index : index + 8]:
                if " version " in field:
                    parts = field.split(" version ", 1)[1].split(".")
                    return int(parts[0]), int(parts[1])
    raise ValueError(f"{binary} has no macOS deployment target load command")


def validate_bundle(root: Path) -> None:
    plugin = root.joinpath(*PLUGIN.parts)
    library_directory = root.joinpath(*LIBRARY_DIRECTORY.parts)
    if not plugin.is_file() or not library_directory.is_dir():
        raise ValueError("wheel must contain the macOS plugin and imageseqs/lib/")
    binaries = [plugin, *sorted(library_directory.glob("*.dylib"))]
    if not binaries[1:]:
        raise ValueError("no runtime dylibs were bundled")
    bundled_names = {binary.name for binary in binaries[1:]}
    if bundled_names != EXPECTED_LIBRARIES:
        raise ValueError(f"unexpected macOS runtime dylibs: {sorted(bundled_names)}")
    target_parts = os.environ.get("MACOSX_DEPLOYMENT_TARGET", "11.0").split(".")
    target = tuple(int(part) for part in target_parts[:2])
    if len(target) != 2:
        raise ValueError("MACOSX_DEPLOYMENT_TARGET must be a major.minor version")

    for binary in binaries:
        minimum = minimum_macos_version(binary)
        if minimum > target:
            raise ValueError(
                f"{binary.relative_to(root)} requires macOS {minimum[0]}.{minimum[1]}, "
                f"above the wheel target {target[0]}.{target[1]}"
            )
        architectures = command_output("lipo", "-archs", str(binary)).split()
        if "arm64" not in architectures:
            raise ValueError(f"{binary.relative_to(root)} has no arm64 slice: {architectures}")
        install_id_lines = command_output("otool", "-D", str(binary)).splitlines()
        install_id = install_id_lines[1].strip() if len(install_id_lines) > 1 else None
        if install_id != f"@loader_path/{binary.name}":
            raise ValueError(
                f"{binary.relative_to(root)} has a non-relative install ID: {install_id}"
            )
        lines = command_output("otool", "-L", str(binary)).splitlines()[1:]
        for line in lines:
            dependency = line.strip().split(" (compatibility version", 1)[0]
            if dependency == install_id:
                continue
            if dependency.startswith(SYSTEM_PREFIXES):
                continue
            if not dependency.startswith("@loader_path/"):
                raise ValueError(
                    f"{binary.relative_to(root)} has an external dependency: {dependency}"
                )
            resolved = (binary.parent / dependency.removeprefix("@loader_path/")).resolve()
            if not resolved.is_relative_to(root.resolve()) or not resolved.is_file():
                raise ValueError(
                    f"{binary.relative_to(root)} has an unresolved bundled dependency: "
                    f"{dependency}"
                )


def package(directory: Path) -> Path:
    wheel = wheel_file(directory)
    target_name = (
        "macosx_"
        + os.environ.get("MACOSX_DEPLOYMENT_TARGET", "11.0").replace(".", "_")
        + "_arm64"
    )
    if target_name not in wheel.name:
        raise ValueError(f"macOS wheel tag must include {target_name}: {wheel.name}")
    with tempfile.TemporaryDirectory(prefix="imgseqs-macos-wheel-") as temporary:
        root = Path(temporary) / "wheel"
        root.mkdir()
        unpack_wheel(wheel, root)
        plugin = root.joinpath(*PLUGIN.parts)
        if not plugin.is_file():
            raise ValueError(f"wheel is missing {PLUGIN}")
        library_directory = root.joinpath(*LIBRARY_DIRECTORY.parts)
        copied = delocate_path(str(root), str(library_directory), sanitize_rpaths=True)
        if not copied:
            raise ValueError("the macOS plugin has no external runtime dylibs to bundle")
        for binary in [plugin, *sorted(library_directory.glob("*.dylib"))]:
            set_install_id(binary, f"@loader_path/{binary.name}")
        validate_bundle(root)
        repack_wheel(root, wheel)
    print(f"bundled {len(copied)} dylib(s) in {wheel}")
    return wheel


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "directory", type=Path, help="directory containing the intermediate wheel"
    )
    args = parser.parse_args()
    package(args.directory)


if __name__ == "__main__":
    main()
