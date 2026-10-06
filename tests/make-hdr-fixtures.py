r"""Creates the HDR fixtures used by ``tests/readalpha.vpy``.

Run from the repository root with any Python 3:

    python tests/make-hdr-fixtures.py

HDR is the one format in the plan that is **written here** rather than ported,
so these files are the only specification of what the reader has to do. The
plan's rule has three parts and there is a file for each:

- **`Rgb32F` always**, whatever the samples are, so the frame format does not
  depend on the file.
- **`value * 2^(exponent - 128) / 256`, native-endian**, and a **zero exponent
  byte means black**. ``hdr-exponents.hdr`` walks the exponents that matter:
  nought, one (the subnormal case), the neutral 128, and the largest.
- **The sixteen orientations.** The resolution line's signs say which way the
  scanlines and the pixels run, and a reader has to honour them rather than
  hand out the storage order. ``hdr-orient-*.hdr`` covers the four sign
  combinations in both axis orders.

The three scanline encodings are all here too, because a reader that handles
only one of them reads most real files wrongly: flat (which the corpus generator
used), the new per-component run-length form with its ``2, 2`` header, and the
old repeat-marker form.

The pixels are chosen as raw RGBE bytes rather than converted from a float
picture, so what each file decodes to is arithmetic rather than a round trip.
"""

from __future__ import annotations

import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

WIDTH = 37
HEIGHT = 23

# A narrower picture, so the new run-length form is not used: it is only written
# for a width of eight or more, and a flat scanline that happens to begin 2, 2,
# with a third byte below 128 would be mistaken for its header.
NARROW = 7


def pixels(width: int, height: int) -> list[tuple[int, int, int, int]]:
    """The picture, as raw r, g, b and exponent bytes.

    The exponents walk a small set rather than a single value, so one file
    exercises the branch for nought, the subnormal branch for one, and the
    ordinary conversion.
    """
    out: list[tuple[int, int, int, int]] = []
    for y in range(height):
        for x in range(width):
            red = (x * 255) // (width - 1)
            green = (y * 255) // (height - 1)
            blue = 255 if (x + y) % 8 < 4 else 0
            exponent = [128, 129, 160, 96][(x + y) % 4]
            out.append((red, green, blue, exponent))
    return out


def content_pixels(width: int, height: int) -> list[tuple[int, int, int, int]]:
    """The same picture with one fully black pixel, to hold the zero exponent."""
    out = pixels(width, height)
    out[3] = (0, 0, 0, 0)
    return out


def run_length(plane: bytes) -> bytes:
    """Encodes one component plane in the new per-component form.

    A count above 128 introduces a run of ``count - 128`` copies of the next
    byte; a count of 128 or less introduces that many literal bytes.
    """
    out = bytearray()
    index = 0
    while index < len(plane):
        run = 1
        while index + run < len(plane) and plane[index + run] == plane[index] and run < 127:
            run += 1
        if run > 1:
            out += bytes([128 + run, plane[index]])
            index += run
            continue
        start = index
        while index < len(plane) and index - start < 128:
            if index + 1 < len(plane) and plane[index + 1] == plane[index]:
                break
            index += 1
        count = index - start
        out += bytes([count]) + plane[start:index]
    return bytes(out)


def scanline_rle(row: list[tuple[int, int, int, int]]) -> bytes:
    """One scanline in the new run-length form, or flat if it is too wide."""
    width = len(row)
    out = bytearray([2, 2, (width >> 8) & 0x7F, width & 0xFF])
    for component in range(4):
        out += run_length(bytes(pixel[component] for pixel in row))
    return bytes(out)


def scanline_flat(row: list[tuple[int, int, int, int]]) -> bytes:
    """One scanline as raw bytes, with old-style repeat markers.

    The old form repeats a pixel with ``1, 1, 1, count``; this writer emits that
    marker for a repeated pixel and the four raw bytes otherwise.
    """
    out = bytearray()
    index = 0
    while index < len(row):
        run = 1
        while index + run < len(row) and row[index + run] == row[index] and run < 255:
            run += 1
        if run > 1:
            out += bytes([1, 1, 1, run])
            index += run
        else:
            out += bytes(row[index])
            index += 1
    return bytes(out)


def write(name: str, resolution: str, rows: list[bytes], extra: bytes = b"") -> None:
    header = (
        b"#?RADIANCE\n"
        b"# a comment line, which a reader skips\n"
        b"FORMAT=32-bit_rle_rgbe\n"
        b"EXPOSURE=1.0000000000000\n"
        + extra
        + b"\n"
        + resolution.encode()
        + b"\n"
    )
    target = os.path.join(FIXTURES, name)
    data = header + b"".join(rows)
    with open(target, "wb") as handle:
        handle.write(data)
    print(f"  + {name} ({len(data)} bytes)")


def main() -> int:
    os.makedirs(FIXTURES, exist_ok=True)
    wide = pixels(WIDTH, HEIGHT)
    rows_wide = [wide[y * WIDTH : (y + 1) * WIDTH] for y in range(HEIGHT)]
    narrow = content_pixels(NARROW, HEIGHT)
    rows_narrow = [narrow[y * NARROW : (y + 1) * NARROW] for y in range(HEIGHT)]

    # ---- The new per-component run-length form, which is what a width of eight
    # or more uses.
    write(
        "hdr-rle.hdr",
        f"-Y {HEIGHT} +X {WIDTH}",
        [scanline_rle(row) for row in rows_wide],
    )
    # ---- The old repeat-marker form, on a narrow picture so the new header is
    # not expected.
    write(
        "hdr-oldrle.hdr",
        f"-Y {HEIGHT} +X {NARROW}",
        [scanline_flat(row) for row in rows_narrow],
    )
    # ---- A flat scanline with no markers at all, which is what the research
    # corpus wrote.
    write(
        "hdr-flat.hdr",
        f"-Y {HEIGHT} +X {NARROW}",
        [bytes(b for pixel in row for b in pixel) for row in rows_narrow],
    )

    # ---- The exponents. A narrow flat file whose first pixel is not 2, 2.
    exponent_rows: list[tuple[int, int, int, int]] = [
        (255, 255, 255, 0),
        (1, 1, 1, 1),
        (255, 128, 64, 128),
        (255, 255, 255, 129),
        (255, 255, 255, 160),
        (255, 255, 255, 96),
        (255, 255, 255, 255),
    ]
    write(
        "hdr-exponents.hdr",
        f"-Y 1 +X {len(exponent_rows)}",
        [bytes(b for pixel in exponent_rows for b in pixel)],
    )
    # ---- A header with an unknown field, which a reader has to skip rather
    # than refuse.
    write(
        "hdr-fields.hdr",
        f"-Y {HEIGHT} +X {NARROW}",
        [scanline_flat(row) for row in rows_narrow],
        extra=b"SOFTWARE=a reader has to skip an unknown field\n",
    )

    # ---- The resolution line's signs, in both axis orders. A reader that
    # ignores them hands out the storage order instead of the picture.
    for vertical in ("-Y", "+Y"):
        for horizontal in ("+X", "-X"):
            # The signs are applied to the pixels and the encoding follows, so
            # the file's raster is stored the way its resolution line says.
            rows = [list(row) for row in rows_wide]
            if vertical == "+Y":
                rows = list(reversed(rows))
            if horizontal == "-X":
                rows = [list(reversed(row)) for row in rows]
            direction = "top" if vertical == "-Y" else "bottom"
            sideways = "left" if horizontal == "+X" else "right"
            name = f"hdr-orient-{direction}-{sideways}.hdr"
            write(name, f"{vertical} {HEIGHT} {horizontal} {WIDTH}", [scanline_rle(r) for r in rows])
    print(f"wrote the HDR fixtures to {FIXTURES}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
