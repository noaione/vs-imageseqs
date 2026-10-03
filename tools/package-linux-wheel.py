"""Move auditwheel's bundled codecs into the manifest-controlled plugin folder.

The destination is a build output of the Linux build, so the wheel it already
holds is removed first: the checker beside this script requires exactly one, and
a second run that kept the previous version's wheel would fail there rather than
here.
"""

import argparse
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import build_output


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    wheel = build_output.single(args.directory, "*.whl", "auditwheel-repaired wheel")
    with tempfile.TemporaryDirectory() as temporary:
        subprocess.run([sys.executable, "-m", "wheel", "unpack", str(wheel), "-d", temporary], check=True)
        root, = Path(temporary).iterdir()
        plugin_directory = root / "vapoursynth/plugins/imageseqs"
        shutil.move(str(root / "vapoursynth_imageseqs.libs"), str(plugin_directory / "lib"))
        # Every CPU variant is a library of its own and every one of them needs
        # the loader path to the directory the codecs just moved into.
        for library in sorted(plugin_directory.glob("libvs_imageseqs*.so")):
            subprocess.run([
                "patchelf", "--set-rpath", "$ORIGIN/lib",
                str(library),
            ], check=True)
        # wheel pack regenerates RECORD after both the move and ELF modification.
        args.destination.mkdir(parents=True, exist_ok=True)
        build_output.clear(args.destination, ("*.whl",))
        subprocess.run([sys.executable, "-m", "wheel", "pack", str(root), "-d", str(args.destination)], check=True)


if __name__ == "__main__":
    main()
