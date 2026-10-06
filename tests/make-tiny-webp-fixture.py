"""Writes the one pixel lossy webp fixture, which the yuv row writer needs.

Run from the repository root with Pillow on ``PATH``:

    python tests/make-tiny-webp-fixture.py

A lossy webp narrower than the chroma step is the case a 4:2:0 frame cannot hold
a plane for: the chroma plane of ``lossy-1x1.webp`` is handed over as no bytes
with no stride, which is the plane a writer dividing by that stride aborts on.
The committed file is 44 bytes and holds one ``VP8 `` chunk, and its single luma
sample is 16 because VP8 states black as 16 in its limited range.
"""

import pathlib

from PIL import Image

FIXTURES = pathlib.Path(__file__).resolve().parent / "fixtures"


def main() -> int:
    FIXTURES.mkdir(parents=True, exist_ok=True)
    path = FIXTURES / "lossy-1x1.webp"
    # One black sample: the encoder writes a VP8 keyframe of one macroblock.
    Image.new("RGB", (1, 1)).save(path, quality=80, method=4)
    print(f"wrote {path.name} ({path.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
