"""Writes the core-header bitmap fixtures the reader now accepts.

`BITMAPCOREHEADER` is twelve bytes and differs from the information header in
three places this script has to get right, because the reader is checking them:
both dimensions are signed sixteen bit values, there is no compression field so
only the uncompressed form exists, and a palette entry is three bytes.

    python tests/make-core-bmp-fixtures.py

The 24 bit one is the case the gaps table names -- a 1x1 bitmap that was refused
by name -- and the 4 bit one is here so a three byte palette table is read at
all. A core header has no colour count, so a palette page carries an entry for
every index its depth names, which is why this one is 4 bit and not 8: sixteen
entries are a table a person can read and 256 are not.
"""

import pathlib
import struct

HERE = pathlib.Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures"


def core_bmp(width, height, bit_count, palette, rows):
    """A `BM` file with a `BITMAPCOREHEADER`, bottom-up, rows already padded."""
    header = struct.pack("<IhhHH", 12, width, height, 1, bit_count)
    table = b"".join(bytes(entry) for entry in palette)
    stride = ((width * bit_count + 31) // 32) * 4
    raster = b"".join(row.ljust(stride, b"\0") for row in rows)
    offset = 14 + len(header) + len(table)
    return (
        b"BM"
        + struct.pack("<IHHI", offset + len(raster), 0, 0, offset)
        + header
        + table
        + raster
    )


def main():
    # 1x1, 24 bit, bottom-up: one row of one padded pixel, blue, green, red.
    (FIXTURES / "bmp-core-24.bmp").write_bytes(
        core_bmp(1, 1, 24, [], [bytes([7, 5, 3])])
    )

    # 2x2, 4 bit: a sixteen entry palette of three byte entries, which a reader
    # that stepped four bytes at a time would misread as soon as it passed the
    # first one. Two pixels share a byte, so the row also checks the bit order.
    palette = [(30, 0, 0), (0, 60, 0), (0, 0, 90), (200, 200, 200)] + [(0, 0, 0)] * 12
    rows = [bytes([0x10]), bytes([0x32])]
    (FIXTURES / "bmp-core-4.bmp").write_bytes(core_bmp(2, 2, 4, palette, rows))

    for name in ("bmp-core-24.bmp", "bmp-core-4.bmp"):
        print(name, (FIXTURES / name).stat().st_size)


if __name__ == "__main__":
    main()