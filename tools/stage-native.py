"""Stage a standalone plugin bundle from the final wheel on any platform.

The destination is a build output of this script, so the entries it holds are
replaced rather than merged: a wheel that no longer carries a file must not leave
the previous build's copy of it behind. Only the entries this wheel writes are
touched, so a file a user keeps in the same directory survives.
"""

import argparse
import shutil
import zipfile
from pathlib import Path, PurePosixPath

import build_output


def staged_names(archive: zipfile.ZipFile, prefix: str) -> dict[str, str]:
    """The files of a wheel this bundle keeps, as archive name to relative path."""
    kept = {}
    for name in archive.namelist():
        path = PurePosixPath(name)
        if path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
            raise ValueError(f"invalid archive path: {name}")
        if name.endswith("/"):
            continue
        if name.startswith(prefix + "imageseqs/"):
            kept[name] = name.removeprefix(prefix)
        elif name in {"LICENSE", "THIRD_PARTY_NOTICES"} or name.startswith("LICENSES/"):
            kept[name] = name
    return kept


def plugin_libraries(archive: zipfile.ZipFile, prefix: str, stem: str) -> list[str]:
    """The plugin's own libraries: the stem, beside any CPU variant of it.

    A variant is named ``<stem>.<variant><extension>``, which is the shape
    VapourSynth looks for itself, so what is checked is that shape rather than
    a list of variant names kept in step with the build.
    """
    directory = prefix + "imageseqs/"
    extensions = {".dll", ".so", ".dylib"}
    found = []
    for name in archive.namelist():
        path = PurePosixPath(name)
        if not name.startswith(directory) or path.suffix not in extensions:
            continue
        library = path.stem
        if library != stem and not library.startswith(f"{stem}."):
            raise SystemExit(f"unexpected plugin library in the wheel: {name}")
        found.append(name)
    if not any(PurePosixPath(name).stem == stem for name in found):
        raise SystemExit(f"the manifest names {stem}, which the wheel does not hold")
    return found


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Stage a standalone plugin bundle from the final wheel on any platform."
    )
    parser.add_argument("directory", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    wheel = build_output.single(args.directory, "*.whl", "wheel")
    prefix = "vapoursynth/plugins/"
    with zipfile.ZipFile(wheel) as archive:
        manifest = archive.read(prefix + "imageseqs/manifest.vs").decode("utf-8").splitlines()
        if len(manifest) != 2 or manifest[0] != "[VapourSynth Manifest V1]":
            raise SystemExit("invalid plugin manifest")
        if manifest[1] not in {"vs_imageseqs", "libvs_imageseqs"}:
            raise SystemExit("unexpected manifest entry")
        # One library per CPU level the build ships, all named the way
        # VapourSynth looks for them, so this bundle carries whichever the
        # install machine's core picks.
        plugin_libraries(archive, prefix, manifest[1])
        kept = staged_names(archive, prefix)
        # What this bundle owns is what the wheel writes at the top of the
        # destination, which is the wheel's own layout rather than a list kept
        # here: a platform that stages something else needs no change to this.
        for owned in sorted({PurePosixPath(relative).parts[0] for relative in kept.values()}):
            existing = args.destination / owned
            if existing.is_dir():
                shutil.rmtree(existing)
            elif existing.exists():
                existing.unlink()
        for name, relative in kept.items():
            target = args.destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(archive.read(name))
    print(f"staged {wheel.name} in {args.destination}")


if __name__ == "__main__":
    main()
