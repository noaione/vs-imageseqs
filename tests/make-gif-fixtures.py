r"""Creates the still gif fixtures used by ``tests/readalpha.vpy``.

Run from the repository root with any Python 3:

    python tests/make-gif-fixtures.py

Hand written rather than handed to Pillow, for two reasons. The interesting case
is a single frame that is **not** the whole logical screen -- a gif may state an
8x8 screen and draw a 3x2 rectangle at (2,1) -- and nothing here will produce
one for a file with a single frame: Pillow takes the logical screen from the
first image, so a one frame file it writes is always full canvas. And these
fixtures are the specification of what the still reader has to keep, so the bytes
are written out here rather than left to whatever a library version decides.

The image data is emitted with a clear code before every literal, which is the
standard way to write LZW without a compressor: the dictionary never grows past
its initial size, so the code width stays at ``min_code_size + 1`` bits and there
is no dictionary bookkeeping to get wrong. A clear resets it, and only one literal
ever follows a clear, so no entry is ever added.
"""

from __future__ import annotations

import pathlib
import struct

HERE = pathlib.Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures"

# Four entries, so a pixel code is three bits wide and the clear code is four.
PALETTE = bytes(
    [
        255, 0, 0,  # 0 red
        0, 255, 0,  # 1 green
        0, 0, 255,  # 2 blue
        255, 255, 0,  # 3 yellow
    ]
)
MIN_CODE_SIZE = 2
CLEAR = 1 << MIN_CODE_SIZE  # 4
END = CLEAR + 1  # 5
CODE_WIDTH = MIN_CODE_SIZE + 1  # 3


def lzw(indices: list[int]) -> bytes:
    """The pixel codes, with a clear before every literal, packed LSB first."""
    codes: list[int] = []
    for index in indices:
        codes += [CLEAR, index]
    codes.append(END)

    out = bytearray()
    bits = 0
    held = 0
    for code in codes:
        bits |= code << held
        held += CODE_WIDTH
        while held >= 8:
            out.append(bits & 0xFF)
            bits >>= 8
            held -= 8
    if held:
        out.append(bits & 0xFF)
    return bytes(out)


def blocks(data: bytes) -> bytes:
    """Image data is carried in sub-blocks of at most 255 bytes, then a zero."""
    out = bytearray()
    for at in range(0, len(data), 255):
        block = data[at : at + 255]
        out.append(len(block))
        out += block
    out.append(0)
    return bytes(out)


def gif(
    screen: tuple[int, int],
    rect: tuple[int, int, int, int],
    indices: list[int],
    transparent: int | None = None,
) -> bytes:
    """A GIF89a with one image, a global colour table and no local one."""
    left, top, width, height = rect
    # Global colour table present, one bit of colour resolution per primary and
    # a table of four entries (`size` is two to the size field plus one).
    flags = 0x80 | (0x01 << 4) | 0x01
    out = bytearray(b"GIF89a")
    out += struct.pack("<HH", *screen) + bytes([flags, 0, 0])
    out += PALETTE
    if transparent is not None:
        # Graphic control extension: no disposal, no user input, transparent
        # index set. A gif states transparency per frame, not per file.
        out += b"\x21\xf9\x04" + bytes([0x01, 0, 0, transparent]) + b"\x00"
    out += b"\x2c" + struct.pack("<HHHH", left, top, width, height) + bytes([0])
    out += bytes([MIN_CODE_SIZE]) + blocks(lzw(indices))
    out += b"\x3b"
    return bytes(out)


def main() -> int:
    FIXTURES.mkdir(parents=True, exist_ok=True)

    # A full canvas frame, so the screen and the rectangle agree.
    written = [
        (
            "gif-still.gif",
            gif((4, 4), (0, 0, 4, 4), [0, 1, 2, 3, 1, 2, 3, 0, 2, 3, 0, 1, 3, 0, 1, 2]),
            "4x4 screen, one 4x4 frame",
        ),
        # The case no writer here makes: an 8x8 screen with a 3x2 rectangle at
        # (2,1), so the screen around the frame is the reader's to fill and the
        # size it reports is a decision rather than an accident.
        (
            "gif-still-subrect.gif",
            gif((8, 8), (2, 1, 3, 2), [0, 1, 0, 1, 0, 1]),
            "8x8 screen, one 3x2 frame at (2,1)",
        ),
        # One index transparent, which is what a colour type of four channels
        # has to be decided from.
        (
            "gif-still-alpha.gif",
            gif(
                (4, 4),
                (0, 0, 4, 4),
                [0, 1, 2, 3, 1, 2, 3, 0, 2, 3, 0, 1, 3, 0, 1, 2],
                transparent=0,
            ),
            "4x4 screen, one 4x4 frame, index 0 transparent",
        ),
    ]

    for name, payload, note in written:
        (FIXTURES / name).write_bytes(payload)
        print(f"  + {name:26} {len(payload):5} bytes  {note}")

    print(f"wrote the still gif fixtures to {FIXTURES}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
