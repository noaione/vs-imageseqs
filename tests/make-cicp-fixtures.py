r"""Creates the colour metadata fixtures used by ``tests/readalpha.vpy``.

Run from the repository root with any Python 3:

    python tests/make-cicp-fixtures.py

``cicp-rgb8.png`` holds the same three by two rgb8 samples as
``alpha-rgb8.png`` and one more chunk: a ``cICP`` chunk stating bt.2020
primaries, the hlg transfer function and the full range flag. That chunk is the
only difference between the two files, so the validator reads both and states
that the picture is the same while the two properties differ, which is what
makes the claim come from the container rather than from the samples.

The matrix coefficients are 0, and have to be: rgb is the only colour model a
png has, so the specification requires that field to be 0. The full range flag
is a whole byte that conforming files write as 0 or 1, which is read as a flag
rather than as the top bit of a bit field the way an ``nclx`` box stores it.

``cicp-rgb8.avif`` is encoded from that png by hand, because only an encoder can
produce one. Re-run the command from the repository root whenever the source
changes, with ``avifenc`` on ``PATH``, then re-run ``tests/readalpha.vpy``:

    avifenc --lossless --cicp 9/18/0 -r full -o tests/fixtures/cicp-rgb8.avif tests/fixtures/cicp-rgb8.png

``--lossless`` keeps the samples the png holds, and requires the identity matrix
in return, so ``--cicp 9/18/0`` writes the same three code points into the
``nclx`` colour box that the png states in its ``cICP`` chunk, in the wider
fields the box holds them in; ``-r full`` sets the full range flag beside them.
Both files therefore hold the same picture and state the same colour, which is
what lets a check read one property out of each and find no more in the file
that states none; see ``docs/improvements/08-color-metadata.md``.
"""

from __future__ import annotations

import os
import struct
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

WIDTH = 3
HEIGHT = 2

# PNG color type 2: three channels of eight bits.
RGB = 2

# The code points both fixtures state, in the order the `cICP` chunk stores them:
# bt.2020 primaries, the hlg transfer function, and the identity matrix. r,g,b has
# no matrix to state other than the identity, and a png requires that field to be
# 0 outright, so neither container says anything about the matrix of this file
# that its frame would not already assume.
PRIMARIES = 9
TRANSFER = 18
MATRIX = 0


def png(path: str, rows: list[list[int]]) -> None:
    """Writes an 8 bit rgb PNG of `rows` with a `cICP` chunk."""

    def chunk(kind: bytes, payload: bytes) -> bytes:
        crc = zlib.crc32(kind + payload) & 0xFFFFFFFF
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", crc)

    header = struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, RGB, 0, 0, 0)
    # The video full range flag is the fourth byte, which conforming files write
    # as 0 or 1.
    cicp = bytes([PRIMARIES, TRANSFER, MATRIX, 1])
    raw = b"".join(b"\x00" + bytes(row) for row in rows)
    document = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        # A `cICP` chunk goes after `IHDR` and before the image data.
        + chunk(b"cICP", cicp)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    with open(path, "wb") as handle:
        handle.write(document)


def main() -> None:
    os.makedirs(FIXTURES, exist_ok=True)
    png(
        os.path.join(FIXTURES, "cicp-rgb8.png"),
        [
            [1, 2, 3, 4, 5, 6, 7, 8, 9],
            [10, 11, 12, 13, 14, 15, 16, 17, 18],
        ],
    )
    print(f"wrote the colour metadata fixtures to {FIXTURES}")


if __name__ == "__main__":
    main()
