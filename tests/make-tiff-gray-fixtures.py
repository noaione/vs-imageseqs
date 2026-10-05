r"""Writes the low-bit grayscale TIFF fixtures into tests/fixtures, and checks them.

    .\.venv\Scripts\python.exe tests/make-tiff-gray-fixtures.py [--check]

It writes `tiff-gray1.tiff`, `tiff-gray1-lzw.tiff`, `tiff-gray1-pred.tiff`,
`tiff-gray1-white.tiff`, `tiff-gray2.tiff` and `tiff-gray4.tiff`, which are
the fixtures `src/formats/tiff.rs`'s narrow gray tests and `tests/readalpha.vpy`'s
`test_narrow_gray_pages` read.

The candidate table in plan 34 names low-bit grayscale TIFF as needing an
individual decision, and a decision needs files. Pillow has no four bit gray mode
-- and its "1" mode is stored as eight bit gray, so it cannot write a bilevel
TIFF either -- so the container is written here by hand: a minimal uncompressed
TIFF, which is the shape every writer emits.

The one compressed fixture is not written by hand, because a file that says LZW
and holds uncompressed bytes is a lie a reader may or may not forgive. It is the
hand written bilevel page round tripped through libtiff with LZW selected, so the
container is genuine and only the compression came from a writer.

Each fixture is a 16x8 page over every value its width can hold, so a wrong
expansion is a wrong picture rather than a plausible one, and a bilevel fixture
is half black and half white rather than an even spread that could hide a swap.
`--check` prints what Pillow reads, which is what the validator and the unit
tests compare against.
"""

from __future__ import annotations

import hashlib
import pathlib
import struct
import sys

FIXTURES = pathlib.Path("tests/fixtures")

WIDTH, HEIGHT = 16, 8

SHORT, LONG = 3, 4

# ImageWidth, ImageLength, BitsPerSample, Compression, PhotometricInterpretation,
# StripOffsets, SamplesPerPixel, RowsPerStrip, StripByteCounts, Predictor.
# The order matters: a TIFF directory is sorted by tag, and a reader is entitled
# to rely on it.
TAGS = [256, 257, 258, 259, 262, 273, 277, 278, 279, 317]

COMPRESSION_NONE = 1

BLACK_IS_ZERO, WHITE_IS_ZERO = 1, 0

NAMES = [
    "tiff-gray1.tiff",
    "tiff-gray1-lzw.tiff",
    "tiff-gray1-pred.tiff",
    "tiff-gray1-white.tiff",
    "tiff-gray2.tiff",
    "tiff-gray4.tiff",
]


def pack(rows: list[list[int]], bits: int) -> bytes:
    """Pack samples most significant bit first, each row padded to a byte."""
    out = bytearray()
    per_byte = 8 // bits
    for row in rows:
        for start in range(0, WIDTH, per_byte):
            byte = 0
            for offset in range(per_byte):
                value = row[start + offset] if start + offset < WIDTH else 0
                byte |= (value & ((1 << bits) - 1)) << (8 - bits * (offset + 1))
            out.append(byte)
    return bytes(out)


def write_tiff(
    path: pathlib.Path,
    bits: int,
    rows: list[list[int]],
    *,
    predictor: int = 1,
    photometric: int = BLACK_IS_ZERO,
) -> pathlib.Path:
    """A minimal little endian TIFF holding one uncompressed strip."""
    raster = pack(rows, bits)
    entries = len(TAGS)
    ifd_offset = 8
    raster_offset = ifd_offset + 2 + entries * 12 + 4

    values = {
        256: (SHORT, WIDTH),
        257: (SHORT, HEIGHT),
        258: (SHORT, bits),
        259: (SHORT, COMPRESSION_NONE),
        262: (SHORT, photometric),
        273: (LONG, raster_offset),
        277: (SHORT, 1),
        278: (SHORT, HEIGHT),
        279: (LONG, len(raster)),
        317: (SHORT, predictor),
    }

    header = b"II" + struct.pack("<HI", 42, ifd_offset)
    directory = struct.pack("<H", entries)
    for tag in TAGS:
        kind, value = values[tag]
        if kind == SHORT:
            directory += struct.pack("<HHIHH", tag, kind, 1, value, 0)
        else:
            directory += struct.pack("<HHII", tag, kind, 1, value)
    directory += struct.pack("<I", 0)

    path.write_bytes(header + directory + raster)
    return path


def ramp(bits: int) -> list[list[int]]:
    """Every value the width can hold, cycling so one row holds several."""
    values = [v * 255 // ((1 << bits) - 1) for v in range(1 << bits)]
    return [[values[(x + y) % len(values)] for x in range(WIDTH)] for y in range(HEIGHT)]


def bilevel() -> list[list[int]]:
    """Half black and half white, which is what a fax page is."""
    return [[1 if x < WIDTH // 2 else 0 for x in range(WIDTH)] for _ in range(HEIGHT)]


def build() -> None:
    write_tiff(FIXTURES / "tiff-gray1.tiff", 1, bilevel())
    write_tiff(FIXTURES / "tiff-gray1-pred.tiff", 1, bilevel(), predictor=2)
    write_tiff(FIXTURES / "tiff-gray1-white.tiff", 1, bilevel(), photometric=WHITE_IS_ZERO)
    write_tiff(FIXTURES / "tiff-gray2.tiff", 2, ramp(2))
    write_tiff(FIXTURES / "tiff-gray4.tiff", 4, ramp(4))

    # The compressed one is libtiff's, because only a real compressor can write
    # a real compressed stream.
    from PIL import Image

    page = Image.open(FIXTURES / "tiff-gray1.tiff")
    page.save(FIXTURES / "tiff-gray1-lzw.tiff", compression="tiff_lzw")


def check() -> None:
    from PIL import Image

    for name in NAMES:
        path = FIXTURES / name
        image = Image.open(path)
        gray = image.convert("L")
        digest = hashlib.sha256(bytes(gray.tobytes())).hexdigest()[:16]
        corners = [gray.getpixel((0, 0)), gray.getpixel((WIDTH - 1, 0))]
        print(
            f"{name}: pillow {image.mode} {gray.size}"
            f" bits={image.tag_v2.get(258)} comp={image.tag_v2.get(259)}"  # pyright: ignore[reportAttributeAccessIssue]
            f" photo={image.tag_v2.get(262)} {digest} first/last={corners}"  # pyright: ignore[reportAttributeAccessIssue]
        )


if __name__ == "__main__":
    if "--check" not in sys.argv:
        build()
    check()
