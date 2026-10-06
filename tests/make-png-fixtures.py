r"""Creates the png feature-set fixtures used by ``tests/readalpha.vpy``.

Run from the repository root with ImageMagick on ``PATH``:

    python tests/make-png-fixtures.py

Every committed png fixture was eight or sixteen bits, colour type 0, 2, 4 or 6,
and **not one of them was interlaced**. Three feature sets this reader claims had
therefore never been exercised: Adam7 interlacing, which `src/formats/png.rs`
declines and hands to the `image` crate; a palette page, whose indices the walk
expands itself; and a bit depth of one, two or four.

The script writes one of each and then **dumps what was actually produced**, which
is the lesson every fixture script in this tree taught: ImageMagick accepts the
options meant to select a subtype and quietly writes a different one when the build
cannot. A fixture that is not the subtype it claims is worse than no fixture.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures"

WIDTH = 37
HEIGHT = 23


def magick(*args: str) -> None:
    # The encoder stamps a `tIME` chunk and a date `tEXt` into every png it writes,
    # so the same command twice does not produce the same bytes: the picture is
    # identical and the file is not. Both are excluded, which is what lets a run
    # of this script be re-derived by whoever gets the tree next.
    #
    # The settings go before the output path because ImageMagick applies them in
    # order, and the last argument is always the file being written.
    subprocess.run(
        ["magick", *args[:-1], "-define", "png:exclude-chunk=date,time", args[-1]],
        check=True,
    )


def source(name: str, depth: int = 8) -> str:
    """Writes the picture every fixture holds, as a png the encoders read."""
    path = str(FIXTURES / name)
    magick("-size", f"{WIDTH}x{HEIGHT}", "gradient:black-white", "-depth", str(depth), path)
    return path


def header(path: str) -> tuple[int, int, int]:
    """The bit depth, colour type and interlace flag the IHDR actually states."""
    data = pathlib.Path(path).read_bytes()
    # A png is eight bytes of signature, then the IHDR chunk: four of length,
    # four of type, then width, height, bit depth, colour type and the rest.
    return data[24], data[25], data[28]


def report(label: str, path: str, want: tuple[int, int] | None = None) -> None:
    depth, color_type, interlace = header(path)
    note = ""
    if want is not None and (depth, color_type) != want:
        note = f"  <-- ASKED FOR {want}"
    print(
        f"  {os.path.basename(path):32} bit depth={depth:2} colour type={color_type}"
        f" interlace={interlace}{note}"
    )


def main() -> int:
    FIXTURES.mkdir(parents=True, exist_ok=True)
    grey = source("_src-grey.png", 8)
    colour = str(FIXTURES / "_src-colour.png")
    magick("-size", f"{WIDTH}x{HEIGHT}", "gradient:red-blue", "-depth", "8", colour)
    # A few flat colours, so a palette of eight is enough and a transparent
    # index is a colour rather than a blend.
    blocks = str(FIXTURES / "_src-blocks.png")
    magick("-size", f"{WIDTH}x{HEIGHT}", "xc:white", "-fill", "red", "-draw", "rectangle 0,0 18,11", "-fill", "blue", "-draw", "rectangle 19,0 36,11", "-fill", "green", "-draw", "rectangle 0,12 18,22", blocks)

    print("interlaced (Adam7):")
    # The colour type is named rather than inferred. Left to itself ImageMagick
    # writes a palette whenever the picture holds few enough colours -- a 37x23
    # gradient does -- so a fixture meant to be truecolour silently is not.
    for name, src, color_type, extra, want in [
        ("png-interlaced-rgb8.png", colour, "2", [], (8, 2)),
        ("png-interlaced-rgba8.png", colour, "6", ["-alpha", "set"], (8, 6)),
        ("png-interlaced-gray8.png", grey, "0", [], (8, 0)),
        ("png-interlaced-palette8.png", blocks, "3", ["-colors", "8"], (4, 3)),
    ]:
        path = str(FIXTURES / name)
        magick(
            src,
            *extra,
            "-define",
            f"png:color-type={color_type}",
            "-interlace",
            "PNG",
            path,
        )
        report(name, path, want)

    # The same pictures without interlacing, so that a check can say interlacing
    # changes nothing but the order the samples are stored in.
    print("the same pictures, not interlaced:")
    for name, src, color_type, extra in [
        ("png-plain-rgb8.png", colour, "2", []),
        ("png-plain-gray8.png", grey, "0", []),
    ]:
        path = str(FIXTURES / name)
        magick(src, *extra, "-define", f"png:color-type={color_type}", path)
        report(name, path, (8, int(color_type)))

    print("not interlaced, for comparison:")
    for name, src, extra, want in [
        ("png-palette8.png", blocks, ["-colors", "8"], (4, 3)),
        # A palette page whose alpha is a `tRNS` chunk is deliberately **not**
        # here, and that is a gap rather than a choice: ImageMagick will not write
        # one. Four orderings of `-transparent` and `-colors` were tried against a
        # four colour source and every one produced a `PLTE` and no `tRNS`, so
        # covering that path needs the chunk written by hand -- the way the two
        # part EXR and the still gif fixtures are. Recorded here rather than
        # papered over with a fixture whose name claims a chunk it does not hold.
        # Enough colours that the table needs a whole byte, which is the only
        # shape that takes the walk's own index expansion rather than the
        # decoder's.
        ("png-palette256.png", colour, ["-colors", "256"], (8, 3)),
    ]:
        path = str(FIXTURES / name)
        magick(
            src,
            *extra,
            "-define",
            "png:color-type=3",
            path,
        )
        report(name, path, want)

    print("narrower than eight bits:")
    for bits in (1, 2, 4):
        name = f"png-gray{bits}.png"
        path = str(FIXTURES / name)
        magick(
            grey,
            # Posterising first gives the encoder a reason to use the depth:
            # a 256 level gradient asked for two bits is written as one.
            "-posterize",
            str(1 << bits),
            "-define",
            "png:color-type=0",
            "-depth",
            str(bits),
            path,
        )
        report(name, path, want=(bits, 0))

    for leftover in ["_src-grey.png", "_src-colour.png", "_src-blocks.png"]:
        target = FIXTURES / leftover
        if target.exists():
            target.unlink()

    print(f"wrote the png feature-set fixtures to {FIXTURES}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
