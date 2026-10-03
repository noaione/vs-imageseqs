"""Validates ``ReadAlpha`` against the fixtures made by ``make-alpha-fixtures.py``.

Run from the repository root after ``cargo build --release``, with the
repository's virtual environment active:

    python tests/readalpha.vpy
"""

from __future__ import annotations

import hashlib
import os
import random
import struct
import subprocess
import sys
from pathlib import Path

import vapoursynth as vs

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
PLUGIN_NAME = {
    "win32": "vs_imageseqs.dll",
    "darwin": "libvs_imageseqs.dylib",
}.get(sys.platform, "libvs_imageseqs.so")
PLUGIN = Path(
    os.environ.get(
        "IMGSEQS_PLUGIN",
        str(ROOT / "target" / "release" / PLUGIN_NAME),
    )
)
FIXTURES = HERE / "fixtures"

# How long a frame request on an item that holds no picture may take before the
# validator calls it a hang. The request runs in a child process, so a decode
# loop that never returns fails this check instead of stalling the runner.
NO_PICTURE_TIMEOUT = 60

# The child that asks for that frame. It loads the plugin under test and reports
# the error it got, so the parent can tell a failed request from a hanging one.
NO_PICTURE_CHILD = """
import os
import sys

import vapoursynth as vs


class Policy(vs.EnvironmentPolicy):
    def on_policy_registered(self, special_api):
        self.api = special_api
        self.environment = special_api.create_environment(vs.CoreCreationFlags.DISABLE_AUTO_LOADING)

    def on_policy_cleared(self):
        self.api.destroy_environment(self.environment)

    def get_current_environment(self):
        return self.environment

    def set_environment(self, environment):
        return self.environment


vs.register_policy(Policy())
core = vs.core
core.std.LoadPlugin(os.environ["IMGSEQS_PLUGIN"])
clip = core.imgseqs.Read(files=[sys.argv[1]], prefetch=int(sys.argv[2]))
try:
    clip.get_frame(0)
except Exception as error:
    print(error)
    sys.exit(0)
print("the frame request returned a frame")
sys.exit(1)
"""


class ValidationPolicy(vs.EnvironmentPolicy):
    """Keep an installed plugin from shadowing the binary under test."""

    def on_policy_registered(self, special_api):
        self.api = special_api
        self.environment = special_api.create_environment(vs.CoreCreationFlags.DISABLE_AUTO_LOADING)

    def on_policy_cleared(self):
        self.api.destroy_environment(self.environment)

    def get_current_environment(self):
        return self.environment

    def set_environment(self, environment):
        return self.environment


if not vs.has_policy():
    vs.register_policy(ValidationPolicy())

core = vs.core
core.std.LoadPlugin(str(PLUGIN))

FAILURES: list[str] = []


def check(condition: bool, message: str) -> None:
    if condition:
        print(f"ok - {message}")
    else:
        print(f"FAIL - {message}")
        FAILURES.append(message)


def fixture(name: str) -> str:
    return str(FIXTURES / name)


def read_alpha(**arguments):
    """Calls `ReadAlpha` and returns its color and alpha clips."""
    result = core.imgseqs.ReadAlpha(**arguments)
    assert isinstance(result, dict), f"ReadAlpha returned {type(result).__name__}"
    assert sorted(result) == ["alpha", "clip"], f"ReadAlpha returned {sorted(result)}"
    return result["clip"], result["alpha"]


def samples(frame, plane: int = 0) -> list:
    """Returns the samples of one plane; the frame view has no row padding."""
    raw = frame[plane].tobytes()
    if frame.format.bytes_per_sample == 1:
        return list(raw)
    if frame.format.sample_type == vs.FLOAT:
        return [struct.unpack("<f", raw[index : index + 4])[0] for index in range(0, len(raw), 4)]
    return [struct.unpack("<H", raw[index : index + 2])[0] for index in range(0, len(raw), 2)]


def almost(actual: list, expected: list, tolerance: float = 1e-6) -> bool:
    return len(actual) == len(expected) and all(
        abs(a - b) <= tolerance for a, b in zip(actual, expected)
    )


def test_rgba8() -> None:
    result = core.imgseqs.ReadAlpha(files=[fixture("alpha-rgba8.png")])
    check(
        sorted(result) == ["alpha", "clip"],
        f"rgba8: ReadAlpha returns the clip and alpha keys (got {sorted(result)})",
    )
    clip, alpha = read_alpha(files=[fixture("alpha-rgba8.png")])
    check(len(clip) == 1 and len(alpha) == 1, "rgba8: both clips have one frame")
    check(clip.format.name == "RGB24", f"rgba8: color clip is RGB24 (got {clip.format.name})")
    check(alpha.format.name == "Gray8", f"rgba8: alpha clip is Gray8 (got {alpha.format.name})")

    color = clip.get_frame(0)
    frame = alpha.get_frame(0)
    check(samples(color, 0) == [1, 4, 7, 10, 13, 16], "rgba8: red plane drops the alpha sample")
    check(samples(frame) == [9, 8, 7, 6, 5, 4], "rgba8: alpha plane")
    check(frame.props["ImgSeqAlpha"] == 1, "rgba8: alpha frames carry ImgSeqAlpha")
    check("ImgSeqAlpha" not in color.props, "rgba8: color frames have no ImgSeqAlpha")
    check(frame.props.get("_Matrix") is None, "rgba8: alpha frames are not marked as RGB")
    check(color.props.get("_Matrix") == vs.MATRIX_RGB, "rgba8: color frames are marked as RGB")
    check(str(frame.props["ImgSeqPath"]).endswith("alpha-rgba8.png"), "rgba8: alpha frame keeps ImgSeqPath")
    check(frame.props["ImgSeqIndex"] == 0, "rgba8: alpha frame keeps ImgSeqIndex")
    check(
        frame.props["ImgSeqOriginalColorType"] == color.props["ImgSeqOriginalColorType"],
        "rgba8: both clips describe the same source",
    )


def test_la8() -> None:
    clip, alpha = read_alpha(files=[fixture("alpha-la8.png")])
    check(clip.format.name == "Gray8", f"la8: color clip is Gray8 (got {clip.format.name})")
    check(alpha.format.name == "Gray8", f"la8: alpha clip is Gray8 (got {alpha.format.name})")
    check(samples(clip.get_frame(0)) == [10, 20, 30, 40, 50, 60], "la8: gray plane")
    check(samples(alpha.get_frame(0)) == [1, 2, 3, 4, 5, 6], "la8: alpha plane")


def test_la16() -> None:
    clip, alpha = read_alpha(files=[fixture("alpha-la16.png")])
    check(alpha.format.name == "Gray16", f"la16: alpha clip is Gray16 (got {alpha.format.name})")
    check(
        samples(alpha.get_frame(0)) == [65535, 16384, 32768, 4096, 2048, 1],
        "la16: alpha plane keeps the sample depth",
    )
    check(samples(clip.get_frame(0)) == [100, 200, 300, 400, 500, 600], "la16: gray plane")


def test_rgba16() -> None:
    clip, alpha = read_alpha(files=[fixture("alpha-rgba16.png")])
    check(alpha.format.name == "Gray16", f"rgba16: alpha clip is Gray16 (got {alpha.format.name})")
    check(
        samples(alpha.get_frame(0)) == [100, 200, 300, 400, 500, 600],
        "rgba16: alpha plane",
    )
    check(
        samples(clip.get_frame(0), 2) == [3000, 6000, 9000, 12000, 15000, 18000],
        "rgba16: blue plane drops the alpha sample",
    )


def test_dds_and_farbfeld() -> None:
    """The image crate's DDS and farbfeld readers reach the normal frame path."""
    clip, alpha = read_alpha(files=[fixture("alpha-dds.dds")])
    color = clip.get_frame(0)
    frame = alpha.get_frame(0)
    check(color.format.name == "RGB24", f"dds: color clip is RGB24 (got {color.format.name})")
    check(frame.format.name == "Gray8", f"dds: alpha clip is Gray8 (got {frame.format.name})")
    check(samples(color, 0) == [255] * 16, "dds: DXT5 red plane")
    check(samples(color, 1) == [0] * 16, "dds: DXT5 green plane")
    check(samples(color, 2) == [0] * 16, "dds: DXT5 blue plane")
    check(samples(frame) == [0, 255] * 8, "dds: DXT5 alpha plane")

    clip, alpha = read_alpha(files=[fixture("alpha-rgba16.ff")])
    color = clip.get_frame(0)
    frame = alpha.get_frame(0)
    check(
        color.format.name == "RGB48",
        f"farbfeld: color clip is RGB48 (got {color.format.name})",
    )
    check(
        frame.format.name == "Gray16",
        f"farbfeld: alpha clip is Gray16 (got {frame.format.name})",
    )
    check(samples(color, 0) == [1000, 4000, 7000, 10000], "farbfeld: red plane")
    check(samples(color, 1) == [2000, 5000, 8000, 11000], "farbfeld: green plane")
    check(samples(color, 2) == [3000, 6000, 9000, 12000], "farbfeld: blue plane")
    check(samples(frame) == [100, 200, 300, 400], "farbfeld: alpha plane")


def test_jpeg2000() -> None:
    """JP2 and bare J2K files use the OpenJPEG path and keep their depth."""
    cases = [
        ("alpha-jp2-gray8.jp2", "Gray8", [7, 8, 9, 10, 11, 12], None),
        ("alpha-jp2-rgb8.jp2", "RGB24", None, [1, 4, 7, 10, 13, 16]),
        ("alpha-j2k-rgb8.j2k", "RGB24", None, [1, 4, 7, 10, 13, 16]),
        ("alpha-jp2-lossy.jp2", "RGB24", None, None),
        ("alpha-jp2-rgb16.jp2", "RGB48", None, [1000, 4000, 7000, 10000, 13000, 16000]),
    ]
    files = [fixture(name) for name, *_ in cases]
    clip, alpha = read_alpha(files=files, mismatch=True)
    for index, (name, format_name, gray, red) in enumerate(cases):
        color = clip.get_frame(index)
        opaque = alpha.get_frame(index)
        check(
            color.format.name == format_name,
            f"{name}: color clip is {format_name} (got {color.format.name})",
        )
        check(
            opaque.format.name == ("Gray16" if format_name == "RGB48" else "Gray8"),
            f"{name}: opaque alpha keeps the source depth",
        )
        if gray is not None:
            check(samples(color) == gray, f"{name}: gray plane")
        if red is not None:
            check(samples(color, 0) == red, f"{name}: red plane")
        check(
            samples(opaque) == ([65535] * 6 if format_name == "RGB48" else [255] * 6),
            f"{name}: opaque alpha plane",
        )


def test_opaque_fill() -> None:
    _, alpha = read_alpha(files=[fixture("alpha-rgb8.png")])
    check(alpha.format.name == "Gray8", "rgb8: an alpha clip is still produced")
    check(samples(alpha.get_frame(0)) == [255] * 6, "rgb8: files without alpha are opaque")

    _, gray_alpha = read_alpha(files=[fixture("alpha-l8.png")])
    check(samples(gray_alpha.get_frame(0)) == [255] * 6, "l8: files without alpha are opaque")


def test_float() -> None:
    clip, alpha = read_alpha(files=[fixture("alpha-rgba32f.tiff")])
    check(clip.format.name == "RGBS", f"float: color clip is RGBS (got {clip.format.name})")
    check(alpha.format.name == "GrayS", f"float: alpha clip is GrayS (got {alpha.format.name})")
    check(
        almost(samples(alpha.get_frame(0)), [0.25, 0.75, 0.0, 1.0, 0.5, 0.125]),
        "float: alpha plane",
    )


def test_alpha_presence_mixed() -> None:
    files = [fixture("alpha-rgb8.png"), fixture("alpha-rgba8.png")]
    _, alpha = read_alpha(files=files, mismatch=True)
    check(alpha.get_frame(0).format.name == "Gray8", "mixed: frame 0 keeps the 8 bit depth")
    check(samples(alpha.get_frame(0)) == [255] * 6, "mixed: frame 0 without alpha is opaque")
    check(samples(alpha.get_frame(1)) == [9, 8, 7, 6, 5, 4], "mixed: frame 1 keeps its alpha")


def test_variable_depth() -> None:
    files = [fixture("alpha-rgb8.png"), fixture("alpha-rgba16.png")]
    _, alpha = read_alpha(files=files, mismatch=True)
    first = alpha.get_frame(0)
    second = alpha.get_frame(1)
    check(first.format.name == "Gray8", f"depth: frame 0 is Gray8 (got {first.format.name})")
    check(samples(first) == [255] * 6, "depth: frame 0 is opaque")
    check(second.format.name == "Gray16", f"depth: frame 1 is Gray16 (got {second.format.name})")
    check(
        samples(second) == [100, 200, 300, 400, 500, 600],
        "depth: frame 1 keeps its 16 bit alpha",
    )

    try:
        core.imgseqs.ReadAlpha(files=files)
    except Exception as error:
        check(True, f"depth: mismatch=False still rejects mixed formats ({error})")
    else:
        check(False, "depth: mismatch=False still rejects mixed formats")


def test_read_is_unchanged() -> None:
    clip = core.imgseqs.Read(files=[fixture("alpha-rgba8.png")])
    check(clip.format.name == "RGB24", "read: color clip is RGB24")
    frame = clip.get_frame(0)
    check(samples(frame, 1) == [2, 5, 8, 11, 14, 17], "read: green plane")
    check("ImgSeqAlpha" not in frame.props, "read: color frames have no ImgSeqAlpha")
    check(clip.fps_num == 24 and clip.fps_den == 1, "read: the default frame rate is unchanged")

    _, alpha = read_alpha(files=[fixture("alpha-rgba8.png")])
    check(
        alpha.fps_num == clip.fps_num and alpha.fps_den == clip.fps_den,
        "readalpha: the clips share the frame rate",
    )
    check(len(alpha) == len(clip), "readalpha: the clips share the frame count")


def test_prefetch_variants() -> None:
    for workers in (0, 1, 2, 4):
        clip, alpha = read_alpha(
            files=[fixture("alpha-rgba16.png")], prefetch=workers
        )
        check(
            samples(alpha.get_frame(0)) == [100, 200, 300, 400, 500, 600],
            f"prefetch={workers}: alpha plane",
        )
        # The color clip is requested after the alpha clip, so it reuses the decode.
        check(
            samples(clip.get_frame(0), 0) == [1000, 4000, 7000, 10000, 13000, 16000],
            f"prefetch={workers}: color plane",
        )


def test_interleaved_consumers() -> None:
    files = [
        fixture("alpha-rgba8.png"),
        fixture("alpha-la8.png"),
        fixture("alpha-rgb8.png"),
        fixture("alpha-l8.png"),
        fixture("alpha-rgba16.png"),
        fixture("alpha-la16.png"),
    ]
    expected = [
        [9, 8, 7, 6, 5, 4],
        [1, 2, 3, 4, 5, 6],
        [255] * 6,
        [255] * 6,
        [100, 200, 300, 400, 500, 600],
        [65535, 16384, 32768, 4096, 2048, 1],
    ]
    names = ["Gray8", "Gray8", "Gray8", "Gray8", "Gray16", "Gray16"]
    clip, alpha = read_alpha(files=files, mismatch=True, prefetch=2)
    check(len(alpha) == len(files), "interleaved: every file becomes one frame")
    check(len(clip) == len(files), "interleaved: the color clip has the same frame count")

    for index in range(len(files)):
        # Ask for the clips in a different order every time so both consumers
        # share one decode of every file.
        if index % 2 == 0:
            frame = alpha.get_frame(index)
            color = clip.get_frame(index)
        else:
            color = clip.get_frame(index)
            frame = alpha.get_frame(index)
        check(
            frame.format.name == names[index],
            f"interleaved: frame {index} is {names[index]} (got {frame.format.name})",
        )
        check(samples(frame) == expected[index], f"interleaved: alpha plane of frame {index}")
        check(samples(color, 0) == samples(color, 0), f"interleaved: color plane of frame {index}")


# The monochrome heif and avif fixtures are encoded from a seven by five source,
# so their sample count is not the six the png fixtures use.
MONO_WIDTH = 7
MONO_HEIGHT = 5

# The orientation fixtures are four wide and three tall, so a transposing exif
# code changes the clip's size as well as the order of its samples. The two jxl
# ones are the same pictures as "orientation-6.png" and "alpha-rgba8.png"
# encoded losslessly with cjxl, which keeps every sample and states the code in
# the codestream; see "make-orientation-fixtures.py".
ORIENTATION_WIDTH = 4
ORIENTATION_HEIGHT = 3

# The jpeg xl fixtures that state their own depth are four wide and three tall:
# the two grayscale ones hold the numbers one to twelve and the four channel one
# holds four planes of distinct numbers, so the alpha plane of a deeper file is
# checked apart from the colour planes that share its word.
DEPTH_WIDTH = 4
DEPTH_HEIGHT = 3

# The yuv orientation fixtures are four quadrants of four grey levels rather
# than the twelve different samples of the png ones, because a lossy encode
# cannot keep those: the four levels survive it and their order is enough to say
# which way the picture was turned.
SPLIT_WIDTH = 16
SPLIT_HEIGHT = 12
SPLIT_QUADRANTS = [[16, 90], [170, 235]]


def flip_h(picture: list[list[int]]) -> list[list[int]]:
    """Mirrors a picture horizontally."""
    return [row[::-1] for row in picture]


def flip_v(picture: list[list[int]]) -> list[list[int]]:
    """Mirrors a picture vertically."""
    return picture[::-1]


def transpose(picture: list[list[int]]) -> list[list[int]]:
    """Mirrors a picture across its main diagonal."""
    return [list(column) for column in zip(*picture)]


def shown(stored: list[list[int]], orientation: int) -> list[list[int]]:
    """The picture a viewer shows for an exif `orientation`.

    This is written the way the exif table reads, with a rotation and a mirror
    as separate steps, rather than with the transpose and flip pair the plugin
    writes with, so the two only agree when both are right.
    """
    match orientation:
        case 1:
            return stored
        case 2:
            return flip_h(stored)
        case 3:
            return flip_h(flip_v(stored))
        case 4:
            return flip_v(stored)
        case 5:
            return flip_h(flip_h(transpose(stored)))  # a quarter turn, mirrored
        case 6:
            return flip_h(transpose(stored))  # a quarter turn clockwise
        case 7:
            return flip_h(flip_v(transpose(stored)))  # the other turn, mirrored
        case 8:
            return flip_v(transpose(stored))  # a quarter turn counter-clockwise
        case _:
            raise AssertionError(f"unknown orientation {orientation}")


def ten_bit(value: int) -> int:
    """The ten bit sample an eight bit one is stored as.

    ``avifenc`` scales the eight bit source onto the ten bit range when it codes
    a deeper copy of it, and the lossless encode keeps exactly that number, so
    this is what the ten bit fixture holds rather than four times a sample.
    """
    return round(value * 1023 / 255)


def depth_samples(depth: int) -> tuple[list[int], list[int]]:
    """Return the gray and alpha samples in a native-depth fixture."""
    maximum = (1 << depth) - 1
    return (
        [
            maximum // 4 + 17 * x + 31 * y
            for y in range(MONO_HEIGHT)
            for x in range(MONO_WIDTH)
        ],
        [
            maximum - 13 * x - 29 * y
            for y in range(MONO_HEIGHT)
            for x in range(MONO_WIDTH)
        ],
    )


def test_monochrome_containers() -> None:
    """A monochrome heif and avif hand out one plane per sample.

    ``mono-alpha.png`` holds ``8 + 11x + 23y`` grey and ``240 - 9x - 13y`` alpha.
    The eight bit containers bring those numbers back unchanged in a ``Gray8``
    colour clip and a ``Gray8`` alpha clip. The ten bit avif is handed out as
    ``Gray10`` holding the ten bit number the encode stored, which is the eight
    bit one scaled onto the ten bit range: the format names the depth the file
    states and the samples are the ones that depth holds, rather than the same
    numbers left aligned in the sixteen bit word it used to be described as.
    The native-depth sources are sixteen-bit PNGs whose gray samples are
    left-aligned for the encoder. AVIF's normalized alpha samples are full-range
    scaled, so its output comes back as the exact right-aligned twelve-bit
    values. This covers the monochrome libheif path at both depths and the
    monochrome AVIF correction at twelve bits.
    """
    grey = [8 + 11 * x + 23 * y for y in range(MONO_HEIGHT) for x in range(MONO_WIDTH)]
    alpha_plane = [
        240 - 9 * x - 13 * y for y in range(MONO_HEIGHT) for x in range(MONO_WIDTH)
    ]
    cases = [
        ("mono-alpha.heic", "heic", "Gray8", lambda value: value),
        ("mono-alpha.avif", "avif", "Gray8", lambda value: value),
        ("mono-alpha-10.avif", "avif 10 bit", "Gray10", ten_bit),
    ]
    files = [fixture(name) for name, *_ in cases]
    clip, alpha = read_alpha(files=files, mismatch=True)
    check(len(clip) == len(files), "mono: every file becomes one frame")
    for index, (_, label, format_name, stored) in enumerate(cases):
        color = clip.get_frame(index)
        frame = alpha.get_frame(index)
        check(
            color.format.name == format_name,
            f"mono {label}: the color clip is {format_name} (got {color.format.name})",
        )
        check(
            frame.format.name == format_name,
            f"mono {label}: the alpha clip is {format_name} (got {frame.format.name})",
        )
        check(
            almost(samples(color), [stored(value) for value in grey]),
            f"mono {label}: the gray plane",
        )
        check(
            almost(samples(frame), [stored(value) for value in alpha_plane]),
            f"mono {label}: the alpha plane",
        )
        check(frame.props["ImgSeqAlpha"] == 1, f"mono {label}: alpha frames carry ImgSeqAlpha")

    for depth in (10, 12):
        depth_grey, depth_alpha = depth_samples(depth)
        format_name = f"Gray{depth}"
        name = f"mono-{depth}.heic"
        clip, alpha = read_alpha(files=[fixture(name)])
        color = clip.get_frame(0)
        frame = alpha.get_frame(0)
        check(
            color.format.name == format_name,
            f"mono {name}: the color clip is {format_name} (got {color.format.name})",
        )
        check(
            frame.format.name == format_name,
            f"mono {name}: the alpha clip is {format_name} (got {frame.format.name})",
        )
        check(samples(color) == depth_grey, f"mono {name}: the gray plane")
        check(samples(frame) == [((1 << depth) - 1)] * len(depth_grey), f"mono {name}: opaque alpha")

    depth_grey, depth_alpha = depth_samples(12)
    name = "mono-alpha-12.avif"
    clip, alpha = read_alpha(files=[fixture(name)])
    color = clip.get_frame(0)
    frame = alpha.get_frame(0)
    check(color.format.name == "Gray12", f"mono {name}: the color clip is Gray12 (got {color.format.name})")
    check(frame.format.name == "Gray12", f"mono {name}: the alpha clip is Gray12 (got {frame.format.name})")
    check(samples(color) == depth_grey, f"mono {name}: the gray plane")
    check(samples(frame) == depth_alpha, f"mono {name}: the alpha plane")
    check(frame.props["ImgSeqAlpha"] == 1, f"mono {name}: alpha frames carry ImgSeqAlpha")

    # The png pair the containers were encoded from has to agree with them.
    plain_clip, plain_alpha = read_alpha(files=[fixture("mono-alpha.png")])
    check(samples(plain_clip.get_frame(0)) == grey, "mono: the source png keeps the same gray plane")
    check(samples(plain_alpha.get_frame(0)) == alpha_plane, "mono: the source png keeps the same alpha")


def test_jxl_depths() -> None:
    """A jpeg xl that states its own depth is handed out at that depth.

    The three fixtures hold the same twelve positions and differ only in the
    depth their codestream states, which ``cjxl`` reads from the ``MAXVAL`` of
    the pgm or pam they were encoded from; see "make-alpha-fixtures.py". A file
    that states ten bits is a ``Gray10`` or ``RGB30`` frame holding the sample
    itself, and the twelve bit one is a ``Gray12`` frame, so all of them hand out
    the numbers below: moving the samples down out of the sixteen bit word the
    decoder returns by a depth of the reader's own, rather than by the one the
    file states, gives a different plane for at least one of them.

    ``jxl-rgba10.jxl`` holds an alpha channel at the same depth, which is where a
    deeper alpha plane of its own is checked: it is a ``Gray10`` plane of its own
    numbers, moved down the same way the colour planes are. The two gray
    fixtures hold no alpha channel, so their alpha clip is an opaque plane of
    their format instead: 1023 for the ten bit one and 4095 for the twelve bit
    one, rather than the 65535 the sixteen bit word would hold.
    """
    positions = [(x, y) for y in range(DEPTH_HEIGHT) for x in range(DEPTH_WIDTH)]

    def numbered(first: int) -> list[int]:
        """`first` upward along every row, which is how the fixtures are built."""
        return [first + x + DEPTH_WIDTH * y for x, y in positions]

    for name, format_name, opaque in (
        ("jxl-gray10.jxl", "Gray10", 1023),
        ("jxl-gray12.jxl", "Gray12", 4095),
    ):
        clip, alpha = read_alpha(files=[fixture(name)])
        color = clip.get_frame(0)
        frame = alpha.get_frame(0)
        check(
            color.format.name == format_name,
            f"{name}: the color clip is {format_name} (got {color.format.name})",
        )
        check(
            frame.format.name == format_name,
            f"{name}: the alpha clip is {format_name} (got {frame.format.name})",
        )
        check(samples(color) == numbered(1), f"{name}: the gray plane")
        check(
            samples(frame) == [opaque] * len(positions),
            f"{name}: the opaque alpha plane",
        )

    clip, alpha = read_alpha(files=[fixture("jxl-rgba10.jxl")])
    color = clip.get_frame(0)
    frame = alpha.get_frame(0)
    check(
        color.format.name == "RGB30",
        f"jxl-rgba10.jxl: the color clip is RGB30 (got {color.format.name})",
    )
    check(
        frame.format.name == "Gray10",
        f"jxl-rgba10.jxl: the alpha clip is Gray10 (got {frame.format.name})",
    )
    for plane, first in ((0, 1), (1, 13), (2, 25)):
        check(samples(color, plane) == numbered(first), f"jxl-rgba10.jxl: plane {plane}")
    check(samples(frame) == numbered(100), "jxl-rgba10.jxl: the alpha plane")


def test_color_containers() -> None:
    """A colour container that states no usable matrix keeps its rgb format.

    ``alpha-rgba8.avif`` states the identity matrix, whose samples are already
    r,g,b, so the file keeps the ``RGB24`` it has always been handed out as and
    the alpha item beside it is unchanged. ``alpha-rgba8.heic`` holds the same
    picture and states bt.601 for the yuv it codes instead, which is what
    ``test_yuv_containers`` covers.
    """
    clip, alpha = read_alpha(files=[fixture("alpha-rgba8.avif")])
    check(len(clip) == 1 and len(alpha) == 1, "color avif: both clips have one frame")
    color = clip.get_frame(0)
    frame = alpha.get_frame(0)
    check(
        color.format.name == "RGB24",
        f"color avif: the color clip stays RGB24 (got {color.format.name})",
    )
    check(samples(color, 0) == [1, 4, 7, 10, 13, 16], "color avif: red plane")
    check(samples(color, 1) == [2, 5, 8, 11, 14, 17], "color avif: green plane")
    check(samples(color, 2) == [3, 6, 9, 12, 15, 18], "color avif: blue plane")
    check(samples(frame) == [9, 8, 7, 6, 5, 4], "color avif: alpha plane")


def test_yuv_containers() -> None:
    """A container whose samples it describes hands out the yuv it holds.

    The three avif fixtures are crops of the ``sandbox/hitokage-sample`` files of
    the same layout and ``alpha-rgba8.heic`` is the heif a real encoder wrote for
    the same picture as ``alpha-rgba8.avif``: all four state bt.601 with the full
    range, so every one of them is handed out as the planar format its own
    samples are in rather than as the rgb a decoder used to build, the chroma
    planes have their own size, and the two properties that describe those
    samples are written.

    ``alpha-yuv420p.avif`` is the four by four source of ``yuv-rgba8.png``, whose
    four colour rows are 72 apart and whose four alpha columns are 0, 85, 170 and
    255, so a plane that was shifted, subsampled the wrong way or scaled shows up
    in the numbers below; see "make-alpha-fixtures.py" for the encodes.
    """
    cases = (
        ("avif-yuv420p.avif", "YUV420P8", 64, 48, 2, 2),
        ("avif-yuv422p.avif", "YUV422P8", 64, 48, 2, 1),
        ("avif-yuv444p10.avif", "YUV444P10", 32, 24, 1, 1),
        ("alpha-rgba8.heic", "YUV420P8", 3, 2, 2, 2),
    )
    files = [fixture(name) for name, *_ in cases]
    clip = core.imgseqs.Read(files=files, mismatch=True)
    check(len(clip) == len(files), "yuv: one frame per file")
    for index, (name, format_name, width, height, step_x, step_y) in enumerate(cases):
        frame = clip.get_frame(index)
        check(
            frame.format.name == format_name,
            f"yuv {name}: the clip is {format_name} (got {frame.format.name})",
        )
        check(
            (frame.width, frame.height) == (width, height),
            f"yuv {name}: the clip is {width}x{height} "
            f"(got {frame.width}x{frame.height})",
        )
        check(
            len(samples(frame, 1)) == (width // step_x) * (height // step_y),
            f"yuv {name}: the chroma plane is its own size",
        )
        check(
            frame.props.get("_Matrix") == 6,
            f"yuv {name}: the bt.601 matrix the file states "
            f"(got {frame.props.get('_Matrix')})",
        )
        check(
            frame.props.get("_Range") == 1,
            f"yuv {name}: the full range the file states "
            f"(got {frame.props.get('_Range')})",
        )
        check(
            "_ChromaLocation" not in frame.props,
            f"yuv {name}: a file that states no chroma position gains no property",
        )

    # None of those four holds an alpha item, so its alpha clip is the opaque
    # fill of the depth the page is at.
    fill = core.imgseqs.ReadAlpha(files=[files[0], files[2]], mismatch=True)["alpha"]
    check(
        set(samples(fill.get_frame(0))) == {255},
        "yuv: an eight bit page without an alpha item is filled with 255",
    )
    check(
        set(samples(fill.get_frame(1))) == {1023},
        "yuv: a ten bit page without an alpha item is filled with 1023",
    )

    # The 4:2:0 page with an alpha item, whose every sample the fixture states.
    clip, alpha = read_alpha(files=[fixture("alpha-yuv420p.avif")])
    color = clip.get_frame(0)
    frame = alpha.get_frame(0)
    check(
        color.format.name == "YUV420P8",
        f"yuv alpha-yuv420p: the color clip is YUV420P8 (got {color.format.name})",
    )
    check(samples(color, 1) == [128] * 4, "yuv alpha-yuv420p: the chroma of a neutral source")
    check(samples(color, 2) == [128] * 4, "yuv alpha-yuv420p: and its other chroma plane")
    check(
        samples(color)
        == [
            0, 0, 0, 0, 72, 72, 72, 72,
            144, 144, 144, 144, 216, 216, 216, 216,
        ],
        "yuv alpha-yuv420p: the luma plane is the four rows of the source",
    )
    check(
        frame.format.name == "Gray8",
        f"yuv alpha-yuv420p: the alpha clip is YUV420P8's Gray8 (got {frame.format.name})",
    )
    check(
        samples(frame) == [0, 85, 170, 255] * 4,
        "yuv alpha-yuv420p: the alpha plane is the four columns of the source",
    )
    check(frame.props["ImgSeqAlpha"] == 1, "yuv alpha-yuv420p: alpha frames carry ImgSeqAlpha")

    # The heic of the same picture as the avif keeps its own alpha item.
    _, heic_alpha = read_alpha(files=[fixture("alpha-rgba8.heic")])
    check(
        samples(heic_alpha.get_frame(0)) == [9, 8, 7, 6, 5, 4],
        "yuv alpha-rgba8.heic: the alpha item is unchanged",
    )

    # A yuv page beside an rgb one is two formats, so the sequence needs
    # ``mismatch``, the same promise a mixed file list keeps.
    try:
        core.imgseqs.Read(files=[fixture("avif-yuv420p.avif"), fixture("alpha-rgb8.png")])
    except Exception as error:
        check(True, f"yuv: a yuv page beside an rgb one is rejected ({str(error).strip()})")
    else:
        check(False, "yuv: a yuv page beside an rgb one is rejected")


def test_errors() -> None:
    cases = (
        ({"files": []}, "an empty file list"),
        ({"files": [fixture("alpha-rgba8.png")], "prefetch": -1}, "a negative prefetch"),
        ({"files": [fixture("does-not-exist.png")]}, "a missing file"),
        ({"files": [fixture("alpha-rgba8.png")], "fpsden": 0}, "a zero denominator"),
    )
    for arguments, label in cases:
        try:
            core.imgseqs.ReadAlpha(**arguments)  # pyright: ignore[reportArgumentType]
        except Exception as error:
            check(True, f"errors: {label} is rejected ({str(error).strip()})")
        else:
            check(False, f"errors: {label} is rejected")


def test_no_picture() -> None:
    """An item that holds no frame fails a frame request instead of hanging.

    The fixture is a well-formed avif whose primary item payload is temporal
    delimiters and no coded frame, so it passes a probe that reads its container
    and has no picture to hand over. The request runs in a child process with a
    timeout, because the defect it guards against is a decode loop that never
    returns: that stalls the requesting thread, a lookahead worker and the pool's
    destruction, and a check that hangs cannot report a failure. Both lookahead
    settings are covered, and the child has to exit on its own for the shutdown
    to count.
    """
    for prefetch in (0, 4):
        environment = dict(os.environ, IMGSEQS_PLUGIN=str(PLUGIN))
        try:
            result = subprocess.run(
                [
                    sys.executable,
                    "-c",
                    NO_PICTURE_CHILD,
                    fixture("avif-no-picture.avif"),
                    str(prefetch),
                ],
                capture_output=True,
                text=True,
                timeout=NO_PICTURE_TIMEOUT,
                env=environment,
            )
        except subprocess.TimeoutExpired:
            check(
                False,
                f"no picture: a frame request at prefetch={prefetch} ends within "
                f"{NO_PICTURE_TIMEOUT}s",
            )
            continue
        output = f"{result.stdout.strip()} {result.stderr.strip()}".strip()
        check(
            result.returncode == 0 and "holds no picture" in output,
            f"no picture: a frame request at prefetch={prefetch} fails instead of "
            f"hanging ({output})",
        )


def test_avif_extents() -> None:
    """An item split over several extents is joined here, not handed over.

    ``avif-split-extents.avif`` is the coded item of ``avif-yuv420p.avif`` cut in
    half and located as two extents, which is a legal way to write the same item.
    The extents are one payload split across the container, so joining them is a
    concatenation in the order ``iloc`` lists them, and the two files are then
    the same picture through the *same* decoder.

    That is the check: not a sample this script was told to expect, but the two
    files agreeing plane for plane. Before the join this reader declined the file
    and the ``image`` decoder read it, so the plugin handed out the r,g,b that
    decoder builds where the file it was cut from came out as the yuv the
    container states.
    """
    clip = core.imgseqs.Read(files=[fixture("avif-split-extents.avif")])
    check(
        clip.format.name == "YUV420P8",
        f"extents: the clip is the yuv the container states (got {clip.format.name})",
    )
    check(
        (clip.width, clip.height) == (64, 48),
        f"extents: the clip is 64x48 (got {clip.width}x{clip.height})",
    )
    frame = clip.get_frame(0)

    # The file it was cut from holds the same picture, read by the same reader.
    whole = core.imgseqs.Read(files=[fixture("avif-yuv420p.avif")])
    check(
        whole.format.name == "YUV420P8",
        f"extents: the file it was cut from is yuv too (got {whole.format.name})",
    )
    check(
        (whole.width, whole.height) == (clip.width, clip.height),
        "extents: and the same size",
    )
    other = whole.get_frame(0)

    # Every plane of the joined file has to equal the plane it was cut from: the
    # join is exact, not approximate, and a wrong order or a missed byte shows up
    # here rather than in a sample or two.
    for plane in range(frame.format.num_planes):
        check(
            samples(frame, plane) == samples(other, plane),
            f"extents: plane {plane} is the one the whole file holds "
            f"({len(samples(frame, plane))} samples)",
        )
    check(
        [samples(frame, plane)[0] for plane in range(3)] == [74, 126, 128],
        "extents: the first pixel is the one the reference decoder reads "
        f"(got {[samples(frame, plane)[0] for plane in range(3)]})",
    )

    # Both files are one format now, so a sequence holding the two needs no
    # `mismatch` and is one frame per file.
    both = core.imgseqs.Read(
        files=[fixture("avif-split-extents.avif"), fixture("avif-yuv420p.avif")]
    )
    check(
        both.num_frames == 2,
        f"extents: the two are one format, so a sequence holds both "
        f"(got {both.num_frames} frames)",
    )
    check(
        both.format.name == "YUV420P8",
        f"extents: as that one format (got {both.format.name})",
    )

def test_avif_grid() -> None:
    """A 2x2 grid avif is joined by libheif, which is who decodes it now.

    ``avif-grid.avif`` is written by ``avifenc -g 2x2`` from
    ``avif-grid-source.png``: four 128x128 squares of one sample each, so the
    grid's four cells are four distinct uniform values (top left 8, top right
    10, bottom left 15, bottom right 17).

    This check was written to fail the moment the grid started decoding, as the
    signal to turn it into this sample check. The plugin's own avif walker still
    excludes a grid in ``Meta::native_eligible``; what reads it now is
    ``libheif``, which is who a container this tree's walk refuses goes to
    instead of the ``image`` decoder. That decoder failed here -- it has no
    monochrome avif, and the grid's cells are monochrome, so it answered
    ``Invalid argument``.

    The samples are the ones ``avifdec`` reads back, and they are taken at the
    middle of each cell, so a wrong cell order or an offset wrong by a whole
    tile cannot pass.
    """
    clip = core.imgseqs.Read(files=[fixture("avif-grid.avif")])
    check(
        (clip.width, clip.height) == (256, 256),
        f"grid: the clip is the grid joined, 256x256 (got {clip.width}x{clip.height})",
    )
    check(
        clip.format.name == "Gray8",
        f"grid: and is gray, which is what the container states (got {clip.format.name})",
    )
    frame = clip.get_frame(0)
    luma = samples(frame, 0)
    for x, y, value in ((64, 64, 8), (192, 64, 10), (64, 192, 15), (192, 192, 17)):
        got = luma[y * clip.width + x]
        check(got == value, f"grid: the cell at {x},{y} is {value} (got {got})")

def test_alpha_demand() -> None:
    """A colour-only read does not decode an alpha item it never hands out.

    ``avif-broken-alpha.avif`` is the coded item of ``avif-yuv420p.avif`` with an
    alpha item beside it that holds no frame at all. The alpha item is a coded
    item of its own, so `Read` reads this file — the item is not part of what it
    hands out — while `ReadAlpha` still fails on it, which is the clip it exists
    for. The colour planes are the ones the file it was cut from gives, so what
    was skipped is the alpha and nothing else.
    """
    clip = core.imgseqs.Read(files=[fixture("avif-broken-alpha.avif")])
    check(
        clip.format.name == "YUV420P8",
        f"demand: a colour-only read of a file with a broken alpha item is yuv "
        f"(got {clip.format.name})",
    )
    check(
        (clip.width, clip.height) == (64, 48),
        f"demand: and is 64x48 (got {clip.width}x{clip.height})",
    )
    frame = clip.get_frame(0)
    whole = core.imgseqs.Read(files=[fixture("avif-yuv420p.avif")]).get_frame(0)
    check(
        samples(frame) == samples(whole),
        "demand: the colour planes are the ones the file it was cut from gives",
    )
    check(
        samples(frame, 1) == samples(whole, 1) and samples(frame, 2) == samples(whole, 2),
        "demand: and so are its chroma planes",
    )

    # The same file through ReadAlpha is what still has to fail, because that is
    # the clip the item would have been decoded for.
    _, alpha = read_alpha(files=[fixture("avif-broken-alpha.avif")])
    try:
        alpha.get_frame(0)
    except Exception as error:
        check(True, f"demand: an alpha read still fails on that item ({str(error).strip()})")
    else:
        check(False, "demand: an alpha read still fails on that item")

    # A file whose alpha item is intact keeps handing it out.
    _, intact = read_alpha(files=[fixture("alpha-yuv420p.avif")])
    check(
        samples(intact.get_frame(0)) == [0, 85, 170, 255] * 4,
        "demand: a file with an intact alpha item still hands it out",
    )


def test_orientation() -> None:
    """The exif orientation is applied, and only when it is asked for.

    The fixtures hold one four by three picture of twelve different samples and
    differ in the code their ``eXIf`` chunk states, so every code has to hand
    out its own rearrangement of the same numbers, and the four transposing ones
    a clip that is three wide and four tall. ``apply_rotation=False`` has to hand
    out the stored picture of the same file, with the same ``ImgSeqOrientation``,
    and the size the orientation produces is the size ``mismatch`` compares,
    which is why a rotated page beside an upright one is rejected at creation
    rather than at the first frame.
    """
    stored = [
        [row * ORIENTATION_WIDTH + column + 1 for column in range(ORIENTATION_WIDTH)]
        for row in range(ORIENTATION_HEIGHT)
    ]
    for orientation in range(1, 9):
        picture = shown(stored, orientation)
        width, height = len(picture[0]), len(picture)
        clip, alpha = read_alpha(files=[fixture(f"orientation-{orientation}.png")])
        check(
            (clip.width, clip.height) == (width, height),
            f"exif {orientation}: the clip is {width}x{height} (got {clip.width}x{clip.height})",
        )
        frame = clip.get_frame(0)
        samples_flat = [sample for row in picture for sample in row]
        check(samples(frame) == samples_flat, f"exif {orientation}: the picture the file states")
        check(
            frame.props["ImgSeqOrientation"] == orientation,
            f"exif {orientation}: ImgSeqOrientation keeps the code the file states",
        )
        check(
            samples(alpha.get_frame(0)) == [255] * len(samples_flat),
            f"exif {orientation}: the opaque alpha clip matches the size",
        )

    # The stored picture, for files whose two sizes differ, which is what says
    # the transform and not the decoder produced the sizes above.
    files = [fixture(f"orientation-{orientation}.png") for orientation in range(1, 9)]
    clip, _ = read_alpha(files=files, apply_rotation=False)
    check(
        (clip.width, clip.height) == (ORIENTATION_WIDTH, ORIENTATION_HEIGHT),
        f"exif: apply_rotation=False keeps the stored size (got {clip.width}x{clip.height})",
    )
    stored_flat = [sample for row in stored for sample in row]
    for orientation in range(1, 9):
        frame = clip.get_frame(orientation - 1)
        check(
            samples(frame) == stored_flat,
            f"exif {orientation}: apply_rotation=False hands out the stored picture",
        )
        check(
            frame.props["ImgSeqOrientation"] == orientation,
            f"exif {orientation}: apply_rotation=False still reports the code",
        )

    # A rotated page beside an upright one is one stored size and two shown
    # ones, so the clip cannot have a constant size.
    mixed = [fixture("orientation-6.png"), fixture("orientation-1.png")]
    try:
        core.imgseqs.Read(files=mixed)
    except Exception as error:
        check(
            "3x4" in str(error) and "4x3" in str(error),
            f"exif: a rotated page beside an upright one is rejected ({str(error).strip()})",
        )
    else:
        check(False, "exif: a rotated page beside an upright one is rejected")

    unrotated = core.imgseqs.Read(files=mixed, apply_rotation=False)
    check(
        (unrotated.width, unrotated.height) == (ORIENTATION_WIDTH, ORIENTATION_HEIGHT),
        "exif: the same two files agree when rotation is off",
    )
    variable = core.imgseqs.Read(files=mixed, mismatch=True)
    check(
        (variable.width, variable.height) == (0, 0),
        "exif: mismatch=True hands out a clip of frames of their own size",
    )
    frame = variable.get_frame(0)
    check(
        (frame.width, frame.height) == (ORIENTATION_HEIGHT, ORIENTATION_WIDTH),
        f"exif: the rotated frame is its own size (got {frame.width}x{frame.height})",
    )


def quadrant_values(frame) -> list:
    """The sample at the centre of each quadrant of the luma plane, row major."""
    plane = samples(frame)
    width, height = frame.width, frame.height
    centres = [
        (width // 4 + column * width // 2, height // 4 + row * height // 2)
        for row in range(2)
        for column in range(2)
    ]
    return [plane[y * width + x] for x, y in centres]


def test_orientation_yuv() -> None:
    """A rotated lossy webp is turned in its own planes, not in an rgb buffer.

    The webp fixtures hold the four quadrants of the picture they were encoded
    from and differ in the code their ``EXIF`` chunk states, so this is the
    planar write path: the frame stays ``YUV420P8``, the chroma planes are half
    the size of the luma one and still transformed with their own dimensions,
    and a transposing code changes the size of all three. The levels are only
    compared by their order because the compression is lossy, which is also why
    the four of them are far apart.
    """
    cases = (
        (2, SPLIT_WIDTH, SPLIT_HEIGHT),
        (6, SPLIT_HEIGHT, SPLIT_WIDTH),
        (8, SPLIT_HEIGHT, SPLIT_WIDTH),
    )
    for orientation, width, height in cases:
        expected = [value for row in shown(SPLIT_QUADRANTS, orientation) for value in row]
        clip, _ = read_alpha(files=[fixture(f"orientation-{orientation}.webp")])
        check(
            (clip.width, clip.height) == (width, height),
            f"exif webp {orientation}: the clip is {width}x{height} "
            f"(got {clip.width}x{clip.height})",
        )
        frame = clip.get_frame(0)
        check(
            frame.format.name == "YUV420P8",
            f"exif webp {orientation}: a lossy webp stays yuv (got {frame.format.name})",
        )
        check(
            len(samples(frame, 1)) == (width // 2) * (height // 2),
            f"exif webp {orientation}: the chroma plane is its own size",
        )
        observed = quadrant_values(frame)
        for first in range(4):
            for second in range(first + 1, 4):
                check(
                    (observed[first] < observed[second])
                    == (expected[first] < expected[second]),
                    f"exif webp {orientation}: quadrant {first} against {second}",
                )
        check(
            frame.props["ImgSeqOrientation"] == orientation,
            f"exif webp {orientation}: ImgSeqOrientation keeps the code the file states",
        )


def test_orientation_jxl() -> None:
    """A jpeg xl states its code in the codestream, and it is undone on request.

    The decoder applies the code itself, so the plugin is what has to report it
    and what has to undo it: ``Read`` hands out the display picture beside the
    code, and ``apply_rotation=False`` the stored one, which is the pair of
    promises every other format keeps. The fixture is the same picture as
    ``orientation-6.png`` encoded losslessly, so the two frames have to agree
    sample for sample rather than merely in their order, and the alpha fixture is
    ``alpha-rgba8.png`` encoded the same way.
    """
    stored = [
        [row * ORIENTATION_WIDTH + column + 1 for column in range(ORIENTATION_WIDTH)]
        for row in range(ORIENTATION_HEIGHT)
    ]
    picture = shown(stored, 6)
    display_flat = [sample for row in picture for sample in row]

    clip, alpha = read_alpha(files=[fixture("orientation-6.jxl")])
    check(
        (clip.width, clip.height) == (ORIENTATION_HEIGHT, ORIENTATION_WIDTH),
        f"jxl: the clip is the display size (got {clip.width}x{clip.height})",
    )
    frame = clip.get_frame(0)
    check(
        frame.format.name == "Gray8",
        f"jxl: a grayscale file stays gray (got {frame.format.name})",
    )
    check(samples(frame) == display_flat, "jxl: the picture the file states")
    check(
        frame.props["ImgSeqOrientation"] == 6,
        f"jxl: ImgSeqOrientation is the code the file states "
        f"(got {frame.props.get('ImgSeqOrientation')})",
    )
    check(
        samples(alpha.get_frame(0)) == [255] * len(display_flat),
        "jxl: a file with no alpha channel gets the opaque one",
    )

    # The same picture out of the png beside it, which is what says the two
    # formats agree on what the code means rather than each being self
    # consistent, and one clip holding both says they agree on size and format.
    reference, _ = read_alpha(files=[fixture("orientation-6.png")])
    check(
        samples(reference.get_frame(0)) == samples(frame),
        "jxl: the same picture as the png of the same code",
    )
    together = core.imgseqs.Read(
        files=[fixture("orientation-6.png"), fixture("orientation-6.jxl")]
    )
    check(len(together) == 2, "jxl: the png and the jxl are one sequence")

    # The stored picture, which is the code undone rather than applied.
    stored_flat = [sample for row in stored for sample in row]
    plain = core.imgseqs.Read(files=[fixture("orientation-6.jxl")], apply_rotation=False)
    check(
        (plain.width, plain.height) == (ORIENTATION_WIDTH, ORIENTATION_HEIGHT),
        f"jxl: apply_rotation=False hands out the stored size (got {plain.width}x{plain.height})",
    )
    plain_frame = plain.get_frame(0)
    check(
        samples(plain_frame) == stored_flat,
        "jxl: apply_rotation=False hands out the stored picture",
    )
    check(
        plain_frame.props["ImgSeqOrientation"] == 6,
        "jxl: apply_rotation=False still reports the code",
    )

    # A lossless jxl of the rgba fixture keeps its color and its alpha samples.
    color, alpha = read_alpha(files=[fixture("alpha-rgba8.jxl")])
    reference_color, reference_alpha = read_alpha(files=[fixture("alpha-rgba8.png")])
    check(color.format.name == "RGB24", f"jxl: rgba is RGB24 (got {color.format.name})")
    check(alpha.format.name == "Gray8", f"jxl: the alpha clip is Gray8 (got {alpha.format.name})")
    check(
        samples(color.get_frame(0)) == samples(reference_color.get_frame(0)),
        "jxl: the color of the rgba fixture, channel for channel",
    )
    check(
        samples(alpha.get_frame(0)) == samples(reference_alpha.get_frame(0)),
        "jxl: the alpha of the rgba fixture",
    )


def test_color_metadata() -> None:
    """The colour a file states reaches the frame, and only where it states one.

    ``cicp-rgb8.png`` and ``cicp-rgb8.avif`` hold the same picture as
    ``alpha-rgb8.png`` and one statement more: the png carries a ``cICP`` chunk
    and the avif an ``nclx`` box, both of them stating bt.2020 primaries with the
    hlg transfer function, so both frames have to report those two codes while
    the plain png reports neither. The widths are different -- four one-byte
    fields against four sixteen-bit ones -- and the containers are read by
    different code, which is what the pair is for; ``make-cicp-fixtures.py``
    documents the chunk and the encode.

    The files a real encoder wrote are read beside them: a heic, an avif and a
    jxl all state bt.709 primaries with the sRGB transfer function, and the
    monochrome pair states them for a gray frame, which carries no matrix. A
    jpeg xl that states only some of the codes writes only those properties, and
    the matrix a heic states for its coded yuv is not adopted by the rgb frame
    it is handed out as. The code points are the ones ``docs/IMPLEMENTATION.md``
    and ``docs/improvements/08-color-metadata.md`` name, which are the h.273
    codes vapoursynth itself uses.
    """
    plain, stated, boxed = (
        fixture("alpha-rgb8.png"),
        fixture("cicp-rgb8.png"),
        fixture("cicp-rgb8.avif"),
    )
    clip = core.imgseqs.Read(files=[plain, stated, boxed])
    check(len(clip) == 3, "colour: one frame per file")
    frames = [clip.get_frame(index) for index in range(3)]
    check(
        all(samples(frame) == samples(frames[0]) for frame in frames[1:]),
        "colour: the three files hand out the same picture",
    )
    check(
        "_Primaries" not in frames[0].props and "_Transfer" not in frames[0].props,
        "colour: a file that states no colour gains no property",
    )
    for index, label in ((1, "png cICP chunk"), (2, "avif nclx box")):
        props = frames[index].props
        check(
            props.get("_Primaries") == 9,
            f"colour {label}: bt.2020 primaries (got {props.get('_Primaries')})",
        )
        check(
            props.get("_Transfer") == 18,
            f"colour {label}: the hlg transfer function (got {props.get('_Transfer')})",
        )

    # What encoders wrote, one file at a time so each frame's properties come
    # from its own file.
    stated_srgb = (
        "alpha-rgba8.avif",
        "alpha-rgba8.heic",
        "alpha-rgba8.jxl",
        "mono-alpha.heic",
        "mono-alpha.avif",
        "mono-alpha-10.avif",
    )
    for name in stated_srgb:
        frame = core.imgseqs.Read(files=[fixture(name)]).get_frame(0)
        props = frame.props
        check(
            props.get("_Primaries") == 1,
            f"colour {name}: bt.709 primaries (got {props.get('_Primaries')})",
        )
        check(
            props.get("_Transfer") == 13,
            f"colour {name}: the sRGB transfer function (got {props.get('_Transfer')})",
        )
        check(
            props.get("ImgSeqHasICC", 0) == 0,
            f"colour {name}: a file with no icc profile reports none "
            f"(got {props.get('ImgSeqHasICC')})",
        )

    # The heic states a smpte 170m matrix beside those primaries, which is the
    # matrix of the yuv samples it hands out and of the full range they are in.
    heic = core.imgseqs.Read(files=[fixture("alpha-rgba8.heic")]).get_frame(0)
    check(
        heic.props.get("_Matrix") == 6,
        f"colour heic: the matrix of its coded yuv (got {heic.props.get('_Matrix')})",
    )
    check(
        heic.props.get("_Range") == 1,
        f"colour heic: and the range of those samples (got {heic.props.get('_Range')})",
    )
    # The avif of the same picture states the identity beside its primaries, and
    # the rgb frame it keeps has the identity matrix of an rgb frame.
    avif = core.imgseqs.Read(files=[fixture("alpha-rgba8.avif")]).get_frame(0)
    check(
        avif.props.get("_Matrix") == 0,
        f"colour avif: an rgb frame keeps its own matrix (got {avif.props.get('_Matrix')})",
    )
    check(
        avif.props.get("_Range") == 1,
        f"colour avif: and its own range (got {avif.props.get('_Range')})",
    )

    # A jxl that states the transfer function but not the primaries writes one
    # property and not the other, rather than guessing the other one.
    jxl = core.imgseqs.Read(files=[fixture("orientation-6.jxl")]).get_frame(0)
    check(
        jxl.props.get("_Transfer") == 13 and "_Primaries" not in jxl.props,
        "colour jxl: a code the file does not state writes no property "
        f"(got {jxl.props.get('_Primaries')} and {jxl.props.get('_Transfer')})",
    )

    # A format that states its colour some other way keeps what it had: a lossy
    # webp holds bt.601 yuv, and neither code is one the plugin reads out of a
    # file, so its frame stays the bt.470 background matrix in the limited range
    # the plugin assumed before it read any colour out of a container.
    webp = core.imgseqs.Read(files=[fixture("lossy.webp")]).get_frame(0)
    check(
        webp.props.get("_Matrix") == 5 and webp.props.get("_Range") == 0,
        f"colour webp: the assumed matrix and range "
        f"(got {webp.props.get('_Matrix')} and {webp.props.get('_Range')})",
    )
    check(
        "_Primaries" not in webp.props and "_Transfer" not in webp.props,
        "colour webp: no primaries or transfer is written",
    )


def test_icc_profile_properties() -> None:
    """Exports exact embedded ICC bytes without changing the decoded frames."""
    expected = (FIXTURES / "icc-srgb.icc").read_bytes()

    disabled = core.imgseqs.Read(files=[fixture("icc-rgb8.png")]).get_frame(0)
    check(disabled.props.get("ImgSeqHasICC") == 1, "icc: the source reports its profile")
    check("ICCProfile" not in disabled.props, "icc: export is disabled by default")

    enabled = core.imgseqs.Read(
        files=[fixture("icc-rgb8.png")],
        icc_profile=True,
    ).get_frame(0)
    check(
        enabled.props.get("ICCProfile") == expected,
        "icc: enabled export keeps the exact profile bytes",
    )
    check(samples(enabled) == samples(disabled), "icc: export does not change pixels")

    plain = read_alpha(files=[fixture("icc-rgba8.png")])
    result = core.imgseqs.ReadAlpha(
        files=[fixture("icc-rgba8.png")],
        icc_profile=True,
    )
    color = result["clip"].get_frame(0)
    alpha = result["alpha"].get_frame(0)
    check(
        color.props.get("ICCProfile") == expected
        and alpha.props.get("ICCProfile") == expected,
        "icc: ReadAlpha copies the profile to both frames",
    )
    check(
        samples(color) == samples(plain[0].get_frame(0))
        and samples(alpha) == samples(plain[1].get_frame(0)),
        "icc: ReadAlpha export does not change either clip",
    )


# The container-transform fixtures state their orientation as the `irot` and
# `imir` item properties of the image item rather than as an exif tag, so the
# code beside each one is the exif code those two describe. The source they are
# encoded from is the same four by three grid of twelve distinct samples as the
# exif fixtures, so the picture each code produces is the one `shown` states.
CONTAINER_STORED = [
    [row * ORIENTATION_WIDTH + column + 7 for column in range(ORIENTATION_WIDTH)]
    for row in range(ORIENTATION_HEIGHT)
]

# (fixture, the exif code its container transform is, the format it is handed
# out as, the stored picture). The monochrome one is encoded from the three by
# two gray fixture, whose samples are 7 to 12.
CONTAINER_CASES = (
    ("orientation-avif-none.avif", 1, "YUV444P8", CONTAINER_STORED),
    ("orientation-avif-irot-1.avif", 8, "YUV444P8", CONTAINER_STORED),
    ("orientation-avif-irot-2.avif", 3, "YUV444P8", CONTAINER_STORED),
    ("orientation-avif-irot-3.avif", 6, "YUV444P8", CONTAINER_STORED),
    ("orientation-avif-imir-0.avif", 4, "YUV444P8", CONTAINER_STORED),
    ("orientation-avif-imir-1.avif", 2, "YUV444P8", CONTAINER_STORED),
    ("orientation-avif-irot-1-imir-0.avif", 5, "YUV444P8", CONTAINER_STORED),
    ("orientation-avif-irot-1-imir-1.avif", 7, "YUV444P8", CONTAINER_STORED),
    # The identity matrix the frame properties cannot name keeps the rgb path.
    ("orientation-avif-rgb-irot-1.avif", 8, "RGB24", CONTAINER_STORED),
    ("orientation-avif-mono-irot-1.avif", 8, "Gray8", [[7, 8, 9], [10, 11, 12]]),
    ("orientation-heic-rot-90.heic", 6, "YUV420P8", CONTAINER_STORED),
    ("orientation-heic-rot-180.heic", 3, "YUV420P8", CONTAINER_STORED),
    ("orientation-heic-rot-270.heic", 8, "YUV420P8", CONTAINER_STORED),
)


def test_orientation_container() -> None:
    """The orientation a container states as `irot` and `imir` is applied.

    An avif or heic states its orientation as two transformative item properties
    rather than as an exif tag, which is what the plugin had never read: it handed
    a rotated heic out the way `libheif` drew it while reporting orientation 1,
    and it handed a rotated avif out as the stored picture. Both are the same
    question the exif fixtures ask, so each fixture is checked the same way, with
    the expected picture built from the code its container states rather than
    from the plugin's own idea of it. `apply_rotation=False` has to hand the
    stored picture back for all of them, which is the half a decoder that applies
    the transform itself - `libheif` here - cannot do on its own.
    """
    for name, code, format_name, stored in CONTAINER_CASES:
        picture = shown(stored, code)
        width, height = len(picture[0]), len(picture)
        clip = core.imgseqs.Read(files=[fixture(name)], prefetch=0)
        check(
            clip.format.name == format_name,
            f"container {name}: {format_name} (got {clip.format.name})",
        )
        check(
            (clip.width, clip.height) == (width, height),
            f"container {name}: the clip is {width}x{height} "
            f"(got {clip.width}x{clip.height})",
        )
        frame = clip.get_frame(0)
        check(
            samples(frame) == [sample for row in picture for sample in row],
            f"container {name}: the picture the container states",
        )
        check(
            frame.props["ImgSeqOrientation"] == code,
            f"container {name}: ImgSeqOrientation is the code it describes "
            f"(got {frame.props['ImgSeqOrientation']})",
        )

        unrotated = core.imgseqs.Read(files=[fixture(name)], prefetch=0, apply_rotation=False)
        check(
            (unrotated.width, unrotated.height) == (len(stored[0]), len(stored)),
            f"container {name}: apply_rotation=False keeps the stored size "
            f"(got {unrotated.width}x{unrotated.height})",
        )
        stored_frame = unrotated.get_frame(0)
        check(
            samples(stored_frame) == [sample for row in stored for sample in row],
            f"container {name}: apply_rotation=False hands out the stored picture",
        )
        check(
            stored_frame.props["ImgSeqOrientation"] == code,
            f"container {name}: apply_rotation=False still reports the code",
        )

    # A rotated page beside an upright one is one stored size and two shown ones,
    # so the clip cannot have a constant size: the size the orientation produces
    # is the size `mismatch` compares.
    mixed = [fixture("orientation-avif-irot-1.avif"), fixture("orientation-avif-none.avif")]
    try:
        core.imgseqs.Read(files=mixed)
    except Exception as error:
        check(
            "3x4" in str(error) and "4x3" in str(error),
            f"container: a rotated page beside an upright one is rejected "
            f"({str(error).strip()})",
        )
    else:
        check(False, "container: a rotated page beside an upright one is rejected")

    together = core.imgseqs.Read(files=mixed, apply_rotation=False)
    check(
        (together.width, together.height) == (ORIENTATION_WIDTH, ORIENTATION_HEIGHT),
        "container: the same two files agree when rotation is off",
    )


def frame_hash(frame) -> str:
    """Content hash of every plane of a frame, padding excluded."""
    digest = hashlib.sha256()
    for plane in range(frame.format.num_planes):
        digest.update(frame[plane].tobytes())
    return digest.hexdigest()


def test_animation() -> None:
    """Checks that animated inputs expand to their displayed timeline.

    The four fixture animations all state the same four pictures over 600 ms,
    so they sample onto the same output timeline whatever their container is:
    14 frames at 24 fps, and 6 at 10 fps. A still file keeps its one frame, and
    a list that mixes the two adds up.
    """
    # The avif, and every format below it that re-encodes the same source
    # frames, states four pictures of 80, 170, 110 and 240 ms. The heic fixture
    # holds four equal 150 ms samples, because the encoder that made it accepts
    # one duration for the whole sequence, so its pictures land on different
    # output ticks even though its timeline is the same 600 ms long.
    animations = [
        ("animation.png", [0, 2, 6, 9]),
        ("animation.gif", [0, 2, 6, 9]),
        ("animation.webp", [0, 2, 6, 9]),
        ("animation.jxl", [0, 2, 6, 9]),
        ("animation.avif", [0, 2, 6, 9]),
        ("animation.heic", [0, 4, 8, 11]),
    ]

    for name, starts in animations:
        clip = core.imgseqs.Read(files=[fixture(name)])
        check(clip.num_frames == 14, f"animation: {name} is 14 frames at 24 fps (got {clip.num_frames})")
        check(
            (clip.width, clip.height) == (16, 12),
            f"animation: {name} is 16x12 (got {clip.width}x{clip.height})",
        )

        # The pictures start at 0, 80, 250 and 360 ms, so at 24 fps output
        # frames 0, 2, 6 and 9 are the four distinct presentations.
        hashes = [frame_hash(clip.get_frame(index)) for index in range(clip.num_frames)]
        # Each picture is held until the next one starts, and a picture that
        # becomes visible part way through an output tick is shown from the
        # first tick at or after it.
        ends = [*starts[1:], clip.num_frames]
        for index, (start, end) in enumerate(zip(starts, ends)):
            check(
                len({hashes[frame] for frame in range(start, end)}) == 1,
                f"animation: {name} holds presentation {index} across frames {start}..{end}",
            )
        check(
            len({hashes[start] for start in starts}) == 4,
            f"animation: {name} shows four distinct pictures",
        )

        # Random access: a shuffled order has to agree with the sequential one.
        order = list(range(clip.num_frames))
        random.Random(7).shuffle(order)
        for index in order:
            check(
                frame_hash(clip.get_frame(index)) == hashes[index],
                f"animation: {name} frame {index} is the same however it is asked for",
            )

        # A different output rate maps the same timeline onto different ticks.
        slower = core.imgseqs.Read(files=[fixture(name)], fpsnum=10, fpsden=1)
        check(
            slower.num_frames == 6,
            f"animation: {name} is 6 frames at 10 fps (got {slower.num_frames})",
        )

    # A still of a format that has an animation path stays one frame.
    for name in ["alpha-rgb8.png", "alpha-rgba8.png", "alpha-rgba8.jxl"]:
        clip = core.imgseqs.Read(files=[fixture(name)])
        check(
            clip.num_frames == 1,
            f"animation: {name} is still one frame (got {clip.num_frames})",
        )

    # A 16-bit APNG keeps its depth, which is why it is composed here. Its two
    # pictures hold for 100 ms and 200 ms, so the 300 ms timeline is seven ticks
    # at 24 fps and the second picture starts at the third.
    rgba16 = core.imgseqs.ReadAlpha(files=[fixture("animation-rgba16.png")])
    check(
        rgba16["clip"].format.name == "RGB48",
        f"animation: a 16-bit APNG stays 16-bit (got {rgba16['clip'].format.name})",
    )
    check(
        rgba16["clip"].num_frames == 7,
        f"animation: a 16-bit APNG is 7 frames (got {rgba16['clip'].num_frames})",
    )
    sixteen = [frame_hash(rgba16["clip"].get_frame(index)) for index in range(7)]
    check(
        len(set(sixteen[:3])) == 1 and len(set(sixteen[3:])) == 1 and sixteen[0] != sixteen[3],
        "animation: a 16-bit APNG switches picture at the third frame",
    )

    # Alpha varies between the presentations of an animation.
    animated = core.imgseqs.ReadAlpha(files=[fixture("animation.png")])
    colour, alpha = animated["clip"], animated["alpha"]
    check(colour.num_frames == alpha.num_frames, "animation: both clips have the same length")
    alpha_hashes = {
        frame_hash(alpha.get_frame(index))
        for index in range(alpha.num_frames)
    }
    check(len(alpha_hashes) > 1, "animation: the alpha clip varies between presentations")

    # A list that mixes a still with an animation adds up, and the still keeps
    # its one frame at the front.
    mixed = core.imgseqs.Read(files=[fixture("animation.gif"), fixture("animation.gif")])
    check(
        mixed.num_frames == 28,
        f"animation: two animations play back to back (got {mixed.num_frames})",
    )

    # A sequence and a still of another size need mismatch, exactly as two
    # stills of different sizes do.
    try:
        core.imgseqs.Read(files=[fixture("animation.gif"), fixture("alpha-rgb8.png")])
        check(False, "animation: a mixed-size list is refused without mismatch")
    except vs.Error as error:
        check(
            "expected frame 0" in str(error),
            "animation: a mixed-size list is refused without mismatch",
        )
    variable = core.imgseqs.Read(
        files=[fixture("animation.gif"), fixture("alpha-rgb8.png")], mismatch=1
    )
    check(
        variable.num_frames == 15,
        f"animation: mismatch allows a still after an animation (got {variable.num_frames})",
    )


def main() -> None:
    for test in (
        test_rgba8,
        test_la8,
        test_la16,
        test_rgba16,
        test_dds_and_farbfeld,
        test_jpeg2000,
        test_float,
        test_opaque_fill,
        test_alpha_presence_mixed,
        test_variable_depth,
        test_read_is_unchanged,
        test_monochrome_containers,
        test_jxl_depths,
        test_color_containers,
        test_yuv_containers,
        test_color_metadata,
        test_icc_profile_properties,
        test_orientation,
        test_orientation_yuv,
        test_orientation_jxl,
        test_orientation_container,
        test_prefetch_variants,
        test_interleaved_consumers,
        test_errors,
        test_no_picture,
        test_avif_extents,
        test_avif_grid,
        test_alpha_demand,
        test_animation,
    ):
        print(f"--- {test.__name__}")
        test()

    if FAILURES:
        print(f"\n{len(FAILURES)} check(s) failed")
        raise SystemExit(1)
    print("\nall checks passed")


if __name__ == "__main__":
    main()
