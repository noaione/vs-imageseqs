r"""Creates the PNM fixtures used by ``tests/readalpha.vpy``.

Run from the repository root with any Python 3:

    python tests/make-pnm-fixtures.py

PNM is ported rather than taken from a crate, so these files are the
specification of what the port has to keep. Written here rather than by
ImageMagick, for the reason the BMP, TGA and DDS fixtures are: the encoder does
not honour the options that select these subtypes, and a fixture that is not the
subtype it claims is worse than no fixture.

The plan's rule has three parts, and there is a file for each:

- **`P1` to `P7`**, which is seven subtypes rather than one: three ASCII
  rasters, three binary ones, and the tagged container.
- **An ASCII raster whose comments are refused.** A comment is legal between
  header fields; inside the raster of an ASCII file it is not, and the two
  ``pnm-comment*`` fixtures hold that difference.
- **Samples rescaled through `f32` whenever `MAXVAL` is not one less than a
  power of two.** ``pnm-p7-maxval31.pam`` is the case: its samples are not a
  shift of the frame's word, so the reader has to rescale them rather than
  widen them.

``tests/fixtures/gray.pgm`` and ``rgb.ppm`` predate this script and are left
alone: the latter is read by the colour-metadata section for its size.
"""

from __future__ import annotations

import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

WIDTH = 37
HEIGHT = 23


def samples() -> list[tuple[int, int, int]]:
    """The picture every fixture holds: a ramp with a hard diagonal."""
    pixels: list[tuple[int, int, int]] = []
    for y in range(HEIGHT):
        for x in range(WIDTH):
            red = (x * 255) // (WIDTH - 1)
            green = (y * 255) // (HEIGHT - 1)
            blue = 255 if (x + y) % 8 < 4 else 0
            pixels.append((red, green, blue))
    return pixels


def write(name: str, text: str | bytes) -> None:
    target = os.path.join(FIXTURES, name)
    data = text.encode("ascii") if isinstance(text, str) else text
    with open(target, "wb") as handle:
        handle.write(data)
    print(f"  + {name} ({len(data)} bytes)")


def scale(value: int, maxval: int) -> int:
    """Maps a 0..255 sample onto 0..maxval."""
    return (value * maxval + 127) // 255


def main() -> int:
    os.makedirs(FIXTURES, exist_ok=True)
    pixels = samples()

    # ---- P1 and P4, one bit a sample, where 1 is black. The ASCII form writes
    # a digit a sample; the binary one packs eight samples into a byte, most
    # significant bit first, and pads each row to a whole byte.
    bits = [1 if (r + g + b) / 3 < 128 else 0 for r, g, b in pixels]
    write(
        "pnm-p1.pbm",
        "P1\n"
        + f"# a comment between two header fields, which is legal\n{WIDTH} {HEIGHT}\n"
        + "\n".join(
            " ".join(str(bit) for bit in bits[y * WIDTH : (y + 1) * WIDTH])
            for y in range(HEIGHT)
        )
        + "\n",
    )
    packed = bytearray()
    for y in range(HEIGHT):
        row = bytearray()
        for x in range(0, WIDTH, 8):
            byte = 0
            for bit in range(8):
                if x + bit < WIDTH and bits[y * WIDTH + x + bit]:
                    byte |= 0x80 >> bit
            row.append(byte)
        packed += row
    write("pnm-p4.pbm", f"P4\n{WIDTH} {HEIGHT}\n".encode() + bytes(packed))

    # ---- P2 and P5, grey at eight bits and at sixteen.
    grey = [(r + g + b) // 3 for r, g, b in pixels]
    write(
        "pnm-p2.pgm",
        f"P2\n{WIDTH} {HEIGHT}\n255\n"
        + "\n".join(
            " ".join(str(grey[y * WIDTH + x]) for x in range(WIDTH))
            for y in range(HEIGHT)
        )
        + "\n",
    )
    write("pnm-p5.pgm", f"P5\n{WIDTH} {HEIGHT}\n255\n".encode() + bytes(grey))
    wide_grey = b"".join(scale(v, 65535).to_bytes(2, "big") for v in grey)
    write(
        "pnm-p5-16.pgm",
        f"P5\n{WIDTH} {HEIGHT}\n65535\n".encode() + wide_grey,
    )

    # ---- P3 and P6, colour at eight bits and at sixteen.
    write(
        "pnm-p3.ppm",
        f"P3\n{WIDTH} {HEIGHT}\n255\n"
        + "\n".join(
            " ".join(
                f"{pixels[y * WIDTH + x][0]} {pixels[y * WIDTH + x][1]} "
                f"{pixels[y * WIDTH + x][2]}"
                for x in range(WIDTH)
            )
            for y in range(HEIGHT)
        )
        + "\n",
    )
    write(
        "pnm-p6.ppm",
        f"P6\n{WIDTH} {HEIGHT}\n255\n".encode()
        + b"".join(bytes(pixel) for pixel in pixels),
    )
    write(
        "pnm-p6-16.ppm",
        f"P6\n{WIDTH} {HEIGHT}\n65535\n".encode()
        + b"".join(scale(v, 65535).to_bytes(2, "big") for pixel in pixels for v in pixel),
    )

    # ---- P7, the tagged container, whose tuple type is the whole point of the
    # subtype. Four spellings, so the tuple type is what varies.
    def pam(name: str, tuple_type: str, depth: int, body: bytes, maxval: int = 255) -> None:
        header = (
            f"P7\nWIDTH {WIDTH}\nHEIGHT {HEIGHT}\nDEPTH {depth}\n"
            f"MAXVAL {maxval}\nTUPLTYPE {tuple_type}\nENDHDR\n"
        ).encode()
        write(name, header + body)

    pam("pnm-p7-rgb.pam", "RGB", 3, b"".join(bytes(pixel) for pixel in pixels))
    pam(
        "pnm-p7-rgba.pam",
        "RGB_ALPHA",
        4,
        b"".join(
            bytes([r, g, b, 255 if (x // 4) % 2 == 0 else 64])
            for y in range(HEIGHT)
            for x, (r, g, b) in enumerate(pixels[y * WIDTH : (y + 1) * WIDTH])
        ),
    )
    pam("pnm-p7-gray.pam", "GRAYSCALE", 1, bytes(grey))
    pam("pnm-p7-gray-alpha.pam", "GRAYSCALE_ALPHA", 2,
        b"".join(bytes([v, 255 - v]) for v in grey))
    # MAXVAL 31 is not one less than a power of two, so a sample is not a shift
    # of the frame's word and the reader has to rescale it through f32.
    pam(
        "pnm-p7-maxval31.pam",
        "GRAYSCALE",
        1,
        bytes(scale(v, 31) for v in grey),
        maxval=31,
    )
    pam(
        "pnm-p7-rgb16.pam",
        "RGB",
        3,
        b"".join(scale(v, 1023).to_bytes(2, "big") for pixel in pixels for v in pixel),
        maxval=1023,
    )

    # ---- A comment between header fields is legal in every subtype; one inside
    # an ASCII raster is not, and the reader refuses it rather than skipping it.
    write(
        "pnm-comment.pgm",
        f"P5\n# a comment between two header fields\n{WIDTH} # and another\n{HEIGHT}\n255\n".encode()
        + bytes(grey),
    )
    write(
        "pnm-ascii-comment.pgm",
        f"P2\n{WIDTH} {HEIGHT}\n255\n"
        + "\n".join(
            " ".join(
                ("# not allowed here" if x == 3 and y == 1 else str(grey[y * WIDTH + x]))
                for x in range(WIDTH)
            )
            for y in range(HEIGHT)
        )
        + "\n",
    )

    print(f"wrote the PNM fixtures to {FIXTURES}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
