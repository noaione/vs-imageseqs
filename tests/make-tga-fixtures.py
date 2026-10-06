r"""Creates the TGA fixtures used by ``tests/readalpha.vpy``.

Run from the repository root with any Python 3:

    python tests/make-tga-fixtures.py

TGA is ported rather than taken from a crate, so these files are the
specification of what the port has to keep. Each one exists for a row of
``docs/improvements/27-direct-still-decoders.md``'s parity rules or for one of
the eleven image types the format defines, and the comment beside it says which.

**They are written here rather than by ImageMagick**, for the reason the BMP
fixtures are: `magick`'s TGA encoder does not honour the options that would
select these subtypes, so a recipe that asks for a sixteen bit file can quietly
write a thirty-two bit one. A fixture that is not the subtype it claims is worse
than no fixture, so every field of the eighteen byte header is written out here
and checked back by ``tests/readalpha.vpy``.

The one case the plan calls out is ``tga-rgb32-attr0.tga``: a thirty-two bit
image whose descriptor states **zero attribute bits** is still handed out as
``Rgba8``, keeping its fourth byte as alpha. That is the opposite of the rule for
a BMP, where the same fourth byte is dropped, and the two files exist to hold
that difference still.

The second case is the two byte colour map entry. ``tga-mapped8-555-widened.tga``
holds the same indexed picture as ``tga-mapped8-555-opaque.tga``, whose map
entries are fifteen bits, and as ``tga-mapped8-555.tga``, whose entries are
sixteen bits and state an attribute bit at bit fifteen. The eight bit map holds
the values the five bit channels widen to, so the three have to decode alike.
"""

from __future__ import annotations

import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

WIDTH = 37
HEIGHT = 23

# TGA image types, by the number the header stores. One to three are raw, nine
# to eleven are the same three run-length encoded.
TYPE_COLOR_MAPPED = 1
TYPE_TRUECOLOR = 2
TYPE_GRAYSCALE = 3
TYPE_COLOR_MAPPED_RLE = 9
TYPE_TRUECOLOR_RLE = 10
TYPE_GRAYSCALE_RLE = 11

# The descriptor's bits: the low nibble counts attribute bits, bit four runs the
# columns right to left, and bit five runs the rows top to bottom.
RIGHT_TO_LEFT = 1 << 4
TOP_TO_BOTTOM = 1 << 5


def samples() -> list[tuple[int, int, int, int]]:
    """The picture every fixture holds: a ramp with a diagonal and an alpha step."""
    pixels: list[tuple[int, int, int, int]] = []
    for y in range(HEIGHT):
        for x in range(WIDTH):
            red = (x * 255) // (WIDTH - 1)
            green = (y * 255) // (HEIGHT - 1)
            blue = 255 if (x + y) % 8 < 4 else 0
            alpha = 255 if x >= WIDTH // 2 else 64
            pixels.append((red, green, blue, alpha))
    return pixels


def palette() -> list[tuple[int, int, int]]:
    """A 256 entry colour map, so an eight bit index reaches a colour."""
    return [((i * 7) % 256, (i * 13) % 256, (i * 29) % 256) for i in range(256)]


def widen(value: int, bits: int = 5) -> int:
    """The round-to-nearest widening the reader shares with the bitmap reader."""
    top = (1 << bits) - 1
    return (value * 255 + top // 2) // top


def five_bit_palette() -> list[tuple[int, int, int]]:
    """A 256 entry map in five bit channels, which a two byte entry holds."""
    return [((i * 7) % 32, (i * 13) % 32, (i * 29) % 32) for i in range(256)]


def packed_entry(red: int, green: int, blue: int, attribute: int = 0) -> bytes:
    """One two byte entry: five bits a channel and the attribute bit on top."""
    value = (attribute << 15) | (red << 10) | (green << 5) | blue
    return value.to_bytes(2, "little")


def header(
    image_type: int,
    *,
    width: int = WIDTH,
    height: int = HEIGHT,
    pixel_depth: int,
    descriptor: int,
    map_type: int = 0,
    map_first: int = 0,
    map_length: int = 0,
    map_entry_size: int = 0,
) -> bytes:
    """The eighteen byte header, with every field stated rather than implied.

    The colour map entry size is a *single* byte at offset seven, and the two
    origin fields follow it. Writing it as a word shifts every later field by
    one and the file is refused by every reader, which is what happened the
    first time this was written.
    """
    return (
        bytes([0, map_type, image_type])
        + map_first.to_bytes(2, "little")
        + map_length.to_bytes(2, "little")
        + bytes([map_entry_size])
        + (0).to_bytes(2, "little")  # x origin
        + (0).to_bytes(2, "little")  # y origin
        + width.to_bytes(2, "little")
        + height.to_bytes(2, "little")
        + bytes([pixel_depth, descriptor])
    )


def truecolour_bytes(
    pixels: list[tuple[int, int, int, int]], depth: int
) -> bytes:
    """The pixel data of a truecolour image, in the order the format stores it.

    The format is blue first, and rows run bottom to top unless the descriptor
    says otherwise. This writer always stores bottom-up and left-to-right; the
    orientation fixtures set the descriptor instead and reverse the data here,
    which is what a writer does.
    """
    out = bytearray()
    for red, green, blue, alpha in pixels:
        if depth == 16:
            # Five bits a channel, one unused. The low bit of the blue field is
            # dropped rather than rounded.
            value = ((red >> 3) << 10) | ((green >> 3) << 5) | (blue >> 3)
            out += value.to_bytes(2, "little")
        elif depth == 24:
            out += bytes([blue, green, red])
        else:
            out += bytes([blue, green, red, alpha])
    return bytes(out)


def rle(packets: bytes, bytes_per_pixel: int) -> bytes:
    """Encodes raw pixels as run-length packets.

    A packet's high bit says whether it is a run; the low seven bits count one
    less than the number of pixels it covers. This writer emits runs for
    repeated pixels and raw packets otherwise, and never a packet shorter than
    two pixels because a one pixel run costs more than it saves.
    """
    pixels = [
        packets[at : at + bytes_per_pixel]
        for at in range(0, len(packets), bytes_per_pixel)
    ]
    out = bytearray()
    index = 0
    while index < len(pixels):
        run = 1
        while (
            index + run < len(pixels)
            and pixels[index + run] == pixels[index]
            and run < 128
        ):
            run += 1
        if run > 1:
            out.append(0x80 | (run - 1))
            out += pixels[index]
            index += run
            continue
        # A raw packet: gather until a pixel repeats, up to 128 pixels.
        start = index
        while index < len(pixels) and index - start < 128:
            if index + 1 < len(pixels) and pixels[index + 1] == pixels[index]:
                break
            index += 1
        count = index - start
        out.append(count - 1)
        for pixel in pixels[start:index]:
            out += pixel
    return bytes(out)


def write(
    name: str,
    image_type: int,
    body: bytes,
    *,
    pixel_depth: int,
    descriptor: int,
    extra_header: dict[str, int] | None = None,
    color_map: bytes = b"",
) -> None:
    """Writes one fixture: header, colour map, then the pixel data."""
    options = {"pixel_depth": pixel_depth, "descriptor": descriptor}
    options.update(extra_header or {})
    document = header(image_type, **options) + color_map + body
    target = os.path.join(FIXTURES, name)
    with open(target, "wb") as handle:
        handle.write(document)
    print(f"  + {name} ({len(document)} bytes)")


def main() -> int:
    os.makedirs(FIXTURES, exist_ok=True)
    pixels = samples()
    table = palette()

    # ---- The three raw image types, at every depth that changes the outcome.
    write("tga-rgb24.tga", TYPE_TRUECOLOR, truecolour_bytes(pixels, 24),
          pixel_depth=24, descriptor=0)
    # A thirty-two bit image stating NO attribute bits is still r,g,b,a: the
    # fourth byte is alpha, which is the opposite of the rule for a BMP.
    write("tga-rgb32-attr0.tga", TYPE_TRUECOLOR, truecolour_bytes(pixels, 32),
          pixel_depth=32, descriptor=0)
    # The same file stating eight attribute bits, which reaches the same layout
    # through the other arm of the match.
    write("tga-rgb32-attr8.tga", TYPE_TRUECOLOR, truecolour_bytes(pixels, 32),
          pixel_depth=32, descriptor=8)
    # Sixteen bits is five a channel, widened by the round-to-nearest table.
    write("tga-rgb16.tga", TYPE_TRUECOLOR, truecolour_bytes(pixels, 16),
          pixel_depth=16, descriptor=0)
    # Grayscale, with and without an alpha byte.
    write("tga-gray8.tga", TYPE_GRAYSCALE,
          bytes((r + g + b) // 3 for r, g, b, _ in pixels),
          pixel_depth=8, descriptor=0)
    write("tga-gray8-alpha.tga", TYPE_GRAYSCALE,
          b"".join(bytes([(r + g + b) // 3, a]) for r, g, b, a in pixels),
          pixel_depth=16, descriptor=8)
    # A colour-mapped image: eight bit indices into a 24 bit colour map.
    indices = bytes((i * 3) % 256 for i in range(WIDTH * HEIGHT))
    color_map = b"".join(bytes([blue, green, red]) for red, green, blue in table)
    write("tga-mapped8.tga", TYPE_COLOR_MAPPED, indices,
          pixel_depth=8, descriptor=0,
          extra_header={"map_type": 1, "map_length": 256, "map_entry_size": 24},
          color_map=color_map)

    # The same indexed picture with two byte map entries, which are five bits a
    # channel. The eight bit map holds the values those channels widen to, so the
    # pair decodes alike; the sixteen bit spelling states an attribute bit at bit
    # fifteen, which is the entry's alpha, and the fifteen bit one states none
    # and can only be opaque.
    five = five_bit_palette()
    write("tga-mapped8-555-widened.tga", TYPE_COLOR_MAPPED, indices,
          pixel_depth=8, descriptor=0,
          extra_header={"map_type": 1, "map_length": 256, "map_entry_size": 24},
          color_map=b"".join(
              bytes([widen(blue), widen(green), widen(red)])
              for red, green, blue in five
          ))
    write("tga-mapped8-555-opaque.tga", TYPE_COLOR_MAPPED, indices,
          pixel_depth=8, descriptor=0,
          extra_header={"map_type": 1, "map_length": 256, "map_entry_size": 15},
          color_map=b"".join(packed_entry(*entry) for entry in five))
    write("tga-mapped8-555.tga", TYPE_COLOR_MAPPED, indices,
          pixel_depth=8, descriptor=0,
          extra_header={"map_type": 1, "map_length": 256, "map_entry_size": 16},
          color_map=b"".join(
              packed_entry(*entry, attribute=1 if index >= 128 else 0)
              for index, entry in enumerate(five)
          ))

    # ---- The same three again, run-length encoded.
    write("tga-rgb24-rle.tga", TYPE_TRUECOLOR_RLE,
          rle(truecolour_bytes(pixels, 24), 3), pixel_depth=24, descriptor=0)
    write("tga-gray8-rle.tga", TYPE_GRAYSCALE_RLE,
          rle(bytes((r + g + b) // 3 for r, g, b, _ in pixels), 1),
          pixel_depth=8, descriptor=0)
    write("tga-mapped8-rle.tga", TYPE_COLOR_MAPPED_RLE, rle(indices, 1),
          pixel_depth=8, descriptor=0,
          extra_header={"map_type": 1, "map_length": 256, "map_entry_size": 24},
          color_map=color_map)

    # ---- The two descriptor directions, other than the default bottom-left.
    # Stored top-down, which the descriptor says so the reader does not flip it.
    write("tga-rgb24-topdown.tga", TYPE_TRUECOLOR,
          b"".join(
            truecolour_bytes(pixels, 24)[row * WIDTH * 3 : (row + 1) * WIDTH * 3]
            for row in reversed(range(HEIGHT))
          ),
          pixel_depth=24, descriptor=TOP_TO_BOTTOM)
    # Stored right to left, which the descriptor says so the reader does not
    # flop it.
    write("tga-rgb24-rightleft.tga", TYPE_TRUECOLOR,
          b"".join(
            b"".join(
                truecolour_bytes(pixels, 24)[(row * WIDTH + x) * 3 : (row * WIDTH + x + 1) * 3]
                for x in reversed(range(WIDTH))
            )
            for row in range(HEIGHT)
          ),
          pixel_depth=24, descriptor=RIGHT_TO_LEFT)

    print(f"wrote the TGA fixtures to {FIXTURES}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
