"""Check the final repaired Linux wheel, including its RECORD hashes."""

from __future__ import annotations

import argparse
import base64
import csv
import hashlib
import io
import zipfile
from email.parser import Parser
from pathlib import Path, PurePosixPath

import build_output

PLUGIN = "vapoursynth/plugins/imageseqs/libvs_imageseqs.so"
PLUGIN_STEM = "vapoursynth/plugins/imageseqs/libvs_imageseqs"
PLUGIN_EXTENSION = ".so"
MANIFEST = "vapoursynth/plugins/imageseqs/manifest.vs"
LIBRARIES = "vapoursynth/plugins/imageseqs/lib"
TAG = "py3-none-manylinux_2_28_x86_64"


def is_plugin_variant(name: str) -> bool:
    """Whether a wheel member is a CPU variant of the plugin's own library.

    The build stages one library per x86-64 level beside the baseline, named
    ``<stem>.<variant><extension>``, which is the shape VapourSynth finds them
    by; see ``hatch_build.py``.
    """
    if not name.startswith(f"{PLUGIN_STEM}.") or not name.endswith(PLUGIN_EXTENSION):
        return False
    variant = name[len(PLUGIN_STEM) + 1 : -len(PLUGIN_EXTENSION)]
    return bool(variant) and variant.isalnum()

def check_wheel(wheel: Path) -> None:
    with zipfile.ZipFile(wheel) as archive:
        names = set(archive.namelist())
        metadata_names = [name for name in names if name.endswith(".dist-info/WHEEL")]
        if len(metadata_names) != 1:
            raise ValueError("expected one WHEEL metadata file")
        metadata = Parser().parsestr(archive.read(metadata_names[0]).decode())
        if metadata.get_all("Tag") != [TAG] or not wheel.name.endswith(f"-{TAG}.whl"):
            raise ValueError(f"expected auditwheel's {TAG} tag")
        if metadata.get("Root-Is-Purelib", "").lower() != "false":
            raise ValueError("a native wheel must not be marked pure Python")
        required = {
            PLUGIN, MANIFEST, "LICENSE", "THIRD_PARTY_NOTICES", "LICENSES/README.md",
            "LICENSES/dav1d-COPYING.txt", "LICENSES/libde265-COPYING.txt",
            "LICENSES/libheif-COPYING.txt", "LICENSES/libwebp-COPYING.txt",
            "LICENSES/openjpeg-COPYING.txt",
        }
        if missing := required - names:
            raise ValueError(f"missing wheel contents: {sorted(missing)}")
        libraries = {name for name in names if name.startswith(f"{LIBRARIES}/") and not name.endswith("/")}
        for library in ("libdav1d-", "libde265-"):
            if not any(PurePosixPath(name).name.startswith(library) for name in libraries):
                raise ValueError(f"missing auditwheel-bundled {library} library")
        for name in names:
            path = PurePosixPath(name)
            if path.is_absolute() or ".." in path.parts:
                raise ValueError(f"invalid archive path: {name}")
            if name.endswith("/") or ".dist-info/" in name:
                continue
            allowed = (name in required) or name.startswith(("LICENSES/", f"{LIBRARIES}/"))
            if not allowed and not is_plugin_variant(name):
                raise ValueError(f"unexpected content in plugin-only wheel: {name}")
            if name in libraries and not path.name.startswith(("libdav1d-", "libde265-")):
                raise ValueError(f"unexpected bundled dependency; review its license: {name}")
        if archive.read(MANIFEST) != b"[VapourSynth Manifest V1]\nlibvs_imageseqs\n":
            raise ValueError("manifest must list only the plugin, without its extension")
        record_name = metadata_names[0].removesuffix("WHEEL") + "RECORD"
        records = list(csv.reader(io.StringIO(archive.read(record_name).decode())))
        if {row[0] for row in records} != {name for name in names if not name.endswith("/")}:
            raise ValueError("RECORD does not match the wheel contents")
        for name, digest, size in records:
            if name == record_name:
                continue
            content = archive.read(name)
            expected = "sha256=" + base64.urlsafe_b64encode(hashlib.sha256(content).digest()).rstrip(b"=").decode()
            if digest != expected or size != str(len(content)):
                raise ValueError(f"invalid RECORD hash or size: {name}")
    print(f"checked {wheel.name}: plugin, bundled codecs, legal files, and ABI tag")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    check_wheel(build_output.single(args.directory, "*.whl", "repaired wheel"))


if __name__ == "__main__":
    main()
