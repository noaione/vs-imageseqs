"""Stage a standalone plugin bundle from the final wheel on any platform."""

import argparse
import zipfile
from pathlib import Path, PurePosixPath


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    wheels = list(args.directory.glob("*.whl"))
    if len(wheels) != 1:
        parser.error("expected exactly one wheel")
    prefix = "vapoursynth/plugins/"
    with zipfile.ZipFile(wheels[0]) as archive:
        manifest = archive.read(prefix + "imageseqs/manifest.vs").decode("utf-8").splitlines()
        if len(manifest) != 2 or manifest[0] != "[VapourSynth Manifest V1]":
            raise ValueError("invalid plugin manifest")
        if manifest[1] not in {"vs_imageseqs", "libvs_imageseqs"}:
            raise ValueError("unexpected manifest entry")
        candidates = [prefix + "imageseqs/" + manifest[1] + suffix for suffix in (".dll", ".so", ".dylib")]
        if sum(name in archive.namelist() for name in candidates) != 1:
            raise ValueError("manifest must name exactly one plugin")
        for name in archive.namelist():
            path = PurePosixPath(name)
            if path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
                raise ValueError(f"invalid archive path: {name}")
            if name.endswith("/"):
                continue
            if name.startswith(prefix + "imageseqs/"):
                relative = name.removeprefix(prefix)
            elif name in {"LICENSE", "THIRD_PARTY_NOTICES"} or name.startswith("LICENSES/"):
                relative = name
            else:
                continue
            target = args.destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(archive.read(name))
    print(f"staged {wheels[0].name} in {args.destination}")


if __name__ == "__main__":
    main()
