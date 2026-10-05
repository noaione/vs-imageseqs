r"""Creates the BMP and ICO fixtures used by ``tests/readalpha.vpy``.

Run from the repository root with any Python 3:

    python tests/make-bmp-ico-fixtures.py

BMP and ICO are the two formats whose decoders this tree ports rather than
takes from a crate, so the fixtures are the specification of what the port has
to keep. Each file exists for one row of
``docs/improvements/27-direct-still-decoders.md``'s parity rules, and the
comment beside it says which.

**The BMP files are written here, not by ImageMagick.** That is not a
preference, it is the only way to get them: `magick`'s BMP encoder ignores
``-depth`` and ``-compress`` for the format, so its recipes all produced the
same file. ``-depth 1``, ``-depth 4`` and ``-depth 8`` each wrote an eight bit
``BI_RLE8`` bitmap, ``-depth 16`` wrote a thirty-two bit one, and a flip turned
the ``-depth 24`` recipe into a bitfields file as well. A fixture that is not
the subtype it claims is worse than no fixture, so every header below is
written out field by field and checked by ``tests/readalpha.vpy``.

The ICO directory is a different matter: `magick`'s icon writer does honour its
options, and the two files it writes here match the research corpus byte for
byte in size and entry layout. Only the PNG-payload icon is written here,
because `magick` cannot write one -- and the research corpus turned out not to
have one either. Its ``png-payload.ico`` holds a bare DIB: the directory entry
points at DIB bytes, not at a PNG signature. So the PNG arm of the payload
sniffer was never exercised by the research, and ``ico-png.ico`` is what
exercises it.

The source is a PNG this script also writes, so the fixtures depend on no other
script having run. It is removed afterwards.

A bare DIB is written here too, which is the third thing this script makes: the
same picture with no ``BM`` file header at all, beside the bitmap that holds it.
``dib-depth24.dib`` and ``dib-depth4.dib`` are the pair the validator compares
against ``bmp-depth24.bmp`` and ``bmp-depth4.bmp``. ``dib-not-a-dib.dib`` is the
text file the same name has to refuse.
"""

from __future__ import annotations

import os
import struct
import subprocess
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

WIDTH = 37
HEIGHT = 23

# BMP compression methods, by the number the header stores.
BI_RGB = 0
BI_RLE8 = 1
BI_RLE4 = 2
BI_BITFIELDS = 3

# The five-five-five layout a bitfields file uses by default, and the
# eight-eight-eight-eight one with an alpha channel that is the whole point of
# the 32-bit case.
MASK_555 = (0x7C00, 0x03E0, 0x001F, 0)
MASK_8888 = (0x00FF0000, 0x0000FF00, 0x000000FF, 0xFF000000)


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


def write_png(path: str, pixels: list[tuple[int, int, int, int]]) -> None:
    """Writes an 8-bit RGBA PNG of `pixels`."""

    def chunk(kind: bytes, payload: bytes) -> bytes:
        crc = zlib.crc32(kind + payload) & 0xFFFFFFFF
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", crc)

    header = struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 6, 0, 0, 0)
    raw = b"".join(
        b"\x00" + b"".join(bytes(px) for px in pixels[y * WIDTH : (y + 1) * WIDTH])
        for y in range(HEIGHT)
    )
    document = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    with open(path, "wb") as handle:
        handle.write(document)


def quantise(pixels: list[tuple[int, int, int, int]], bits: int) -> list[int]:
    """Maps `pixels` onto a palette of at most ``2**bits`` colours.

    Returns one palette index per pixel. `bits` is the channel bits to keep, so
    a four bit image keeps one bit a channel and an eight bit one keeps two,
    which is what keeps the palette inside the depth the header states.
    """
    levels = 1 << bits
    step = 256 // levels
    palette: dict[tuple[int, int, int], int] = {}
    order: list[tuple[int, int, int]] = []
    for red, green, blue, _ in pixels:
        key = (red // step, green // step, blue // step)
        if key not in palette:
            palette[key] = len(order)
            order.append(key)
    return [palette[(r // step, g // step, b // step)] for r, g, b, _ in pixels]


def palette_bytes(order: list[tuple[int, int, int]], step: int, entries: int) -> bytes:
    """A palette of `entries` blue, green, red, zero entries."""
    out = bytearray()
    for index in range(entries):
        if index < len(order):
            red, green, blue = order[index]
        else:
            red = green = blue = 0
        out += bytes([blue * step, green * step, red * step, 0])
    return bytes(out)


def pad(row: bytes) -> bytes:
    """A BMP row is padded to a multiple of four bytes."""
    return row + bytes((-len(row)) % 4)


def rle8(indices: list[int], palette: dict[int, int]) -> bytes:
    """Encodes `indices` as BI_RLE8: encoded runs, then end of bitmap."""
    out = bytearray()
    for y in range(HEIGHT):
        row = indices[y * WIDTH : (y + 1) * WIDTH]
        x = 0
        while x < len(row):
            value = row[x]
            run = 1
            while x + run < len(row) and row[x + run] == value and run < 255:
                run += 1
            if run > 2:
                out += bytes([run, value])
            else:
                # Absolute mode: three to 255 literal indices, padded to an
                # even count, introduced by a zero.
                literal = [value]
                x += 1
                while len(literal) < 255 and x < len(row):
                    literal.append(row[x])
                    x += 1
                x -= 1
                out += bytes([0, len(literal)]) + bytes(literal)
                if len(literal) % 2:
                    out += b"\x00"
            x += run
        out += bytes([0, 0])  # end of this line
    out += bytes([0, 1])  # end of bitmap
    return bytes(out)


def rle4(indices: list[int]) -> bytes:
    """Encodes `indices` as BI_RLE4: two indices a byte, then end of bitmap."""
    out = bytearray()
    for y in range(HEIGHT):
        row = indices[y * WIDTH : (y + 1) * WIDTH]
        x = 0
        while x < len(row):
            value = row[x]
            run = 1
            while x + run < len(row) and row[x + run] == value and run < 255:
                run += 1
            if run > 2:
                out += bytes([run, (value << 4) | value])
                x += run
            else:
                literal = [value]
                x += 1
                while len(literal) < 255 and x < len(row):
                    literal.append(row[x])
                    x += 1
                x -= 1
                packed = bytearray()
                for index in range(0, len(literal), 2):
                    high = literal[index]
                    low = literal[index + 1] if index + 1 < len(literal) else 0
                    packed.append((high << 4) | low)
                out += bytes([0, len(literal)]) + bytes(packed)
                if len(packed) % 2:
                    out += b"\x00"
                x += len(literal) - 1
            x += 1
        out += bytes([0, 0])
    out += bytes([0, 1])
    return bytes(out)


def dib_header(
    bpp: int,
    compression: int,
    image_bytes: int,
    height: int,
    dib: int,
    alpha_mask: int,
) -> bytes:
    """A BITMAPINFOHEADER, or the V4/V5 header that continues it.

    `height` is negative for a top-down bitmap. The total length is the `dib`
    the first field states: 40, 108 or 124 bytes.
    """
    header = struct.pack(
        "<IiiHHIIiiII",
        dib,
        WIDTH,
        height,
        1,
        bpp,
        compression,
        image_bytes,
        2835,
        2835,
        0,
        0,
    )
    red, green, blue, _ = MASK_8888 if bpp == 32 else MASK_555
    if dib == 40:
        # A V3 header has no room for the masks, so a bitfields bitmap stores
        # them as three or four words immediately after the header.
        if compression == BI_BITFIELDS:
            masks = struct.pack("<III", red, green, blue)
            if bpp == 32:
                masks += struct.pack("<I", alpha_mask)
            return header + masks
        return header
    # A V4 header carries the masks itself, the colour space, its endpoints and
    # the gamma; a V5 one adds the intent and the profile fields. Each pads out
    # to the size its first field states, so the offsets that follow are right.
    extra = struct.pack("<IIII", red, green, blue, alpha_mask)
    extra += b"BGRs" + b"\x00" * 36 + struct.pack("<III", 0, 0, 0)
    if dib == 124:
        extra += struct.pack("<IIII", 0, 0, 0, 0)
    return header + extra


def bmp_parts(
    pixels: list[tuple[int, int, int, int]],
    *,
    bpp: int,
    compression: int = BI_RGB,
    dib: int = 40,
    top_down: bool = False,
    alpha_mask: int = 0,
) -> tuple[bytes, bytes, bytes]:
    """The header, the palette and the pixel data of one bitmap.

    They are returned apart because a `.bmp` and a bare DIB hold exactly the
    same three: the only difference between the two files is the fourteen byte
    file header the bare one does not have.
    """
    palette = b""
    body = bytearray()

    if bpp <= 8:
        channel_bits = {1: 1, 4: 1, 8: 2}[bpp]
        levels = 1 << channel_bits
        step = 256 // levels
        indices = quantise(pixels, channel_bits)
        # Rebuild the palette in the same order quantise assigned.
        order: list[tuple[int, int, int]] = []
        seen: dict[tuple[int, int, int], int] = {}
        for red, green, blue, _ in pixels:
            key = (red // step, green // step, blue // step)
            if key not in seen:
                seen[key] = len(order)
                order.append(key)
        entries = 1 << bpp
        if bpp == 1:
            # One bit means two entries, so threshold on luminance rather than
            # keeping a channel bit.
            order = [(0, 0, 0), (1, 1, 1)]
            step = 255
            indices = [
                1 if (r + g + b) // 3 >= 128 else 0 for r, g, b, _ in pixels
            ]
        palette = palette_bytes(order, step, entries)
        rows = []
        for y in range(HEIGHT):
            row = indices[y * WIDTH : (y + 1) * WIDTH]
            if bpp == 8:
                rows.append(pad(bytes(row)))
            elif bpp == 4:
                packed = bytearray()
                for index in range(0, len(row), 2):
                    high = row[index]
                    low = row[index + 1] if index + 1 < len(row) else 0
                    packed.append((high << 4) | low)
                rows.append(pad(bytes(packed)))
            else:
                packed = bytearray()
                for index in range(0, len(row), 8):
                    byte = 0
                    for bit in range(8):
                        if index + bit < len(row) and row[index + bit]:
                            byte |= 0x80 >> bit
                    packed.append(byte)
                rows.append(pad(bytes(packed)))
        if compression == BI_RLE8:
            body = bytearray(rle8(indices, {}))
        elif compression == BI_RLE4:
            body = bytearray(rle4(indices))
        else:
            body = bytearray(b"".join(reversed(rows) if not top_down else rows))
    else:
        rows = []
        for y in range(HEIGHT):
            row = bytearray()
            for x in range(WIDTH):
                red, green, blue, alpha = pixels[y * WIDTH + x]
                if bpp == 24:
                    row += bytes([blue, green, red])
                elif bpp == 32:
                    row += bytes([blue, green, red, alpha])
                else:
                    row += struct.pack("<H", ((red >> 3) << 10) | ((green >> 3) << 5) | (blue >> 3))
            rows.append(pad(bytes(row)))
        body = bytearray(b"".join(reversed(rows) if not top_down else rows))

    height = -HEIGHT if top_down else HEIGHT
    header = dib_header(bpp, compression, len(body), height, dib, alpha_mask)
    return header, palette, bytes(body)


def write_bmp(path: str, pixels: list[tuple[int, int, int, int]], **options: object) -> None:
    """Writes one BMP, bottom-up unless `top_down`."""
    header, palette, body = bmp_parts(pixels, **options)  # pyright: ignore[reportArgumentType]
    offset = 14 + len(header) + len(palette)
    file_header = struct.pack("<2sIHHI", b"BM", offset + len(body), 0, 0, offset)
    with open(path, "wb") as handle:
        handle.write(file_header + header + palette + body)


def write_dib(path: str, pixels: list[tuple[int, int, int, int]], **options: object) -> None:
    """Writes the DIB alone, with no `BM` file header at all.

    That is what a `.dib` file holds and what an icon's directory entry points
    at: the file starts at the DIB header, which is the same header walk with
    `file_header` false.
    """
    header, palette, body = bmp_parts(pixels, **options)  # pyright: ignore[reportArgumentType]
    with open(path, "wb") as handle:
        handle.write(header + palette + body)


def png_payload_ico(path: str, payload: str) -> None:
    """Writes a one-entry icon whose payload is the PNG at `payload`."""
    with open(payload, "rb") as handle:
        blob = handle.read()
    directory = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack("<BBBBHHII", 32, 32, 0, 0, 1, 32, len(blob), 6 + 16)
    with open(path, "wb") as handle:
        handle.write(directory + entry + blob)


def magick(*args: str) -> bool:
    """Runs `magick`, and reports whether it produced the file."""
    out = subprocess.run(["magick", *args], capture_output=True, text=True, check=False)
    target = args[-1]
    if out.returncode != 0 or not os.path.exists(target):
        print(f"  ! magick {' '.join(args)} -> {out.returncode} {out.stderr.strip()[:160]}")
        return False
    print(f"  + {os.path.basename(target)} ({os.path.getsize(target)} bytes)")
    return True


def main() -> int:
    os.makedirs(FIXTURES, exist_ok=True)
    pixels = samples()
    source = os.path.join(HERE, "bmp-ico-source.png")
    write_png(source, pixels)

    def fixture(name: str) -> str:
        return os.path.join(FIXTURES, name)

    def emit(name: str, **options: object) -> None:
        target = fixture(name)
        write_bmp(target, pixels, **options)  # pyright: ignore[reportArgumentType]
        print(f"  + {name} ({os.path.getsize(target)} bytes)")

    # ---- One file per palette depth. These exercise the palette expansion and
    # the row padding, and 1 and 4 bits pack several pixels into a byte.
    emit("bmp-depth1.bmp", bpp=1)
    emit("bmp-depth4.bmp", bpp=4)
    emit("bmp-depth8.bmp", bpp=8)
    # The two run-length forms, which are what make the delta and absolute
    # modes reachable.
    emit("bmp-rle4.bmp", bpp=4, compression=BI_RLE4)
    emit("bmp-rle8.bmp", bpp=8, compression=BI_RLE8)
    # Plain truecolour, three bytes to a pixel.
    emit("bmp-depth24.bmp", bpp=24)
    # A top-down bitmap states a negative height, so its rows are not reversed.
    emit("bmp-topdown24.bmp", bpp=24, top_down=True)
    # A 32-bit BI_RGB bitmap **drops its fourth byte**, so no alpha is handed
    # out however the file's fourth byte is set. This is the case the plan's
    # first parity rule names, and it holds for a V4 or V5 header too.
    emit("bmp-rgb32.bmp", bpp=32)
    emit("bmp-rgb32-v5.bmp", bpp=32, dib=124)
    # A bitfields bitmap states its own masks, and alpha is handed out only
    # when the alpha mask is non-zero.
    emit("bmp-bitfields32.bmp", bpp=32, compression=BI_BITFIELDS, dib=108,
         alpha_mask=MASK_8888[3])
    emit("bmp-bitfields32-noalpha.bmp", bpp=32, compression=BI_BITFIELDS, dib=108,
         alpha_mask=0)
    # The same masks under a V5 header, where they live inside the header
    # rather than after it.
    emit("bmp-v5-bitfields32.bmp", bpp=32, compression=BI_BITFIELDS, dib=124,
         alpha_mask=MASK_8888[3])
    emit("bmp-bitfields16.bmp", bpp=16, compression=BI_BITFIELDS, dib=40)

    def emit_dib(name: str, **options: object) -> None:
        target = fixture(name)
        write_dib(target, pixels, **options)  # pyright: ignore[reportArgumentType]
        print(f"  + {name} ({os.path.getsize(target)} bytes)")

    # ---- A bare DIB: the same picture as the bitmaps above with no `BM` file
    # header at all, so the file starts at the DIB header. The palette one is
    # the case that catches an offset still counting the fourteen bytes that
    # are not there, and both are the shape a `.dib` file has.
    emit_dib("dib-depth24.dib", bpp=24)
    emit_dib("dib-depth4.dib", bpp=4)
    # The negative case: the name alone is not a claim, so a `.dib` that holds no
    # DIB header is refused rather than read as a bitmap.
    with open(fixture("dib-not-a-dib.dib"), "wb") as handle:
        handle.write(b"this is not a device-independent bitmap at all\n")
    print(f"  + dib-not-a-dib.dib ({os.path.getsize(fixture('dib-not-a-dib.dib'))} bytes)")

    # ---- ICO. The directory is what decides which entry is read, and the
    # payload is sniffed as PNG or as a DIB.
    small = os.path.join(FIXTURES, "bmp-ico-16.png")
    medium = os.path.join(FIXTURES, "bmp-ico-32.png")
    large = os.path.join(FIXTURES, "bmp-ico-48.png")
    # Forced square: an icon directory states its own width and height, and a
    # reader checks that the payload agrees with them. A plain `-resize` keeps
    # the source's aspect ratio, which made a 32 by 20 payload under a 32 by 32
    # entry and was refused.
    magick(source, "-resize", "16x16!", small)
    magick(source, "-resize", "32x32!", medium)
    magick(source, "-resize", "48x48!", large)
    # Entries are scored by (bits per pixel, area), so a multi-entry icon is
    # what exercises the tie-break and the fact that depth dominates area.
    magick(small, medium, large, fixture("ico-multi.ico"))
    # A single DIB entry. The payload is a bare DIB and the directory's height
    # is twice the image's.
    magick(medium, "-define", "icon:auto-resize=32", fixture("ico-dib32.ico"))
    # A PNG payload, which magick cannot write and the research corpus does not
    # have.
    png_payload_ico(fixture("ico-png.ico"), medium)
    print(f"  + ico-png.ico ({os.path.getsize(fixture('ico-png.ico'))} bytes)")

    for scratch in (source, small, medium, large):
        if os.path.exists(scratch):
            os.remove(scratch)

    print(f"wrote the BMP and ICO fixtures to {FIXTURES}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
