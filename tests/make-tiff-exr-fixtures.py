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

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

WIDTH = 37
HEIGHT = 23


def magick(*args: str) -> None:
    subprocess.run(["magick", *args], check=True)


# The sample type and compression every fixture here uses: float samples and no
# compression, so a chunk is one scan line of raw floats.
FLOAT, UNCOMPRESSED = 2, 0

# The size of every hand written picture here, which is small on purpose: the
# rows are stated by hand and the point is which channels the file holds.
EXR_WIDTH, EXR_HEIGHT = 2, 2


def write_exr(
    path: str,
    parts: list[tuple[str, list[tuple[str, int]], dict[str, list[list[float]]]]],
) -> None:
    """Writes an openexr whose parts are the given channels and their rows.

    Hand written because ImageMagick writes one part of three or four channels and
    nothing else: it writes `B`, `G` and `R` even when asked for a grayscale
    picture, so a single `Y` channel -- the shape the flat gray fixtures need --
    cannot come from it. The bytes are the format's own: a magic, a version whose
    multi part bit is set when there is more than one part, one header a part, a
    zero byte that ends the headers, then an offset table a part and the scan line
    chunks the tables point at. Every part is uncompressed, so a chunk is one scan
    line of raw floats, and a single part chunk states no part number where a
    multi part one does.
    """
    import struct

    width, height = EXR_WIDTH, EXR_HEIGHT
    multipart = len(parts) > 1

    def attribute(name: str, kind: str, payload: bytes) -> bytes:
        return name.encode() + b"\x00" + kind.encode() + b"\x00" + struct.pack("<i", len(payload)) + payload

    def part_header(name: str, channels: list) -> bytes:
        # A channel list entry is a name, a sample type, a linearity flag and a
        # sampling rate, and the list ends with a single zero byte.
        listing = bytearray()
        # The list is sorted by name, which the format requires and the crate
        # checks.
        for channel, sample_type in sorted(channels):
            listing += channel.encode() + b"\x00"
            listing += struct.pack("<i", sample_type) + b"\x00" + b"\x00\x00\x00"
            listing += struct.pack("<ii", 1, 1)
        listing += b"\x00"
        window = struct.pack("<iiii", 0, 0, width - 1, height - 1)
        attributes = [
            ("channels", "chlist", bytes(listing)),
            ("compression", "compression", struct.pack("<B", UNCOMPRESSED)),
            ("dataWindow", "box2i", window),
            ("displayWindow", "box2i", window),
            ("lineOrder", "lineOrder", struct.pack("<B", 0)),
            # A text attribute is the bytes of the string and nothing else: the
            # crate reads exactly the size it was given and compares it to the
            # literal, so a null terminator here is a different value.
            ("name", "string", name.encode()),
            ("pixelAspectRatio", "float", struct.pack("<f", 1.0)),
            ("screenWindowCenter", "v2f", struct.pack("<ff", 0.0, 0.0)),
            ("screenWindowWidth", "float", struct.pack("<f", 1.0)),
            ("type", "string", b"scanlineimage"),
        ]
        if multipart:
            # A multi part file has to state its chunk count; see the note on
            # `OffsetTable` in the crate.
            attributes.append(("chunkCount", "int", struct.pack("<i", height)))
        attributes.sort(key=lambda entry: entry[0])
        out = bytearray()
        for attribute_name, kind, payload in attributes:
            out += attribute(attribute_name, kind, payload)
        return bytes(out) + b"\x00"

    # Version 2 with bit 12 set, which is what makes a file multi part.
    out = bytearray(b"\x76\x2f\x31\x01" + struct.pack("<I", 2 | (0x1000 if multipart else 0)))
    for name, channels, _ in parts:
        out += part_header(name, channels)
    if multipart:
        # The zero byte that ends the header list is the multi part one: a single
        # part file is a 1.x file, where the byte that ends the attributes is also
        # the one that ends the header.
        out += b"\x00"

    # The offset tables follow the headers and the chunks follow the tables, so
    # an offset is only known once both are laid out.
    chunks_at = len(out) + len(parts) * height * 8
    offsets: list[list[int]] = []
    chunks = bytearray()
    for number, (_name, channels, rows) in enumerate(parts):
        part_offsets = []
        for y in range(height):
            payload = bytearray()
            # The chunk states its channels in the order the header lists them,
            # which is the sorted one.
            for channel, _sample_type in sorted(channels):
                payload += struct.pack(f"<{width}f", *rows[channel][y])
            part_offsets.append(chunks_at + len(chunks))
            if multipart:
                chunks += struct.pack("<iii", number, y, len(payload))
            else:
                chunks += struct.pack("<ii", y, len(payload))
            chunks += bytes(payload)
        offsets.append(part_offsets)

    for part_offsets in offsets:
        for offset in part_offsets:
            out += struct.pack("<Q", offset)
    out += chunks

    with open(path, "wb") as handle:
        handle.write(out)
    held = ", ".join(channel for _, channels, _ in parts for channel, _ in channels)
    print(f"  + {os.path.basename(path):28} EXR {width}x{height} {len(parts)} part(s) ({held})")


def write_multipart_exr(path: str) -> None:
    """Writes a two part openexr whose **first** part holds no colour channel.

    The shape is the one the routing plan names -- part 0 is a depth pass holding
    only ``Z``, part 1 is the colour picture -- because that is the file where a
    probe and a decode can settle on different parts.
    """
    write_exr(
        path,
        [
            ("depth", [("Z", FLOAT)], {"Z": [[100.0, 200.0], [300.0, 400.0]]}),
            (
                "colour",
                [("B", FLOAT), ("G", FLOAT), ("R", FLOAT)],
                {
                    "B": [[0.25, 0.5], [1.75, 2.0]],
                    "G": [[0.75, 1.0], [2.25, 2.5]],
                    "R": [[1.25, 1.5], [2.75, 3.0]],
                },
            ),
        ],
    )

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
    # The pixels come from the same png the other colour fixtures were encoded
    # from, so this file holds **that** picture rather than one re-derived from a
    # formula that would only be nearly the same.
    from PIL import Image

    picture = Image.open(colour).convert("RGB")

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

    # ---- Two parts, the first of which holds no colour channel at all. A probe
    # that reads the header and a decode that asks the crate for a layer can
    # settle on different parts, and this is the file where that shows.
    path = os.path.join(FIXTURES, "exr-multipart-z-rgb.exr")
    write_multipart_exr(path)
    report("exr-multipart-z-rgb.exr", path)

    # ---- Flat grayscale: a part holding only `Y`, and one holding `Y` and `A`.
    # ImageMagick writes three or four channels even when asked for a grayscale
    # picture, so these are hand written too.
    gray = {"Y": [[0.25, 0.5], [1.75, 2.0]]}
    gray_alpha = {
        "Y": [[0.25, 0.5], [1.75, 2.0]],
        "A": [[0.125, 0.25], [0.875, 1.0]],
    }
    path = os.path.join(FIXTURES, "exr-gray.exr")
    write_exr(path, [("gray", [("Y", FLOAT)], gray)])
    report("exr-gray.exr", path)
    path = os.path.join(FIXTURES, "exr-gray-alpha.exr")
    write_exr(path, [("gray", [("Y", FLOAT), ("A", FLOAT)], gray_alpha)])
    report("exr-gray-alpha.exr", path)

    for leftover in ["_src-gray.png", "_src-gray16.png", "_src-rgb.png", "_src-rgb16.png", "_src-rgba.png"]:
        target = os.path.join(FIXTURES, leftover)
        if os.path.exists(target):
            os.remove(target)

    print(f"wrote the TIFF and EXR fixtures to {FIXTURES}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
