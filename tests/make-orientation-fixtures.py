r"""Creates the exif orientation fixtures used by ``tests/readalpha.vpy``.

Run from the repository root with any Python 3:

    python tests/make-orientation-fixtures.py

``orientation-1.png`` to ``orientation-8.png`` hold the same picture and differ
only in the orientation the file states, so the validator can assert that the
eight codes are the eight rearrangements of one grid. The grid is four wide and
three tall and every sample is different (``row * 4 + column + 1``), which is
what makes a swapped row, a swapped column and a lost transpose all visible in
the plane the plugin hands out; a square picture would hide the two that swap
the size.

The file is written by hand because the exif tag is the whole point of it: the
``eXIf`` chunk of a png is a bare tiff header (no ``Exif\0\0`` prefix, which is
only the heading of the jpeg APP1 segment) with one IFD entry, the orientation
tag, and nothing else. The ``image`` png decoder hands that chunk to
``Orientation::from_exif_chunk``, which is the reader the plugin's probe uses,
so a fixture written this way exercises the same road as a camera-written file.

A lossy webp is decoded into its own yuv planes instead, which is the other
writer a rotated page goes through, and it needs a fixture of its own:
``orientation-2.webp``, ``orientation-6.webp`` and ``orientation-8.webp`` are
that same test on the planar path. Lossy compression cannot keep twelve
different samples, so this picture is four quadrants of four grey levels, which
survive an encode and still say which way the picture was turned. They are cut
from ``orientation-split.webp``, whose bitstream has to be made once by hand:

    magick orientation-split.png -quality 90 -define webp:method=4 orientation-split.webp

The script copies that bitstream and writes the exif chunk beside it, which is
where a webp keeps its exif, together with the extended header that says the
file has one.

``orientation-6.jxl`` is the same test again on the third format that states an
orientation, and it is not written by this script because a jpeg xl cannot be
assembled by hand the way the other two can: the code lives in the codestream
header, and only an encoder will produce one. It is encoded once from the png
above and committed, and has to be re-encoded by hand if the picture changes:

    cjxl -d 0 tests/fixtures/orientation-6.png tests/fixtures/orientation-6.jxl

It is tiny because ``-d 0`` is lossless and the picture is three by four
samples. The alpha fixture's own jxl copy, ``alpha-rgba8.jxl``, is encoded the
same way and ``tests/make-alpha-fixtures.py`` records it.
"""

from __future__ import annotations

import os
import struct
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

WIDTH = 4
HEIGHT = 3

# The picture the yuv fixtures are encoded from: four quadrants of four grey
# levels, dark to bright, which a lossy encode keeps in order.
SPLIT_WIDTH = 16
SPLIT_HEIGHT = 12
SPLIT_LEVELS = ((16, 90), (170, 235))

# The codes the webp fixtures state, and the source their bitstream is cut from.
WEBP_ORIENTATIONS = (2, 6, 8)
WEBP_SOURCE = "orientation-split.webp"

# PNG color type of the fixtures: a gray image, so the validator states one
# plane per frame.
GRAY = 0

# Exif tag and format of the one IFD entry the fixtures carry.
ORIENTATION_TAG = 0x0112
SHORT = 3

# The webp chunks the script reads and writes: the bitstream of a lossy file,
# the exif chunk, and the extended header that has to state that there is one.
VP8_CHUNK = b"VP8 "
VP8X_CHUNK = b"VP8X"
EXIF_CHUNK = b"EXIF"
VP8X_EXIF_FLAG = 0x08
VP8_START_CODE = b"\x9d\x01\x2a"


def exif(orientation: int) -> bytes:
    """The tiff header a png ``eXIf`` chunk holds, with one orientation tag."""
    return (
        b"II\x2a\x00"  # little endian, and the magic of a tiff file
        + struct.pack("<I", 8)  # offset of the IFD, directly after this header
        + struct.pack("<H", 1)  # one IFD entry
        + struct.pack("<HHIHH", ORIENTATION_TAG, SHORT, 1, orientation, 0)
        + struct.pack("<I", 0)  # no next IFD
    )


def png(path: str, rows: list[list[int]], orientation: int) -> None:
    """Writes an 8 bit gray PNG of `rows` that states `orientation`."""
    height = len(rows)
    width = len(rows[0])
    raw = b"".join(b"\x00" + bytes(row) for row in rows)

    def chunk(kind: bytes, payload: bytes) -> bytes:
        crc = zlib.crc32(kind + payload) & 0xFFFFFFFF
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", crc)

    header = struct.pack(">IIBBBBB", width, height, 8, GRAY, 0, 0, 0)
    document = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"eXIf", exif(orientation))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    with open(path, "wb") as handle:
        handle.write(document)


def split_rows() -> list[list[int]]:
    """The rows of the quadrant picture the yuv fixtures are encoded from."""
    return [
        [
            SPLIT_LEVELS[row // (SPLIT_HEIGHT // 2)][column // (SPLIT_WIDTH // 2)]
            for column in range(SPLIT_WIDTH)
        ]
        for row in range(SPLIT_HEIGHT)
    ]


def webp_chunks(document: bytes) -> list[tuple[bytes, bytes]]:
    """Splits a webp file into its RIFF chunks."""
    if document[:4] != b"RIFF" or document[8:12] != b"WEBP":
        raise ValueError("not a webp file")
    chunks = []
    offset = 12
    while offset + 8 <= len(document):
        fourcc = document[offset : offset + 4]
        size = struct.unpack("<I", document[offset + 4 : offset + 8])[0]
        chunks.append((fourcc, document[offset + 8 : offset + 8 + size]))
        offset += 8 + size + (size & 1)
    return chunks


def webp_document(chunks: list[tuple[bytes, bytes]]) -> bytes:
    """Builds a webp file out of RIFF chunks."""
    body = b"".join(
        fourcc + struct.pack("<I", len(payload)) + payload + (b"\x00" if len(payload) & 1 else b"")
        for fourcc, payload in chunks
    )
    return b"RIFF" + struct.pack("<I", len(body) + 4) + b"WEBP" + body


def extended_header(payload: bytes) -> bytes:
    """The ten byte header a lossy webp needs once it carries an exif chunk."""
    if payload[3:6] != VP8_START_CODE:
        raise ValueError("the source webp is not a lossy one")
    width = struct.unpack("<H", payload[6:8])[0] & 0x3FFF
    height = struct.unpack("<H", payload[8:10])[0] & 0x3FFF
    return (
        bytes([VP8X_EXIF_FLAG, 0, 0, 0])
        + (width - 1).to_bytes(3, "little")
        + (height - 1).to_bytes(3, "little")
    )


def tag_webp(source: bytes, orientation: int) -> bytes:
    """States `orientation` in a webp file, beside the picture it states it of.

    The bitstream is copied as it is, so the picture does not depend on the
    code: only the exif chunk and the header that announces it are written.
    """
    chunks = webp_chunks(source)
    picture = next((payload for fourcc, payload in chunks if fourcc == VP8_CHUNK), None)
    if picture is None:
        raise ValueError("the source webp holds no bitstream")
    kept = [(fourcc, payload) for fourcc, payload in chunks if fourcc not in (EXIF_CHUNK, VP8X_CHUNK)]
    return webp_document(
        [(VP8X_CHUNK, extended_header(picture)), *kept, (EXIF_CHUNK, exif(orientation))]
    )


def main() -> None:
    os.makedirs(FIXTURES, exist_ok=True)
    rows = [[row * WIDTH + column + 1 for column in range(WIDTH)] for row in range(HEIGHT)]
    for orientation in range(1, 9):
        name = f"orientation-{orientation}.png"
        png(os.path.join(FIXTURES, name), rows, orientation)
    png(os.path.join(FIXTURES, "orientation-split.png"), split_rows(), 1)

    # The bitstream of a lossy webp needs an encoder, so the file the three
    # tagged ones are cut from is made once by hand and the script only states a
    # code in it.
    base = os.path.join(FIXTURES, WEBP_SOURCE)
    if not os.path.exists(base):
        print(f"{base} is missing: encode it from orientation-split.png with")
        print("  magick orientation-split.png -quality 90 -define webp:method=4 orientation-split.webp")
        return
    with open(base, "rb") as handle:
        source = handle.read()
    for orientation in WEBP_ORIENTATIONS:
        target = os.path.join(FIXTURES, f"orientation-{orientation}.webp")
        with open(target, "wb") as handle:
            handle.write(tag_webp(source, orientation))
    print(f"wrote the orientation fixtures to {FIXTURES}")


if __name__ == "__main__":
    main()
