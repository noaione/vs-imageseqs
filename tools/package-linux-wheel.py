"""Move auditwheel's bundled codecs into the manifest-controlled plugin folder."""

import argparse
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    wheels = list(args.directory.glob("*.whl"))
    if len(wheels) != 1:
        parser.error("expected exactly one auditwheel-repaired wheel")
    with tempfile.TemporaryDirectory() as temporary:
        subprocess.run([sys.executable, "-m", "wheel", "unpack", str(wheels[0]), "-d", temporary], check=True)
        root, = Path(temporary).iterdir()
        plugin_directory = root / "vapoursynth/plugins/imageseqs"
        shutil.move(str(root / "vapoursynth_imageseqs.libs"), str(plugin_directory / "lib"))
        subprocess.run([
            "patchelf", "--set-rpath", "$ORIGIN/lib",
            str(plugin_directory / "libvs_imageseqs.so"),
        ], check=True)
        # wheel pack regenerates RECORD after both the move and ELF modification.
        args.destination.mkdir(parents=True, exist_ok=True)
        subprocess.run([sys.executable, "-m", "wheel", "pack", str(root), "-d", str(args.destination)], check=True)


if __name__ == "__main__":
    main()
