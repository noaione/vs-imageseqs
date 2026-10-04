#!/usr/bin/env python3
r"""Writes the hand-made tiff fixtures for the separated-ink path.

    python tests/make-tiff-cmyk-fixtures.py

Every file is written by hand rather than by an encoder, for the same reason
`make-gif-fixtures.py` and `make-png-fixtures.py` are: the sample values have to
be known exactly, because the tests assert the conversion arithmetic rather than
an encoder's idea of it. Nothing here depends on a tool being on `PATH`.

A classic tiff is a header, one image file directory, the values too wide to sit
inside an entry, and the strips, all in one byte order. Little endian throughout,
which is the `II` every one of these starts with.

The inks below are picked so that each part of

    channel = (maximum - ink) * (maximum - k) / maximum

is pinned by some pixel: no ink at all, all of it, black alone, a midpoint whose
product truncates, and one pixel with four different inks. The sixteen-bit file
holds the same inks scaled by 257, so the two describe the same colours.
"""

from __future__ import annotations

import pathlib
import struct

FIXTURES = pathlib.Path(__file__).parent / "fixtures"

WIDTH = 4
HEIGHT = 2

# c, m, y, k for each of the eight pixels, row major. Zero is paper and 255 is
# all of the ink, which is the convention the conversion reads.
INKS_8 = [
    (0, 0, 0, 0),
    (255, 255, 255, 0),
    (0, 0, 0, 255),
    (128, 0, 0, 0),
    (0, 0, 0, 128),
    (128, 64, 32, 16),
    (255, 0, 0, 0),
    (10, 20, 30, 40),
]

# The same inks at the width of a sixteen-bit sample.
INKS_16 = [tuple(ink * 257 for ink in pixel) for pixel in INKS_8]

# One alpha value per pixel, for the file that carries a fifth sample. The first
# is fully transparent and the second fully opaque, so a reader that dropped the
# sample or read it from the wrong place cannot pass.
ALPHAS_8 = [0, 255, 128, 1, 254, 64, 192, 32]

TYPE_SHORT = 3
TYPE_LONG = 4


def entry(tag: int, type_: int, values: list[int]) -> tuple[int, int, int, bytes]:
    """One directory entry: the tag, its type, its count and its raw value."""
    raw = b"".join(
        struct.pack("<H", value)
        if type_ == TYPE_SHORT
        else struct.pack("<I", value)
        for value in values
    )
    return tag, type_, len(values), raw


def write_tiff(
    name: str,
    *,
    bits: int,
    samples: int,
    strips: list[bytes],
    planar: int = 1,
    photometric: int = 5,
    extra: list[int] | None = None,
) -> None:
    """The header, one directory and the strips of one image.

    `strips` holds the bytes of each strip as written: one for a chunky file, one
    per channel for a planar one.
    """
    # One strip per plane, or one strip for the whole chunky picture: either way
    # a strip holds every row it covers, so RowsPerStrip is the height.
    entries = [
        entry(256, TYPE_LONG, [WIDTH]),  # ImageWidth
        entry(257, TYPE_LONG, [HEIGHT]),  # ImageLength
        entry(258, TYPE_SHORT, [bits] * samples),  # BitsPerSample
        entry(259, TYPE_SHORT, [1]),  # Compression: none
        entry(262, TYPE_SHORT, [photometric]),  # PhotometricInterpretation
        # StripOffsets is filled in once the strips have an offset, so it starts
        # at zero: only its width matters until then.
        entry(273, TYPE_LONG, [0] * len(strips)),
        entry(277, TYPE_SHORT, [samples]),  # SamplesPerPixel
        entry(278, TYPE_LONG, [HEIGHT]),  # RowsPerStrip
        entry(279, TYPE_LONG, [len(strip) for strip in strips]),  # StripByteCounts
        entry(284, TYPE_SHORT, [planar]),  # PlanarConfiguration
        entry(332, TYPE_SHORT, [1]),  # InkSet: CMYK
    ]
    if extra:
        entries.append(entry(338, TYPE_SHORT, extra))  # ExtraSamples
    entries.sort(key=lambda entry: entry[0])

    ifd_size = 2 + 12 * len(entries) + 4
    # Every value wider than an entry sits after the directory, in tag order,
    # which is what the offsets below are counted through.
    wide = [raw for _, _, _, raw in entries if len(raw) > 4]
    data_start = 8 + ifd_size + sum(len(raw) for raw in wide)

    offsets = []
    at = data_start
    for strip in strips:
        offsets.append(at)
        at += len(strip)
    entries = [
        (tag, type_, count, entry(tag, type_, offsets)[3] if tag == 273 else raw)
        for tag, type_, count, raw in entries
    ]

    out = bytearray()
    out += struct.pack("<2sHI", b"II", 42, 8)
    out += struct.pack("<H", len(entries))
    wide = []
    for tag, type_, count, raw in entries:
        out += struct.pack("<HHI", tag, type_, count)
        if len(raw) <= 4:
            out += raw.ljust(4, b"\0")
        else:
            # The offset is into the file, so it counts from the header.
            out += struct.pack("<I", 8 + ifd_size + sum(len(blob) for blob in wide))
            wide.append(raw)
    out += struct.pack("<I", 0)
    for blob in wide:
        out += blob
    for strip in strips:
        out += strip

    path = FIXTURES / name
    path.write_bytes(bytes(out))
    print(f"{path.name}: {len(out)} bytes, {WIDTH}x{HEIGHT}, {bits} bits, {samples} samples")


def chunky(inks: list[tuple[int, ...]], bits: int) -> bytes:
    """One strip holding every pixel's samples one after another."""
    out = bytearray()
    for pixel in inks:
        for sample in pixel:
            out += struct.pack("<H", sample) if bits == 16 else bytes([sample])
    return bytes(out)


def chunky_alpha(inks: list[tuple[int, ...]], alphas: list[int]) -> bytes:
    """One strip holding every pixel's four inks and then its alpha."""
    out = bytearray()
    for pixel, alpha in zip(inks, alphas, strict=True):
        out += bytes([*pixel, alpha])
    return bytes(out)


def planar(inks: list[tuple[int, ...]], bits: int) -> list[bytes]:
    """One strip per channel, each holding that channel of every pixel."""
    planes = []
    for channel in range(len(inks[0])):
        out = bytearray()
        for pixel in inks:
            sample = pixel[channel]
            out += struct.pack("<H", sample) if bits == 16 else bytes([sample])
        planes.append(bytes(out))
    return planes


def main() -> None:
    write_tiff(
        "tiff-cmyk8.tiff",
        bits=8,
        samples=4,
        strips=[chunky(INKS_8, 8)],
    )
    write_tiff(
        "tiff-cmyk16.tiff",
        bits=16,
        samples=4,
        strips=[chunky(INKS_16, 16)],
    )
    write_tiff(
        "tiff-cmyka8.tiff",
        bits=8,
        samples=5,
        strips=[chunky_alpha(INKS_8, ALPHAS_8)],
        # 2 is unassociated alpha: the samples are not premultiplied, which is
        # what a frame's own alpha plane holds.
        extra=[2],
    )
    # The same pixels as the first file, held one channel at a time, so the
    # reorder a planar file needs is checked against the chunky answer.
    write_tiff(
        "tiff-cmyk8-planar.tiff",
        bits=8,
        samples=4,
        strips=planar(INKS_8, 8),
        planar=2,
    )


if __name__ == "__main__":
    main()
