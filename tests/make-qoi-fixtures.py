r"""Creates the QOI fixtures used by ``tests/readalpha.vpy``.

Run from the repository root with any Python 3:

    python tests/make-qoi-fixtures.py

QOI is small enough to write here, and writing it is what makes the fixtures
reproducible without a tool: fourteen bytes of header, then a stream of
opcodes, then seven zero bytes and a ``1``. The encoder below emits literal
``QOI_OP_RGB``/``QOI_OP_RGBA`` operations and ``QOI_OP_RUN`` for repeated
samples, which is a conforming stream that the reference implementation reads
back; check a regenerated file with

    magick identify tests/fixtures/qoi-rgb8.qoi

Three files, one per thing the migration has to keep:

``qoi-rgb8.qoi``
    Three channels, the sRGB colours flag. The alpha clip of a read of this
    file is opaque, because the file states no alpha: the header's channel
    count is what decides that, not the samples.

``qoi-rgba8.qoi``
    Four channels with alpha that varies and is not all 255, so the alpha clip
    is a picture rather than a constant. ``ImgSeqOriginalColorType`` is
    ``rgba8`` for this one and ``rgb8`` for the other two.

``qoi-linear.qoi``
    Three channels with the header's colours flag set to ``1``, which the
    specification calls linear. That field is *informative* -- it does not
    change a sample -- and it must not become a ``_Transfer`` frame property:
    QOI states no colour description, and a parity migration may not turn this
    flag into one. The file holds the same picture as ``qoi-rgb8.qoi``, so a
    check can read both and find no property here that the other does not have.

The samples are a red to blue ramp with a repeated block, so the stream
exercises a run opcode as well as the literal ones, and so the alpha plane is
different from every colour plane.
"""

from __future__ import annotations

import struct
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures"

WIDTH = 8
HEIGHT = 6

RGB = 3
RGBA = 4

SRGB = 0
LINEAR = 1

# QOI's opcodes, from the specification's table. Only the three this encoder
# emits are named; a decoder reads the high two bits to tell them apart.
OP_RGB = 0xFE
OP_RGBA = 0xFF
OP_RUN = 0xC0

# A run opcode holds one to sixty two repeats of the pixel before it.
RUN_MAX = 62

# Seven zero bytes and a one close a stream.
END_MARKER = bytes(7) + b"\x01"


def samples() -> list[tuple[int, int, int, int]]:
    """The picture, as r,g,b,a per pixel, row major.

    Every fourth column repeats the pixel before it, so a row has runs in it
    and the next row starts on a different colour.
    """
    pixels: list[tuple[int, int, int, int]] = []
    for y in range(HEIGHT):
        for x in range(WIDTH):
            column = x - (x % 2)
            red = 255 - column * 32
            green = (y * 40) % 256
            blue = column * 32
            alpha = 40 + (x + y * WIDTH) * 4
            pixels.append((red & 0xFF, green, blue & 0xFF, alpha & 0xFF))
    return pixels


def write(path: Path, pixels: list[tuple[int, int, int, int]], channels: int, colorspace: int) -> None:
    """Writes a QOI file holding `pixels`."""
    out = bytearray(b"qoif")
    out += struct.pack(">IIBB", WIDTH, HEIGHT, channels, colorspace)

    previous: tuple[int, int, int, int] | None = None
    pending = 0

    def flush() -> None:
        """Emits the repeats counted so far."""
        nonlocal pending
        while pending:
            run = min(pending, RUN_MAX)
            out.append(OP_RUN | (run - 1))
            pending -= run

    for pixel in pixels:
        if pixel == previous:
            pending += 1
            # A run has to be closed before anything else is written, because
            # the opcode carries no colour of its own.
            if pending == RUN_MAX:
                flush()
            continue
        flush()
        if channels == RGBA:
            out += bytes([OP_RGBA, *pixel])
        else:
            out += bytes([OP_RGB, pixel[0], pixel[1], pixel[2]])
        previous = pixel
    flush()
    out += END_MARKER

    with path.open("wb") as handle:
        handle.write(out)


def main() -> None:
    FIXTURES.mkdir(parents=True, exist_ok=True)
    pixels = samples()
    write(FIXTURES / "qoi-rgb8.qoi", pixels, RGB, SRGB)
    write(FIXTURES / "qoi-rgba8.qoi", pixels, RGBA, SRGB)
    write(FIXTURES / "qoi-linear.qoi", pixels, RGB, LINEAR)
    print(f"wrote the QOI fixtures to {FIXTURES}")


if __name__ == "__main__":
    main()
