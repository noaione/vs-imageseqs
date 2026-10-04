r"""Creates the DDS fixtures used by ``tests/readalpha.vpy``.

Run from the repository root with any Python 3:

    python tests/make-dds-fixtures.py

DDS is ported rather than taken from a crate, so these files are the
specification of what the port has to keep. Written here rather than by
ImageMagick, for the reason the BMP and TGA fixtures are: the encoder does not
honour the options that select these subtypes, and a fixture that is not the
subtype it claims is worse than no fixture.

One file per row of the plan's parity rule: **only DXT1, DXT3 and DXT5 and their
DX10 equivalents are accepted, the width and height must be multiples of four,
and mipmaps, other cube faces and other volume slices are ignored without an
error**. The last two fixtures exist for the *ignored* half of that rule, which
is the half a reader can get wrong by refusing a file it should read.

``tests/fixtures/alpha-dds.dds`` predates this script: it is a four by four DXT5
icon-sized file the validator already checks, and it is left alone.
"""

from __future__ import annotations

import os
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

WIDTH = 40
HEIGHT = 24

# The header flags a reader requires: caps, height, width and pixel format.
REQUIRED_FLAGS = 0x1 | 0x2 | 0x4 | 0x1000
# Linearsize and the mipmap count are optional but valid.
EXTRA_FLAGS = 0x80000
PIXEL_FORMAT_FOURCC = 0x4

# DXGI formats: BC1 is 70 to 72, BC2 73 to 75 and BC3 76 to 78, with the last
# of each triple the sRGB spelling.
DXGI_BC1_UNORM = 71
DXGI_BC3_UNORM = 77

CAPS_TEXTURE = 0x1000
# The three bits a reader has to ignore rather than refuse.
CAPS2_CUBEMAP = 0x200
CAPS2_VOLUME = 0x200000


def rgb565(red: int, green: int, blue: int) -> int:
    """Packs eight bit channels into the five-six-five a DXT colour block uses."""
    return ((red >> 3) << 11) | ((green >> 2) << 5) | (blue >> 3)


def colour_block(color0: int, color1: int, indices: list[int]) -> bytes:
    """One eight byte DXT colour block: two colours and sixteen two-bit indices."""
    table = 0
    for index, value in enumerate(indices):
        table |= (value & 3) << (index * 2)
    return struct.pack("<HHI", color0, color1, table)


def dxt1_block(pixels: list[tuple[int, int, int, int]]) -> bytes:
    """A four by four DXT1 block, which has no alpha of its own.

    The two endpoints are taken from the first and last pixel so the block's
    colours span the picture, and every pixel is indexed to whichever of the
    four entries is nearest. That is what an encoder does, and it means the
    decoded block is close to the source rather than arbitrary.
    """
    first, last = pixels[0], pixels[-1]
    color0 = rgb565(first[0], first[1], first[2])
    color1 = rgb565(last[0], last[1], last[2])
    if color0 <= color1:
        color0, color1 = color1, color0
    steps = [(first[0], first[1], first[2]), (last[0], last[1], last[2])]
    steps.append(
        tuple((2 * steps[0][i] + steps[1][i] + 1) // 3 for i in range(3))  # type: ignore[arg-type]
    )
    steps.append(
        tuple((steps[0][i] + 2 * steps[1][i] + 1) // 3 for i in range(3))  # type: ignore[arg-type]
    )
    indices = []
    for red, green, blue, _ in pixels:
        distances = [
            (red - step[0]) ** 2 + (green - step[1]) ** 2 + (blue - step[2]) ** 2
            for step in steps
        ]
        indices.append(distances.index(min(distances)))
    return colour_block(color0, color1, indices)


def dxt5_block(pixels: list[tuple[int, int, int, int]]) -> bytes:
    """A four by four DXT5 block: an alpha block, then a colour block.

    The alpha endpoints are the first and last pixel's alpha, and every pixel is
    indexed to the nearest of the eight table entries, exactly as the decoder
    builds them. Which of the two table modes is used therefore depends on the
    picture rather than on the writer.
    """
    alpha0, alpha1 = pixels[0][3], pixels[-1][3]
    table = [alpha0, alpha1, 0, 0, 0, 0, 0, 255]
    if alpha0 > alpha1:
        for i in range(2, 8):
            table[i] = ((8 - i) * alpha0 + (i - 1) * alpha1) // 7
    else:
        for i in range(2, 6):
            table[i] = ((6 - i) * alpha0 + (i - 1) * alpha1) // 5
    indices = [
        min(range(8), key=lambda i, a=alpha: (abs(table[i] - a), i))
        for _, _, _, alpha in pixels
    ]
    packed = 0
    for index, value in enumerate(indices):
        packed |= value << (index * 3)
    alpha_bytes = bytes([alpha0, alpha1]) + packed.to_bytes(6, "little")
    return alpha_bytes + dxt1_block(pixels)


def dxt3_block(pixels: list[tuple[int, int, int, int]]) -> bytes:
    """A four by four DXT3 block: explicit four bit alpha, then a colour block."""
    packed = 0
    for index, pixel in enumerate(pixels):
        packed |= (pixel[3] >> 4) << (index * 4)
    return packed.to_bytes(8, "little") + dxt1_block(pixels)


def samples() -> list[tuple[int, int, int, int]]:
    """The picture every fixture holds: a ramp with a diagonal and an alpha step.

    The alpha is deliberately not monotonic, so a DXT5 block's two endpoint
    alphas fall on both sides of the eight-entry table's split and both of its
    interpolation modes are exercised by the same file.
    """
    pixels: list[tuple[int, int, int, int]] = []
    for y in range(HEIGHT):
        for x in range(WIDTH):
            red = (x * 255) // (WIDTH - 1)
            green = (y * 255) // (HEIGHT - 1)
            blue = 255 if (x + y) % 8 < 4 else 0
            alpha = 255 if (x // 4 + y // 4) % 2 == 0 else 40
            pixels.append((red, green, blue, alpha))
    return pixels


def blocks(pixels: list[tuple[int, int, int, int]], encode) -> bytes:
    """Every four by four block of the picture, in row-major block order."""
    out = bytearray()
    for block_y in range(HEIGHT // 4):
        for block_x in range(WIDTH // 4):
            block = [
                pixels[(block_y * 4 + row) * WIDTH + block_x * 4 + column]
                for row in range(4)
                for column in range(4)
            ]
            out += encode(block)
    return bytes(out)


def header(
    width: int,
    height: int,
    *,
    fourcc: bytes,
    linear_size: int,
    mipmaps: int = 0,
    caps2: int = 0,
    dx10: bytes = b"",
) -> bytes:
    """The one hundred and twenty-four byte header, then the DX10 one if any."""
    flags = REQUIRED_FLAGS | (EXTRA_FLAGS if linear_size else 0)
    if mipmaps:
        flags |= 0x20000
    out = struct.pack("<4sIIIIIII", b"DDS ", 124, flags, height, width, linear_size, 0, mipmaps)
    # Eleven reserved words.
    out += bytes(44)
    out += struct.pack("<II4s5I", 32, PIXEL_FORMAT_FOURCC, fourcc, 0, 0, 0, 0, 0)
    out += struct.pack("<IIIII", CAPS_TEXTURE | 0x8, caps2, 0, 0, 0)
    return out + dx10


def write(name: str, data: bytes) -> None:
    target = os.path.join(FIXTURES, name)
    with open(target, "wb") as handle:
        handle.write(data)
    print(f"  + {name} ({len(data)} bytes)")


def main() -> int:
    os.makedirs(FIXTURES, exist_ok=True)
    pixels = samples()

    # ---- The three variants, each four bytes a block side.
    for name, fourcc, encode, per_block in [
        ("dds-dxt1.dds", b"DXT1", dxt1_block, 8),
        ("dds-dxt3.dds", b"DXT3", dxt3_block, 16),
        ("dds-dxt5.dds", b"DXT5", dxt5_block, 16),
    ]:
        body = blocks(pixels, encode)
        write(
            name,
            header(WIDTH, HEIGHT, fourcc=fourcc, linear_size=len(body)) + body,
        )
        assert len(body) == (WIDTH // 4) * (HEIGHT // 4) * per_block

    # ---- The DX10 spelling of two of them, where the variant comes from a DXGI
    # format number rather than from the four character code.
    for name, format_number, encode in [
        ("dds-dx10-bc1.dds", DXGI_BC1_UNORM, dxt1_block),
        ("dds-dx10-bc3.dds", DXGI_BC3_UNORM, dxt5_block),
    ]:
        body = blocks(pixels, encode)
        dx10 = struct.pack("<IIIII", format_number, 3, 0, 1, 0)
        write(
            name,
            header(WIDTH, HEIGHT, fourcc=b"DX10", linear_size=len(body), dx10=dx10) + body,
        )

    # ---- The three things a reader has to ignore rather than refuse: a stated
    # mipmap count, the cube map bit and the volume texture bit. The extra
    # surfaces those promise are simply not there, which is the case the rule is
    # about: what is read is the first surface and nothing more.
    body = blocks(pixels, dxt5_block)
    write(
        "dds-dxt5-ignored.dds",
        header(
            WIDTH,
            HEIGHT,
            fourcc=b"DXT5",
            linear_size=len(body),
            mipmaps=3,
            caps2=CAPS2_CUBEMAP | CAPS2_VOLUME,
        )
        + body,
    )

    print(f"wrote the DDS fixtures to {FIXTURES}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
