r"""Creates the YCbCr TIFF fixtures used by ``tests/readalpha.vpy``.

Run from the repository root:

    C:\Python314\python.exe tests\make-tiff-ycbcr-fixtures.py

Every file is written by hand rather than by an encoder, because ImageMagick
writes a YCbCr page only at full resolution and libtiff refuses to read the pages
it does write, so no encoder here produces the shapes this reader has to take.
The raster is stored the way the specification says: a coding unit of two by two
pixels holds its four luma samples and then one chroma pair, a unit of two by one
holds two luma samples and a chroma pair, and one by one holds one of each. The
units of one unit row are walked left to right and the row is padded to a byte
boundary.

The luma steps by eight across and thirty-two down and the chroma is flat, so a
unit read in the wrong order, a unit row padded wrongly, or a chroma plane of the
wrong size is visible as colour where the file states none. `tiff2rgba` from
libtiff reads every one of these to neutral greys at exactly the luma levels,
which is what the checks lean on.
"""

from __future__ import annotations

import os
import struct

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

WIDTH = 37
HEIGHT = 23

TYPE_SHORT = 3
TYPE_RATIONAL = 5

# The two coefficient triples the reader is taught to name, and one it is not.
BT601 = [(299, 1000), (587, 1000), (114, 1000)]
BT709 = [(2126, 10000), (7152, 10000), (722, 10000)]
UNCANNY = [(3, 10), (4, 10), (3, 10)]

# Reference black and white for luma, cb and cr. The specification's default is
# no headroom at all, which is what a file that states nothing means.
FULL = [(0, 1), (255, 1), (128, 1), (255, 1), (128, 1), (255, 1)]
LIMITED = [(16, 1), (235, 1), (128, 1), (240, 1), (128, 1), (240, 1)]


def raster(horiz: int, vert: int, width: int, height: int) -> bytes:
    """The strip of one subsampling, unit by unit, each unit row padded."""
    out = bytearray()
    for unit_y in range(0, height, vert):
        row = bytearray()
        for unit_x in range(0, width, horiz):
            for y in range(unit_y, unit_y + vert):
                for x in range(unit_x, unit_x + horiz):
                    row.append(16 + (x % 8) * 8 + (y % 4) * 32)
            row += bytes([128, 128])
        # No padding: a unit row of byte samples is already a whole number of
        # bytes, and libtiff reads an odd 4:4:4 row of thirty-seven pixels as
        # thirty-seven triples rather than as thirty-seven and a pad.
        out += row
    return bytes(out)


def write(
    name: str,
    *,
    horiz: int = 1,
    vert: int = 1,
    bits: int = 8,
    compression: int = 1,
    coefficients: list[tuple[int, int]] | None = None,
    width: int = WIDTH,
    height: int = HEIGHT,
    levels: list[tuple[int, int]] | None = None,
) -> None:
    strip = raster(horiz, vert, width, height)
    tags: list[tuple[int, int, list[int] | list[tuple[int, int]]]] = [
        (256, 4, [width]),
        (257, 4, [height]),
        (258, TYPE_SHORT, [bits, bits, bits]),
        (259, TYPE_SHORT, [compression]),
        (262, TYPE_SHORT, [6]),
        (273, 4, [0]),
        (277, TYPE_SHORT, [3]),
        (278, 4, [height]),
        (279, 4, [len(strip)]),
        (284, TYPE_SHORT, [1]),
        (530, TYPE_SHORT, [horiz, vert]),
    ]
    if coefficients is not None:
        tags.append((529, TYPE_RATIONAL, coefficients))
    if levels is not None:
        tags.append((532, TYPE_RATIONAL, levels))
    tags.sort(key=lambda entry: entry[0])

    def raw(type_: int, values: list) -> bytes:
        if type_ == TYPE_RATIONAL:
            return b"".join(struct.pack("<II", *value) for value in values)
        return b"".join(
            struct.pack("<H", value) if type_ == TYPE_SHORT else struct.pack("<I", value)
            for value in values
        )

    ifd_size = 2 + 12 * len(tags) + 4
    wide = [raw(type_, values) for _, type_, values in tags if len(raw(type_, values)) > 4]
    data_at = 8 + ifd_size + sum(len(blob) for blob in wide)
    tags = [(tag, type_, [data_at] if tag == 273 else values) for tag, type_, values in tags]

    out = bytearray(struct.pack("<2sHI", b"II", 42, 8))
    out += struct.pack("<H", len(tags))
    blobs: list[bytes] = []
    for tag, type_, values in tags:
        blob = raw(type_, values)
        out += struct.pack("<HHI", tag, type_, len(values))
        if len(blob) <= 4:
            out += blob.ljust(4, b"\0")
        else:
            out += struct.pack("<I", 8 + ifd_size + sum(len(seen) for seen in blobs))
            blobs.append(blob)
    out += struct.pack("<I", 0)
    for blob in blobs:
        out += blob
    out += strip

    path = os.path.join(FIXTURES, name)
    open(path, "wb").write(bytes(out))
    print(f"  {name}: {len(out)} bytes, {width}x{height}, {horiz}x{vert}, {bits} bits")


def main() -> None:
    # ---- The three samplings the format defines. All three hold the same luma.
    write("tiff-ycbcr-444.tiff", horiz=1, vert=1)
    write("tiff-ycbcr-422.tiff", horiz=2, vert=1, width=36, height=22)
    write("tiff-ycbcr-420.tiff", horiz=2, vert=2, width=36, height=22)

    # ---- The colour description, which decides `_Matrix` and `_Range`. A page
    # that states nothing is bt.601 at full range, so these two are the cases
    # where a tag changes the properties.
    write("tiff-ycbcr-709.tiff", coefficients=BT709)
    write("tiff-ycbcr-limited.tiff", levels=LIMITED)
    write("tiff-ycbcr-stated.tiff", coefficients=BT601, levels=FULL)

    # ---- The shapes this reader refuses. Each is refused at identify, so the
    # probe cannot promise a frame the decode would not produce.
    write("tiff-ycbcr-uncanny.tiff", coefficients=UNCANNY)
    write("tiff-ycbcr-16bit.tiff", bits=16)
    # LZW, whose strip is not a compressed one: the reader refuses the
    # compression before it looks at a sample, which is the point of the check.
    write("tiff-ycbcr-lzw.tiff", compression=5)


if __name__ == "__main__":
    main()
