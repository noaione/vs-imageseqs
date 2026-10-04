r"""Creates the palette TIFF fixtures used by ``tests/readalpha.vpy``.

Run from the repository root with ImageMagick on ``PATH``:

    python tests/make-tiff-palette-fixtures.py

A palette page states the width of one index in ``BitsPerSample`` and holds the
colours in ``ColorMap``, and ImageMagick picks the width from the number of
colours it was asked to keep: two colours are one bit an index, four are two,
sixteen are four and anything larger is eight. It never writes a wider one, and
libtiff refuses the sixteen bit page that was written by hand to see whether it
could, so one, two, four and eight are the widths that have a reader to check
against.

``tiff-palette.tiff`` is the four bit page and is written by
``make-tiff-exr-fixtures.py``, which was already making it; this script writes
the widths that had no fixture at all. Which file is which width is printed from
the directory itself rather than assumed, because ImageMagick accepts the options
that are meant to select a width and quietly writes another one when the build
cannot.
"""

from __future__ import annotations

import os
import struct
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

# A committed colour page, so the script needs nothing outside the repository.
SOURCE = os.path.join(FIXTURES, "tiff-rgb8.tiff")


def magick(*args: str) -> None:
    subprocess.run(["magick", *args], check=True)


def write_three_bit(path: str) -> None:
    """Writes a palette whose indices are three bits wide.

    Hand written because ImageMagick does not write one: it picks a width from
    the number of colours and only ever writes one, two, four or eight. Three
    bits is a legal width and a byte does not divide into it, so it is the
    boundary between the widths this tree reads and the widths it refuses, and
    a fixture is the only way that boundary can be a check rather than a
    comment.
    """
    width, height = 4, 2
    entries = 1 << 3
    # One bit an index, three to a byte with the fourth byte's top bit unused:
    # the reader refuses the width before it looks at a sample, so what the
    # raster holds only has to be a whole number of rows.
    raster = bytes(width * 3 // 8 + 1) * height
    # A ramp that fits in sixteen bits, which is what an entry is.
    colour_map = [index * 2000 for index in range(entries * 3)]

    # Tag, type, values. 3 is a short and 4 is a long.
    tags = [
        (256, 4, [width]),
        (257, 4, [height]),
        (258, 3, [3]),
        (259, 3, [1]),
        (262, 3, [3]),
        (273, 4, [0]),
        (277, 3, [1]),
        (278, 4, [height]),
        (279, 4, [len(raster)]),
        (284, 3, [1]),
        (320, 3, colour_map),
    ]

    def raw(type_: int, values: list[int]) -> bytes:
        return b"".join(
            struct.pack("<H", value) if type_ == 3 else struct.pack("<I", value)
            for value in values
        )

    ifd_size = 2 + 12 * len(tags) + 4
    # Only the colour table is wider than its entry.
    data_at = 8 + ifd_size + len(raw(3, colour_map))
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
    out += raster
    open(path, "wb").write(bytes(out))


def report(name: str, path: str) -> None:
    """Prints what was actually written, read back out of the directory."""
    data = open(path, "rb").read()
    endian = "<" if data[:2] == b"II" else ">"
    offset = struct.unpack_from(endian + "I", data, 4)[0]
    count = struct.unpack_from(endian + "H", data, offset)[0]
    found: dict[int, int] = {}
    items_of: dict[int, int] = {}
    for index in range(count):
        at = offset + 2 + 12 * index
        tag, type_, items = struct.unpack_from(endian + "HHI", data, at)
        if type_ not in (3, 4):
            continue
        size = 2 if type_ == 3 else 4
        if size * items <= 4:
            raw = data[at + 8 : at + 8 + size * items]
        else:
            where = struct.unpack_from(endian + "I", data, at + 8)[0]
            raw = data[where : where + size * items]
        form = "H" if type_ == 3 else "I"
        found[tag] = struct.unpack(endian + form * items, raw)[0]
        items_of[tag] = items
    print(
        f"  {name}: {found.get(256)}x{found.get(257)},"
        f" BitsPerSample={found.get(258)},"
        f" PhotometricInterpretation={found.get(262)},"
        f" Compression={found.get(259)},"
        f" ColorMap values={items_of.get(320)}"
    )


def main() -> None:
    # ---- The palette widths. `-colors` is what decides BitsPerSample: two
    # colours are one bit an index, four are two, and 256 are eight, which is the
    # widest ImageMagick writes.
    for name, colours in [
        ("tiff-palette-1bit.tiff", 2),
        ("tiff-palette-2bit.tiff", 4),
        ("tiff-palette-8bit.tiff", 256),
    ]:
        path = os.path.join(FIXTURES, name)
        magick(SOURCE, "-colors", str(colours), "-type", "Palette", path)
        report(name, path)

    # ---- A width this reader refuses, so the boundary is a check. The probe
    # has to refuse it too: a probe that promised a frame the decode would
    # refuse is the failure this whole tree keeps coming back to.
    path = os.path.join(FIXTURES, "tiff-palette-3bit.tiff")
    write_three_bit(path)
    report("tiff-palette-3bit.tiff", path)

    print(f"  tiff-palette.tiff is the four bit page, written by make-tiff-exr-fixtures.py")


if __name__ == "__main__":
    main()
