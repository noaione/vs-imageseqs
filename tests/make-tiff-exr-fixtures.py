r"""Creates the TIFF and EXR fixtures used by ``tests/readalpha.vpy``.

Run from the repository root with ImageMagick on ``PATH``:

    python tests/make-tiff-exr-fixtures.py

Both formats are large enough that the plan splits them across a crate each
rather than porting a reader, so these files are the specification of what the
two crates have to keep. ``alpha-rgba32f.tiff`` predates this script and is left
alone: it is the four channel float case and the validator already reads it.

The script writes each file and then **dumps what was actually produced**, which
is the lesson every earlier fixture script in this tree taught: ImageMagick
accepts the options that are meant to select a subtype and quietly writes a
different one when the build cannot. A fixture that is not the subtype it claims
is worse than no fixture, so the dump is the point of the run, not a nicety.
"""

from __future__ import annotations

import os
import subprocess
import sys

if sys.version_info < (3, 14):
    raise SystemExit("needs Python 3.14 for the zstd writer below")

from compression import zstd  # noqa: E402 - 3.14, hence the guard above

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

WIDTH = 37
HEIGHT = 23


def magick(*args: str) -> None:
    subprocess.run(["magick", *args], check=True)


def write_zstd_tiff(path: str, rows: list[bytes]) -> None:
    """Writes a little-endian RGB tiff whose single strip is a zstd frame.

    Hand written because **no encoder on this machine writes one**:
    ImageMagick rejects the compression name, and the libtiff Pillow bundles
    answers "ZSTD compression support is not configured". Python 3.14 has
    `compression.zstd`, so the container is written here and the strip is a
    raw zstd frame, which is what libtiff's codec 50000 is.
    """
    import struct

    height = len(rows)
    width = len(rows[0]) // 3
    raw = b"".join(rows)
    strip = zstd.compress(raw)

    # Tag, type, count, value. Sorted by tag, which the format asks for.
    SHORT, LONG = 3, 4
    entries = [
        (256, SHORT, 1, width),           # ImageWidth
        (257, SHORT, 1, height),          # ImageLength
        (258, SHORT, 3, None),            # BitsPerSample, an array
        (259, SHORT, 1, 50000),           # Compression, ZSTD
        (262, SHORT, 1, 2),               # PhotometricInterpretation, RGB
        (273, LONG, 1, None),             # StripOffsets
        (277, SHORT, 1, 3),               # SamplesPerPixel
        (278, SHORT, 1, height),          # RowsPerStrip
        (279, LONG, 1, len(strip)),       # StripByteCounts
        (284, SHORT, 1, 1),               # PlanarConfiguration, chunky
    ]
    ifd_at = 8
    ifd_size = 2 + len(entries) * 12 + 4
    bits_at = ifd_at + ifd_size
    strip_at = bits_at + 6

    out = bytearray(b"II\x2a\x00" + struct.pack("<I", ifd_at))
    out += struct.pack("<H", len(entries))
    for tag, kind, count, value in entries:
        if tag == 258:
            value = bits_at
        if tag == 273:
            value = strip_at
        out += struct.pack("<HHI", tag, kind, count)
        # A count of one fits in the value field; anything else is an offset.
        out += struct.pack("<H", value) + b"\x00\x00" if kind == SHORT and count == 1 else struct.pack("<I", value)
    out += struct.pack("<I", 0)  # no next IFD
    out += struct.pack("<3H", 8, 8, 8)
    out += strip
    with open(path, "wb") as handle:
        handle.write(out)
    print(f"  + {os.path.basename(path):28} TIFF {width}x{height} RGB zstd ({len(strip)} bytes in strip)")


def source(name: str, depth: int = 8) -> str:
    """Writes the picture every fixture holds, as a png the encoders read."""
    path = os.path.join(FIXTURES, name)
    magick(
        "-size",
        f"{WIDTH}x{HEIGHT}",
        "gradient:black-white",
        "-depth",
        str(depth),
        path,
    )
    return path


def report(label: str, path: str) -> None:
    """Prints the subtype ImageMagick actually wrote, which is the check."""
    result = subprocess.run(
        ["magick", "identify", "-format", "%m %[bit-depth] %[channels] %[compression]", path],
        capture_output=True,
        text=True,
        check=False,
    )
    print(f"  {os.path.basename(path):28} {result.stdout.strip()}")


def main() -> int:
    os.makedirs(FIXTURES, exist_ok=True)
    grey = source("_src-gray.png", 8)
    grey16 = source("_src-gray16.png", 16)
    colour = os.path.join(FIXTURES, "_src-rgb.png")
    magick(
        "-size", f"{WIDTH}x{HEIGHT}", "gradient:red-blue",
        "-depth", "8", colour,
    )
    colour16 = os.path.join(FIXTURES, "_src-rgb16.png")
    magick("-size", f"{WIDTH}x{HEIGHT}", "gradient:red-blue", "-depth", "16", colour16)
    alpha = os.path.join(FIXTURES, "_src-rgba.png")
    magick(colour, "-alpha", "set", "-channel", "A", "-evaluate", "set", "60%", "-type", "TrueColorAlpha", alpha)

    print("TIFF:")
    # ---- Depth and channel count.
    # The type is stated rather than left to the writer: ImageMagick picks a
    # palette for a picture with few enough colours, and every file then says
    # `Photometric interpretation RGBPalette` in its header however it was
    # asked for. It also needs telling apart the grey cases, which `TrueColor`
    # would otherwise turn into three channel ones.
    for name, src, kind, extra in [
        ("tiff-gray8.tiff", grey, "Grayscale", []),
        ("tiff-gray16.tiff", grey16, "Grayscale", []),
        ("tiff-rgb8.tiff", colour, "TrueColor", []),
        ("tiff-rgb16.tiff", colour16, "TrueColor", ["-depth", "16"]),
        ("tiff-rgba8.tiff", alpha, "TrueColorAlpha", []),
    ]:
        path = os.path.join(FIXTURES, name)
        magick(src, *extra, "-type", kind, path)
        report(name, path)

    # ---- Compression. Each of these is a distinct codec in the header, and a
    # ImageMagick spells the TIFF PackBits codec "RLE", which is what that name
    # means in the header.
    # reader that supports one and not another refuses the file.
    for name, compression in [
        ("tiff-none.tiff", "None"),
        ("tiff-lzw.tiff", "LZW"),
        ("tiff-deflate.tiff", "Zip"),
        ("tiff-packbits.tiff", "RLE"),
    ]:
        path = os.path.join(FIXTURES, name)
        magick(colour, "-type", "TrueColor", "-compress", compression, path)
        report(name, path)

    # ---- Planar, where the samples are stored plane by plane rather than
    # interleaved, and tiled, where they are stored in blocks.
    path = os.path.join(FIXTURES, "tiff-planar.tiff")
    magick(colour, "-type", "TrueColor", "-interlace", "plane", path)
    report("tiff-planar.tiff", path)
    path = os.path.join(FIXTURES, "tiff-tiled.tiff")
    magick(colour, "-type", "TrueColor", "-define", "tiff:tile-geometry=16x16", "-compress", "Zip", path)
    report("tiff-tiled.tiff", path)

    # ---- A palette page, whose indices are expanded through the colour map.
    # ---- A zstd strip, which no encoder here can write; see the writer above.
    # The pixels come from the same png the other colour fixtures were encoded
    # from, so this file holds **that** picture rather than one re-derived from a
    # formula that would only be nearly the same.
    from PIL import Image

    picture = Image.open(colour).convert("RGB")
    zstd_rows = [
        bytes(component for x in range(picture.width) for component in picture.getpixel((x, y)))
        for y in range(picture.height)
    ]
    write_zstd_tiff(os.path.join(FIXTURES, "tiff-zstd.tiff"), zstd_rows)

    path = os.path.join(FIXTURES, "tiff-palette.tiff")
    magick(colour, "-colors", "16", "-type", "Palette", path)
    report("tiff-palette.tiff", path)

    print("EXR:")
    # ---- Half is the format's native sample and what most files hold; float is
    # the other. Both are three or four channels.
    for name, src, extra in [
        ("exr-half-rgb.exr", colour, ["-type", "TrueColor"]),
        ("exr-half-rgba.exr", alpha, []),
        ("exr-float-rgba.exr", alpha, ["-define", "exr:pixel-type=float"]),
    ]:
        path = os.path.join(FIXTURES, name)
        magick(src, *extra, path)
        report(name, path)

    # ---- Every compression the format defines, because a reader that takes one
    # and not the others reads most real files wrongly.
    for name, compression in [
        ("exr-none.exr", "None"),
        ("exr-rle.exr", "RLE"),
        ("exr-zip.exr", "Zip"),
        ("exr-zips.exr", "ZipS"),
        ("exr-piz.exr", "Piz"),
    ]:
        path = os.path.join(FIXTURES, name)
        magick(colour, "-type", "TrueColor", "-compress", compression, path)
        report(name, path)

    for leftover in ["_src-gray.png", "_src-gray16.png", "_src-rgb.png", "_src-rgb16.png", "_src-rgba.png"]:
        target = os.path.join(FIXTURES, leftover)
        if os.path.exists(target):
            os.remove(target)

    print(f"wrote the TIFF and EXR fixtures to {FIXTURES}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
