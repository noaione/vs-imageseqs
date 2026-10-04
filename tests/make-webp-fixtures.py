r"""Creates the webp fixtures that carry an embedded ICC profile.

Run from the repository root with `cwebp` on ``PATH``:

    python tests/make-webp-fixtures.py

The six committed webp fixtures between them hold `VP8 `, `VP8L`, `VP8X`, `EXIF`
and the animation chunks, and **not one of them holds an `ICCP` chunk**. The
profile is the `ImgSeqHasICC` fact and the opt-in `ICCProfile` property, so that
path had no coverage at all -- and it is the one a probe written from the
bitstream is most likely to get wrong, because the profile is a chunk the walk has
to find rather than a number it can read where it stands.

Three shapes, because the profile can arrive on any of them: lossy, lossless, and
one with an alpha channel. The script **dumps the chunks the file actually holds**
after writing it, which is the lesson every fixture script in this tree taught:
`cwebp` copies metadata from its input rather than being handed a profile:
`-metadata icc` carries over the `iCCP` chunk the source png holds. A build
without ICC support would write a file without one rather than failing.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures"
PROFILE = FIXTURES / "icc-srgb.icc"

SIZE = 16


def run(*args: str) -> None:
    subprocess.run(list(args), check=True)


def chunks(path: pathlib.Path) -> str:
    """The four character chunk ids the file holds, in the order they appear."""
    data = path.read_bytes()
    found: list[str] = []
    at = 12
    while at + 8 <= len(data):
        identifier = data[at : at + 4].decode("ascii", "replace")
        size = int.from_bytes(data[at + 4 : at + 8], "little")
        found.append(identifier)
        at += 8 + size + (size & 1)
    return " ".join(found)


def source(name: str, alpha: bool) -> pathlib.Path:
    """Writes the picture every fixture holds, as a png cwebp reads."""
    path = FIXTURES / name
    run(
        "magick",
        "-size",
        f"{SIZE}x{SIZE}",
        "gradient:red-blue",
        # A fully opaque alpha channel is no alpha at all: `cwebp` drops it and
        # writes no `ALPH` chunk, so the file would keep a name that claims one.
        *(
            ["-alpha", "set", "-channel", "A", "-evaluate", "set", "50%", "+channel"]
            if alpha
            else []
        ),
        "-depth",
        "8",
        "-profile",
        str(PROFILE),
        str(path),
    )
    return path


def main() -> int:
    if not PROFILE.exists():
        print(f"missing the profile fixture {PROFILE}")
        return 1
    FIXTURES.mkdir(parents=True, exist_ok=True)

    rgb = source("_src-webp-rgb.png", False)
    rgba = source("_src-webp-rgba.png", True)

    for name, src, options, required in [
        ("webp-icc.webp", rgb, ["-q", "80"], ["ICCP"]),
        ("webp-icc-lossless.webp", rgb, ["-lossless"], ["ICCP"]),
        (
            "webp-icc-alpha.webp",
            rgba,
            ["-q", "80", "-alpha_q", "100"],
            # The alpha channel is why this one is here: a `VP8 ` payload and
            # an `ALPH` chunk is the shape the probe has to report as `Rgba8`.
            ["ICCP", "ALPH"],
        ),
    ]:
        path = FIXTURES / name
        run("cwebp", *options, "-metadata", "icc", str(src), "-o", str(path))
        found = chunks(path)
        missing = [chunk for chunk in required if chunk not in found]
        note = f"   <-- MISSING {' '.join(missing)}" if missing else ""
        print(f"  {name:26} {len(path.read_bytes()):6} bytes  chunks: {found}{note}")

    for leftover in ("_src-webp-rgb.png", "_src-webp-rgba.png"):
        target = FIXTURES / leftover
        if target.exists():
            target.unlink()

    print(f"wrote the webp icc fixtures to {FIXTURES}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
