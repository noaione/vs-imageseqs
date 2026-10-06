#!/usr/bin/env python3
"""Writes the uncompressed spellings of the WebP TIFF fixtures.

The pictures are written here as the png a reader can look at and as the uncompressed
tiffs that libtiff's own `tiffcp` reads to make the compressed spellings:

    tiffcp -c webp tests/fixtures/tiff-source.tiff tests/fixtures/tiff-webp.tiff
    tiffcp -c none tests/fixtures/tiff-webp.tiff tests/fixtures/tiff-webp-uncompressed.tiff
    tiffcp -c zstd tests/fixtures/tiff-source.tiff tests/fixtures/tiff-webp-unknown.tiff
    tiffcp -c jpeg tests/fixtures/tiff-source.tiff tests/fixtures/tiff-jpeg-ycbcr.tiff
    tiffcp -c webp tests/fixtures/tiff-alpha-source.tiff tests/fixtures/tiff-webp-alpha.tiff
    tiffcp -c none tests/fixtures/tiff-webp-alpha.tiff tests/fixtures/tiff-webp-alpha-uncompressed.tiff

`tiff-webp.tiff` is libtiff's own webp page, and `tiff-webp-uncompressed.tiff` is
libtiff's own decode of it -- `tiffcp -c webp` is lossy, so the pair is made in that
direction and the acceptance compares this reader's decode against libtiff's rather
than against a second copy of the same raster. `tiff-webp-unknown.tiff` is a real
ZSTD-compressed page whose code this reader does not take, which is what pins the
refusal by name.

`tiff-alpha-source.tiff` is the same picture with a fourth, unassociated alpha sample,
so `tiff-webp-alpha.tiff` is a webp strip whose bitstream carries four samples a pixel
and `tiff-webp-alpha-uncompressed.tiff` is libtiff's decode of it. That page is what
used to be read wrongly: the strip decode asked the library for three samples a pixel
and gave it a four sample stride, which left the fourth sample of every pixel zero and
shifted every row, so the pair is a regression test rather than only a coverage one.
"""

from __future__ import annotations

import os
import struct
import zlib
from binascii import crc32
from pathlib import Path

ROOT = Path(__file__).resolve().parent
FIXTURES = ROOT / "fixtures"
WIDTH, HEIGHT = 3, 2
# Six distinct colours, so a decoded sample that is off is a sample that shows.
PIXELS = [
    (10, 20, 30),
    (40, 50, 60),
    (70, 80, 90),
    (100, 110, 120),
    (130, 140, 150),
    (160, 170, 180),
]
# The fourth sample of the same picture, in a different range from the colours
# so that a row which moved is a row that shows.
ALPHA = [40, 80, 120, 160, 200, 240]


def raster() -> bytes:
    """The picture's samples, three bytes a pixel."""
    return bytes(value for pixel in PIXELS for value in pixel)


def raster_alpha() -> bytes:
    """The picture's samples, four bytes a pixel with the alpha last."""
    return bytes(value for index, pixel in enumerate(PIXELS) for value in (*pixel, ALPHA[index]))


def write_source_png(path: Path) -> None:
    """The picture `cwebp` encodes, written with only the standard library."""

    def chunk(kind: bytes, data: bytes) -> bytes:
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", crc32(body) & 0xFFFFFFFF)

    rows = bytearray()
    for y in range(HEIGHT):
        rows.append(0)
        rows.extend(raster()[y * WIDTH * 3 : (y + 1) * WIDTH * 3])
    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(rows)))
        + chunk(b"IEND", b"")
    )


def write_tiff(
    path: Path,
    strip: bytes,
    compression: int,
    samples: int = 3,
    extra: int | None = None,
) -> None:
    """A little-endian classic TIFF holding `strip` as its one strip.

    `samples` is the page's `SamplesPerPixel`, and `extra` its `ExtraSamples` value,
    which is what says a fourth sample is alpha rather than a channel with a colour
    of its own.
    """
    entries = [
        (256, 3, 1, WIDTH),
        (257, 3, 1, HEIGHT),
        (258, 3, samples, None),
        (259, 3, 1, compression),
        (262, 3, 1, 2),
        (273, 4, 1, None),
        (277, 3, 1, samples),
        (278, 3, 1, HEIGHT),
        (279, 4, 1, len(strip)),
        (284, 3, 1, 1),
    ]
    if extra is not None:
        entries.append((338, 3, 1, extra))
    bit_depth = struct.pack("<" + "H" * samples, *([8] * samples))
    ifd = 8
    extra_offset = ifd + 2 + len(entries) * 12 + 4
    strip_offset = extra_offset + len(bit_depth)
    document = bytearray(b"II" + struct.pack("<HI", 42, ifd))
    document += struct.pack("<H", len(entries))
    for tag, kind, count, value in entries:
        if tag == 258:
            value = extra_offset
        elif tag == 273:
            value = strip_offset
        document += struct.pack("<HHII", tag, kind, count, value)
    document += struct.pack("<I", 0)
    document += bit_depth
    document += strip
    path.write_bytes(bytes(document))


def main() -> int:
    os.makedirs(FIXTURES, exist_ok=True)
    write_source_png(FIXTURES / "tiff-webp-source.png")
    write_tiff(FIXTURES / "tiff-source.tiff", raster(), 1)
    # One is unassociated alpha and two is associated alpha; the fourth sample
    # of this picture is not premultiplied, so the page states two.
    write_tiff(FIXTURES / "tiff-alpha-source.tiff", raster_alpha(), 1, samples=4, extra=2)
    print("wrote the source pictures and their uncompressed tiffs")
    print("the compressed spellings come from the tiffcp commands in the header")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
