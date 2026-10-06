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
    return len(actual) == len(expected) and all(abs(a - b) <= tolerance for a, b in zip(actual, expected))


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


def test_qoi() -> None:
    """QOI is read by the plugin now, and its colours flag stays informative."""
    # Three channels: the file states no alpha, so the alpha clip is opaque
    # whatever the samples are.
    clip, alpha = read_alpha(files=[fixture("qoi-rgb8.qoi")])
    color = clip.get_frame(0)
    frame = alpha.get_frame(0)
    check(
        (clip.width, clip.height) == (8, 6),
        f"qoi: the clip is 8x6 (got {clip.width}x{clip.height})",
    )
    check(color.format.name == "RGB24", f"qoi: colour clip is RGB24 (got {color.format.name})")
    check(frame.format.name == "Gray8", f"qoi: alpha clip is Gray8 (got {frame.format.name})")
    check(
        clip.get_frame(0).props.get("ImgSeqOriginalColorType") == "Rgb8",
        f"qoi: a three channel file is labelled Rgb8 (got {clip.get_frame(0).props.get('ImgSeqOriginalColorType')})",
    )
    # The ramp: each column pair is one step, so red falls 255, 255, 191, 191
    # and blue rises 0, 0, 64, 64; green steps by 40 a row, which is why the
    # second row starts at 40. The first four samples of each plane are
    # enough to show they are in r,g,b order and not rotated.
    check(samples(color, 0)[:4] == [255, 255, 191, 191], "qoi: red plane")
    check(samples(color, 1)[:4] == [0, 0, 0, 0], "qoi: green plane")
    check(samples(color, 2)[:4] == [0, 0, 64, 64], "qoi: blue plane")
    check(samples(color, 0)[8:10] == [255, 255], "qoi: and the ramp restarts")
    check(samples(color, 1)[8:9] == [40], "qoi: one green step down")
    check(samples(frame) == [255] * 48, "qoi: no alpha stated means an opaque clip")

    # Four channels with alpha that varies, so the alpha clip is a picture.
    clip, alpha = read_alpha(files=[fixture("qoi-rgba8.qoi")])
    color = clip.get_frame(0)
    frame = alpha.get_frame(0)
    check(
        clip.get_frame(0).props.get("ImgSeqOriginalColorType") == "Rgba8",
        f"qoi: a four channel file is labelled Rgba8 (got {clip.get_frame(0).props.get('ImgSeqOriginalColorType')})",
    )
    check(samples(color, 0)[:4] == [255, 255, 191, 191], "qoi: the same red plane")
    check(samples(frame)[:4] == [40, 44, 48, 52], "qoi: the alpha plane is the file's")

    # The colours flag is informative. This file sets it and holds the same
    # picture, so every plane and every property has to match the plain one:
    # a flag that reached a _Transfer property would show up here.
    plain, plain_alpha = read_alpha(files=[fixture("qoi-rgb8.qoi")])
    linear, linear_alpha = read_alpha(files=[fixture("qoi-linear.qoi")])
    for plane in range(3):
        check(
            samples(linear.get_frame(0), plane) == samples(plain.get_frame(0), plane),
            f"qoi: the colours flag changes no sample, plane {plane}",
        )
    check(
        samples(linear_alpha.get_frame(0)) == samples(plain_alpha.get_frame(0)),
        "qoi: nor the alpha plane",
    )
    check(
        (
            linear.get_frame(0).props.get("ImgSeqOriginalColorType")
            == plain.get_frame(0).props.get("ImgSeqOriginalColorType")
        ),
        "qoi: nor the source label",
    )
    for name in ("_Transfer", "_Primaries", "_Matrix"):
        check(
            linear.get_frame(0).props.get(name) == plain.get_frame(0).props.get(name),
            f"qoi: the colours flag states no {name}",
        )


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
        clip, alpha = read_alpha(files=[fixture("alpha-rgba16.png")], prefetch=workers)
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
        [maximum // 4 + 17 * x + 31 * y for y in range(MONO_HEIGHT) for x in range(MONO_WIDTH)],
        [maximum - 13 * x - 29 * y for y in range(MONO_HEIGHT) for x in range(MONO_WIDTH)],
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
    alpha_plane = [240 - 9 * x - 13 * y for y in range(MONO_HEIGHT) for x in range(MONO_WIDTH)]
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
            f"yuv {name}: the clip is {width}x{height} (got {frame.width}x{frame.height})",
        )
        check(
            len(samples(frame, 1)) == (width // step_x) * (height // step_y),
            f"yuv {name}: the chroma plane is its own size",
        )
        check(
            frame.props.get("_Matrix") == 6,
            f"yuv {name}: the bt.601 matrix the file states (got {frame.props.get('_Matrix')})",
        )
        check(
            frame.props.get("_Range") == 1,
            f"yuv {name}: the full range the file states (got {frame.props.get('_Range')})",
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
            0,
            0,
            0,
            0,
            72,
            72,
            72,
            72,
            144,
            144,
            144,
            144,
            216,
            216,
            216,
            216,
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
                f"no picture: a frame request at prefetch={prefetch} ends within {NO_PICTURE_TIMEOUT}s",
            )
            continue
        output = f"{result.stdout.strip()} {result.stderr.strip()}".strip()
        check(
            result.returncode == 0 and "holds no picture" in output,
            f"no picture: a frame request at prefetch={prefetch} fails instead of hanging ({output})",
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
            f"extents: plane {plane} is the one the whole file holds ({len(samples(frame, plane))} samples)",
        )
    check(
        [samples(frame, plane)[0] for plane in range(3)] == [74, 126, 128],
        "extents: the first pixel is the one the reference decoder reads "
        f"(got {[samples(frame, plane)[0] for plane in range(3)]})",
    )

    # Both files are one format now, so a sequence holding the two needs no
    # `mismatch` and is one frame per file.
    both = core.imgseqs.Read(files=[fixture("avif-split-extents.avif"), fixture("avif-yuv420p.avif")])
    check(
        both.num_frames == 2,
        f"extents: the two are one format, so a sequence holds both (got {both.num_frames} frames)",
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
        f"demand: a colour-only read of a file with a broken alpha item is yuv (got {clip.format.name})",
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
        (width // 4 + column * width // 2, height // 4 + row * height // 2) for row in range(2) for column in range(2)
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
            f"exif webp {orientation}: the clip is {width}x{height} (got {clip.width}x{clip.height})",
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
                    (observed[first] < observed[second]) == (expected[first] < expected[second]),
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
        f"jxl: ImgSeqOrientation is the code the file states (got {frame.props.get('ImgSeqOrientation')})",
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
    together = core.imgseqs.Read(files=[fixture("orientation-6.png"), fixture("orientation-6.jxl")])
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
            f"colour {name}: a file with no icc profile reports none (got {props.get('ImgSeqHasICC')})",
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
        f"colour webp: the assumed matrix and range (got {webp.props.get('_Matrix')} and {webp.props.get('_Range')})",
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

    # The profile can arrive in a webp as well, in an `ICCP` chunk, and no
    # committed webp fixture held one until these. All three shapes are checked
    # because the profile is a chunk a bitstream walk has to *find* rather than a
    # number it can read where it stands, which makes it the part of a webp probe
    # most likely to be missed. The alpha one is here for its `ALPH` chunk: a
    # fully opaque alpha channel is dropped by the encoder, so a fixture named for
    # one can easily be a file without it.
    for name in ["webp-icc.webp", "webp-icc-lossless.webp", "webp-icc-alpha.webp"]:
        frame = core.imgseqs.Read(files=[fixture(name)], icc_profile=True).get_frame(0)
        check(frame.props.get("ImgSeqHasICC") == 1, f"icc: {name} reports its profile")
        check(
            frame.props.get("ICCProfile") == expected,
            f"icc: {name} keeps the exact profile bytes",
        )

    plain = read_alpha(files=[fixture("icc-rgba8.png")])
    result = core.imgseqs.ReadAlpha(
        files=[fixture("icc-rgba8.png")],
        icc_profile=True,
    )
    color = result["clip"].get_frame(0)
    alpha = result["alpha"].get_frame(0)
    check(
        color.props.get("ICCProfile") == expected and alpha.props.get("ICCProfile") == expected,
        "icc: ReadAlpha copies the profile to both frames",
    )
    check(
        samples(color) == samples(plain[0].get_frame(0)) and samples(alpha) == samples(plain[1].get_frame(0)),
        "icc: ReadAlpha export does not change either clip",
    )


# The container-transform fixtures state their orientation as the `irot` and
# `imir` item properties of the image item rather than as an exif tag, so the
# code beside each one is the exif code those two describe. The source they are
# encoded from is the same four by three grid of twelve distinct samples as the
# exif fixtures, so the picture each code produces is the one `shown` states.
CONTAINER_STORED = [
    [row * ORIENTATION_WIDTH + column + 7 for column in range(ORIENTATION_WIDTH)] for row in range(ORIENTATION_HEIGHT)
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
            f"container {name}: the clip is {width}x{height} (got {clip.width}x{clip.height})",
        )
        frame = clip.get_frame(0)
        check(
            samples(frame) == [sample for row in picture for sample in row],
            f"container {name}: the picture the container states",
        )
        check(
            frame.props["ImgSeqOrientation"] == code,
            f"container {name}: ImgSeqOrientation is the code it describes (got {frame.props['ImgSeqOrientation']})",
        )

        unrotated = core.imgseqs.Read(files=[fixture(name)], prefetch=0, apply_rotation=False)
        check(
            (unrotated.width, unrotated.height) == (len(stored[0]), len(stored)),
            f"container {name}: apply_rotation=False keeps the stored size (got {unrotated.width}x{unrotated.height})",
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
            f"container: a rotated page beside an upright one is rejected ({str(error).strip()})",
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
        check(clip.num_frames == 15, f"animation: {name} is 15 frames at 24 fps (got {clip.num_frames})")
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

    # A track that states composition offsets presents its samples where the
    # `ctts` box says rather than where they were decoded. The fixture is
    # `animation.avif` with a version one box inserted by
    # `tests/make-animation-fixtures.py`: the first sample is composed 40 ms
    # before zero, which is clamped rather than wrapped, and the second 100 ms
    # after its decode time, so the four holds become 180, 70, 110 and 240 ms
    # and the pictures land on output frames 0, 5, 6 and 9.
    clip = core.imgseqs.Read(files=[fixture("animation-ctts.avif")])
    check(
        clip.num_frames == 15,
        f"animation ctts: the clip is 15 frames at 24 fps (got {clip.num_frames})",
    )
    starts = [0, 5, 6, 9]
    hashes = [frame_hash(clip.get_frame(index)) for index in range(clip.num_frames)]
    ends = [*starts[1:], clip.num_frames]
    for index, (start, end) in enumerate(zip(starts, ends)):
        check(
            len({hashes[frame] for frame in range(start, end)}) == 1,
            f"animation ctts: holds presentation {index} across frames {start}..{end}",
        )
    # The same four pictures as the avif it was cut from, at the instants the
    # box states: only the timing moved.
    plain = core.imgseqs.Read(files=[fixture("animation.avif")])
    check(
        {hashes[start] for start in starts}
        == {frame_hash(plain.get_frame(index)) for index in [0, 2, 6, 9]},
        "animation ctts: the four pictures are the ones the avif holds",
    )
    slower = core.imgseqs.Read(files=[fixture("animation-ctts.avif")], fpsnum=10, fpsden=1)
    check(
        slower.num_frames == 6,
        f"animation ctts: the clip is 6 frames at 10 fps (got {slower.num_frames})",
    )

    # An edit list is where a file says which part of its media it displays. The
    # short one ends the track at 300 of its 600 ms, so the clip is eight frames
    # at 24 fps and the last picture is held only to the end; the skip one starts
    # 250 ms into its media, which this reader refuses rather than playing the
    # pictures the file says are not shown.
    clip = core.imgseqs.Read(files=[fixture("animation-elst-short.avif")])
    check(
        clip.num_frames == 8,
        f"animation elst: the truncated clip is 8 frames at 24 fps (got {clip.num_frames})",
    )
    hashes = [frame_hash(clip.get_frame(index)) for index in range(clip.num_frames)]
    for index, (start, end) in enumerate(zip([0, 2, 6], [2, 6, 8])):
        check(
            len({hashes[frame] for frame in range(start, end)}) == 1,
            f"animation elst: holds presentation {index} across frames {start}..{end}",
        )
    check(
        len({hashes[start] for start in [0, 2, 6]}) == 3,
        "animation elst: the three pictures the edit shows are distinct",
    )
    slower = core.imgseqs.Read(files=[fixture("animation-elst-short.avif")], fpsnum=10, fpsden=1)
    check(
        slower.num_frames == 3,
        f"animation elst: the truncated clip is 3 frames at 10 fps (got {slower.num_frames})",
    )
    try:
        core.imgseqs.Read(files=[fixture("animation-elst-skip.avif")])
    except Exception as error:
        check(
            "starts its media" in str(error),
            f"animation elst: a leading skip is refused by name ({str(error).strip()})",
        )
    else:
        check(False, "animation elst: a leading skip is refused")

    # A fragmented file's movie box states no samples -- they are in the fragments
    # that follow it -- and libheif has no fragment parsing at all, so it refuses
    # such a file: with an empty `stsz` it reports the `stsc` count against it, and
    # with an empty `stsc` it reports the box itself. This reader takes its timeline
    # from the same tables, so there is nothing here to promise either, and the file
    # is refused rather than described as a sequence nothing can decode.
    try:
        core.imgseqs.Read(files=[fixture("animation-fragmented.avif")])
    except Exception as error:
        check(
            "no reader here knows its format" in str(error),
            f"animation fragmented: a movie with no samples is refused ({str(error).strip()})",
        )
    else:
        check(False, "animation fragmented: a movie with no samples is refused")
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
        rgba16["clip"].num_frames == 8,
        f"animation: a 16-bit APNG is 8 frames (got {rgba16['clip'].num_frames})",
    )
    sixteen = [frame_hash(rgba16["clip"].get_frame(index)) for index in range(8)]
    check(
        len(set(sixteen[:3])) == 1 and len(set(sixteen[3:])) == 1 and sixteen[0] != sixteen[3],
        "animation: a 16-bit APNG switches picture at the third frame",
    )

    # Alpha varies between the presentations of an animation.
    animated = core.imgseqs.ReadAlpha(files=[fixture("animation.png")])
    colour, alpha = animated["clip"], animated["alpha"]
    check(colour.num_frames == alpha.num_frames, "animation: both clips have the same length")
    alpha_hashes = {frame_hash(alpha.get_frame(index)) for index in range(alpha.num_frames)}
    check(len(alpha_hashes) > 1, "animation: the alpha clip varies between presentations")

    # A list that mixes a still with an animation adds up, and the still keeps
    # its one frame at the front.
    mixed = core.imgseqs.Read(files=[fixture("animation.gif"), fixture("animation.gif")])
    check(
        mixed.num_frames == 30,
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
    variable = core.imgseqs.Read(files=[fixture("animation.gif"), fixture("alpha-rgb8.png")], mismatch=1)
    check(
        variable.num_frames == 16,
        f"animation: mismatch allows a still after an animation (got {variable.num_frames})",
    )


def test_gif_stills() -> None:
    """A gif is read here now, and it is the *logical screen* that is handed out.

    A gif is a sequence of sub-rectangles drawn onto a screen, so a reader that
    returns the one frame as the picture gets two things wrong for a file whose
    frame is smaller than its screen: the size, and every sample the rectangle
    does not cover. The fixtures are a full canvas frame, an 8x8 screen holding
    a 3x2 frame at (2,1), and a full canvas frame with one index transparent.

    `RGB24` is three *planes* here rather than interleaved bytes, so a sample is
    read a plane at a time: plane 0 is red, 1 is green and 2 is blue.
    """
    clip, alpha = read_alpha(files=[fixture("gif-still-subrect.gif")])
    frame = clip.get_frame(0)
    check(
        (frame.width, frame.height) == (8, 8),
        f"a gif is handed out at its logical screen (got {frame.width}x{frame.height})",
    )
    check(frame.format.name == "RGB24", f"and as three channels (got {frame.format.name})")
    check(frame.format.num_planes == 3, "which are three planes")
    check(alpha.get_frame(0).format.name == "Gray8", "with a gray alpha clip")

    # The palette is red, green, blue, yellow. The frame's indices are
    # 0,1,0 / 1,0,1 drawn at (2,1), so red lands on (2,1), (4,1) and (3,2) and
    # green on (3,1), (2,2) and (4,2) -- and nothing in the frame is blue.
    red = samples(frame, 0)
    green = samples(frame, 1)
    blue = samples(frame, 2)

    def at(x: int, y: int) -> int:
        return y * 8 + x

    for x, y in [(2, 1), (4, 1), (3, 2)]:
        check(red[at(x, y)] == 255, f"the sample at ({x}, {y}) is red (got {red[at(x, y)]})")
        check(green[at(x, y)] == 0, f"and carries no green at ({x}, {y})")
    for x, y in [(3, 1), (2, 2), (4, 2)]:
        check(
            green[at(x, y)] == 255,
            f"the sample at ({x}, {y}) is green (got {green[at(x, y)]})",
        )
        check(red[at(x, y)] == 0, f"and carries no red at ({x}, {y})")
    check(all(value == 0 for value in blue), "and nothing in the frame is blue")

    # The samples the rectangle does not cover are untouched, and the alpha clip
    # says so: transparent rather than the background colour the file names.
    for x, y in [(0, 0), (1, 1), (5, 1), (7, 7)]:
        check(red[at(x, y)] == 0, f"the screen outside the frame is untouched at ({x}, {y})")
    coverage = samples(alpha.get_frame(0))
    check(coverage[at(0, 0)] == 0, "and so is the sample at (0, 0)")
    check(coverage[at(1, 1)] == 0, "and the one just outside the rectangle")
    check(coverage[at(2, 1)] == 255, "while the frame itself is opaque")
    check(coverage[at(4, 2)] == 255, "all the way to its last sample")

    # A transparent index keeps its palette colour in the colour clip. Index 0 is
    # red, so the two full canvas fixtures agree plane for plane and differ only
    # in the alpha clip. That is what the reader being replaced produced, and it is
    # why the still path *places* the frame instead of compositing it: the
    # animation compositor would replace a transparent sample with what is
    # underneath, which for the first frame is nothing at all.
    opaque = read_alpha(files=[fixture("gif-still.gif")])
    see_through = read_alpha(files=[fixture("gif-still-alpha.gif")])
    full = opaque[0].get_frame(0)
    check((full.width, full.height) == (4, 4), "a full canvas frame is the screen size")
    for plane in range(3):
        check(
            samples(see_through[0].get_frame(0), plane) == samples(full, plane),
            f"a transparent index leaves plane {plane} of the colour clip alone",
        )
    check(
        samples(see_through[1].get_frame(0)) != samples(opaque[1].get_frame(0)),
        "and shows in the alpha clip instead",
    )
    check(samples(see_through[1].get_frame(0))[0] == 0, "index 0 is the transparent one")
    check(samples(opaque[1].get_frame(0))[0] == 255, "and is opaque without it")


def test_png_feature_sets() -> None:
    """Interlacing, a palette and a narrow depth, none of which had a fixture.

    Adam7 stores the samples in a different order and the reader has to put that
    back together; a palette page arrives as indices which the walk widens; and a
    one, two or four bit grey page arrives narrower than a byte. Each interlaced
    fixture has a non-interlaced twin holding the same picture, so the check that
    matters is that the two agree -- a reader that dropped an Adam7 pass would
    still decode *something*, and only the comparison says it decoded the page.
    """
    for interlaced, plain in [
        ("png-interlaced-rgb8.png", "png-plain-rgb8.png"),
        ("png-interlaced-gray8.png", "png-plain-gray8.png"),
    ]:
        from_interlaced = read_alpha(files=[fixture(interlaced)])[0].get_frame(0)
        from_plain = read_alpha(files=[fixture(plain)])[0].get_frame(0)
        check(
            (from_interlaced.width, from_interlaced.height) == (37, 23),
            f"{interlaced} is the size its header states",
        )
        check(
            from_interlaced.format.name == from_plain.format.name,
            f"{interlaced} is handed out as {from_plain.format.name}",
        )
        for plane in range(from_interlaced.format.num_planes):
            check(
                samples(from_interlaced, plane) == samples(from_plain, plane),
                f"{interlaced} plane {plane} is {plain}",
            )

    # A palette page is handed out as the r,g,b its indices name, at either table
    # width: four bits is the walk's own expansion and eight is the decoder's.
    for name in ["png-palette8.png", "png-palette256.png"]:
        frame = read_alpha(files=[fixture(name)])[0].get_frame(0)
        check(frame.format.name == "RGB24", f"{name} widens its indices to r,g,b")
        check(
            str(frame.props["ImgSeqOriginalColorType"]) == "Rgb8",
            f"{name} names the layout its indices widen to",
        )

    # A narrow grey page is widened to eight bits, whatever the header states.
    for name, depth in [("png-gray1.png", 1), ("png-gray2.png", 2), ("png-gray4.png", 4)]:
        frame = read_alpha(files=[fixture(name)])[0].get_frame(0)
        check(
            frame.format.name == "Gray8",
            f"{name}, at {depth} bits, is handed out as eight",
        )
        check(
            str(frame.props["ImgSeqOriginalColorType"]) == "L8",
            f"{name} names the grey it widens to",
        )


def test_separated_inks() -> None:
    """A cmyk tiff is the colour its four inks state, converted by this reader.

    The inks are the ones `make-tiff-cmyk-fixtures.py` writes and the expected
    samples are `(255 - ink) * (255 - k) / 255` worked out by hand. The fifth
    pixel is the one worth reading twice: its `k` is 128 and every other ink is
    zero, so the colour is exactly 127, and the plugin answers 126, because the
    factor is computed in `f32` and lands a hair below the value that truncates
    to 127. That is the arithmetic the reader this tree replaced used, so it is
    the sample such a file has always produced, and it is precisely what an
    implementation doing exact arithmetic would get wrong.
    """
    clip, alpha = read_alpha(files=[fixture("tiff-cmyk8.tiff")])
    check(clip.format.name == "RGB24", f"cmyk8: colour clip is RGB24 (got {clip.format.name})")
    check(
        (clip.width, clip.height) == (4, 2),
        f"cmyk8: size is 4x2 (got {clip.width}x{clip.height})",
    )
    check(alpha.format.name == "Gray8", f"cmyk8: alpha clip is Gray8 (got {alpha.format.name})")
    frame = clip.get_frame(0)
    check(
        samples(frame, 0) == [255, 0, 0, 127, 126, 119, 0, 206],
        f"cmyk8: red is the ink it states (got {samples(frame, 0)})",
    )
    check(
        samples(frame, 1) == [255, 0, 0, 255, 126, 179, 255, 198],
        f"cmyk8: green is the ink it states (got {samples(frame, 1)})",
    )
    check(
        samples(frame, 2) == [255, 0, 0, 255, 126, 209, 255, 189],
        f"cmyk8: blue is the ink it states (got {samples(frame, 2)})",
    )
    label = frame.props["ImgSeqOriginalColorType"]
    check(label == "Cmyk8", f"cmyk8: the label names the ink model (got {label})")
    check(
        samples(alpha.get_frame(0)) == [255] * 8,
        "cmyk8: four inks and no alpha is an opaque clip",
    )

    # The same inks at sixteen bits, so the conversion scales with the sample
    # rather than reusing the byte one. The fifth pixel is the same truncation
    # one word up: 32638 where the exact product is 32639.
    wide, _ = read_alpha(files=[fixture("tiff-cmyk16.tiff")])
    check(wide.format.name == "RGB48", f"cmyk16: colour clip is RGB48 (got {wide.format.name})")
    wframe = wide.get_frame(0)
    check(
        samples(wframe, 0) == [65535, 0, 0, 32639, 32638, 30591, 0, 53088],
        f"cmyk16: red is the ink it states (got {samples(wframe, 0)})",
    )
    check(
        samples(wframe, 2) == [65535, 0, 0, 65535, 32638, 53715, 65535, 48754],
        f"cmyk16: blue is the ink it states (got {samples(wframe, 2)})",
    )
    wide_label = wframe.props["ImgSeqOriginalColorType"]
    check(
        wide_label == "Cmyk16",
        f"cmyk16: the label names the ink model (got {wide_label})",
    )

    # A planar file is the same picture as the chunky one, which is what says
    # the four links were reordered: every sample is present either way, so only
    # their order tells the two apart.
    planar, _ = read_alpha(files=[fixture("tiff-cmyk8-planar.tiff")])
    planar_frame = planar.get_frame(0)
    for plane in range(3):
        check(
            samples(planar_frame, plane) == samples(frame, plane),
            f"cmyk8 planar: plane {plane} is the chunky file's",
        )

    # A fifth sample is alpha and not a fifth ink: the colour of every pixel is
    # the four ink file's, so the sample never reached the conversion.
    five, five_alpha = read_alpha(files=[fixture("tiff-cmyka8.tiff")])
    five_frame = five.get_frame(0)
    for plane in range(3):
        check(
            samples(five_frame, plane) == samples(frame, plane),
            f"cmyka8: plane {plane} is the four ink file's",
        )
    check(
        samples(five_alpha.get_frame(0)) == [0, 255, 128, 1, 254, 64, 192, 32],
        f"cmyka8: the fifth sample is the alpha clip (got {samples(five_alpha.get_frame(0))})",
    )


def test_planar_pages() -> None:
    """A separate-planar page is the chunky page, read without splitting its buffer.

    The two spellings of one picture are stored differently -- one interleaved
    buffer, one plane per channel laid end to end -- and the plugin hands the
    second to the frame in the shape the decoder produced it in, so the whole of
    every plane is compared here rather than a few samples of it. Every sample is
    present either way, so only the order tells the two apart, and a picture of
    the right size with the wrong colours passes a spot check.
    """
    planar, planar_alpha = read_alpha(files=[fixture("tiff-planar.tiff")])
    chunky, chunky_alpha = read_alpha(files=[fixture("tiff-rgb8.tiff")])
    planar_frame = planar.get_frame(0)
    chunky_frame = chunky.get_frame(0)
    check(
        (planar_frame.width, planar_frame.height) == (chunky_frame.width, chunky_frame.height),
        f"planar: the picture is the chunky file's size (got {planar_frame.width}x{planar_frame.height})",
    )
    for plane in range(3):
        check(
            samples(planar_frame, plane) == samples(chunky_frame, plane),
            f"planar rgb8: plane {plane} is the chunky file's",
        )
    check(
        samples(planar_alpha.get_frame(0)) == samples(chunky_alpha.get_frame(0)),
        "planar rgb8: the alpha clip is the chunky file's",
    )


def test_core_header_bitmaps() -> None:
    """A `BITMAPCOREHEADER` bitmap is read, which it was refused for by name.

    The older header is the same format with three differences that all show up
    here: signed sixteen bit dimensions, no compression field at all, and a
    palette entry of three bytes where the information header's is four. The 4
    bit file is the one that fails first if the table is stepped four bytes at a
    time, so it is here as well as the 1x1 24 bit file the gaps table names.

    The expected samples are what Pillow reads from the same two files, which is
    the only independent answer for a format this tree wrote itself.
    """
    cases = [
        ("bmp-core-24.bmp", (1, 1), [3, 5, 7]),
        (
            "bmp-core-4.bmp",
            (2, 2),
            # Top row first, which is the order the picture is stored in.
            [
                200,
                200,
                200,
                90,
                0,
                0,
                0,
                60,
                0,
                0,
                0,
                30,
            ],
        ),
    ]
    for name, size, expected in cases:
        clip, alpha = read_alpha(files=[fixture(name)])
        check(clip.format.name == "RGB24", f"{name}: color clip is RGB24 (got {clip.format.name})")
        check(alpha.format.name == "Gray8", f"{name}: alpha clip is Gray8 (got {alpha.format.name})")
        frame = clip.get_frame(0)
        check(
            (frame.width, frame.height) == size,
            f"{name}: the size is the one the core header states (got {frame.width}x{frame.height})",
        )
        for plane in range(3):
            check(
                samples(frame, plane) == expected[plane::3],
                f"{name}: plane {plane} is the colour Pillow reads (got {samples(frame, plane)})",
            )
        check(
            set(samples(alpha.get_frame(0))) == {255},
            f"{name}: a core header states no alpha, so the clip is opaque",
        )


def test_palette_pages() -> None:
    """A palette page is expanded through its colour table.

    The indices are read from the strips by the plugin rather than by the pinned
    decoder, which refuses this photometric before it can describe a chunk, so
    the bit order and the row padding are this tree's own: a row of indices is
    padded to a byte boundary and an index narrower than a byte sits in the bits
    above the one before it. Each width is checked against the colours libtiff
    hands Pillow for the same file at the same positions, at (0, 0), (1, 5) and
    (18, 11), which is what says the expansion is the one the format means.

    A palette reports the layout its samples expand to rather than naming its
    indices, which is what a palette png already reports here, and the four
    widths are the ones a byte divides into.
    """
    expected = {
        "tiff-palette-1bit.tiff": [(197, 0, 58), (197, 0, 58), (64, 0, 191)],
        "tiff-palette-2bit.tiff": [(197, 0, 58), (197, 0, 58), (128, 0, 128)],
        "tiff-palette.tiff": [(244, 0, 11), (209, 0, 46), (110, 0, 145)],
        "tiff-palette-8bit.tiff": [(255, 0, 0), (197, 0, 58), (128, 0, 128)],
    }
    spots = [(0, 0), (1, 5), (18, 11)]
    for name, colours in expected.items():
        clip, alpha = read_alpha(files=[fixture(name)])
        check(
            clip.format.name == "RGB24",
            f"{name}: the colour clip is RGB24 (got {clip.format.name})",
        )
        check(
            (clip.width, clip.height) == (37, 23),
            f"{name}: the size is the one the header states (got {clip.width}x{clip.height})",
        )
        frame = clip.get_frame(0)
        plains = [samples(frame, plane) for plane in range(3)]
        for (x, y), colour in zip(spots, colours, strict=True):
            at = y * 37 + x
            check(
                tuple(plane[at] for plane in plains) == colour,
                f"{name}: ({x}, {y}) is {colour} (got {tuple(plane[at] for plane in plains)})",
            )
        label = frame.props["ImgSeqOriginalColorType"]
        check(
            label == "Rgb8",
            f"{name}: the label names the layout the samples expand to (got {label})",
        )
        check(
            samples(alpha.get_frame(0)) == [255] * 37 * 23,
            f"{name}: a palette has no alpha, so the clip is opaque",
        )

    # A width a byte does not divide into is refused by the probe rather than
    # promised by it, which is what says the refusal is not only in the decode.
    try:
        read_alpha(files=[fixture("tiff-palette-3bit.tiff")])
        check(False, "palette: three bits an index is refused")
    except Exception as error:
        check(
            "3 bit" in str(error),
            f"palette: the refusal names the width (got {error})",
        )


def test_narrow_gray_pages() -> None:
    """A gray page of one, two or four bits a sample is widened to eight.

    The pinned decoder hands these over packed, so the walk that unpacks them
    is this tree's own: a row of samples is padded to a byte and held most
    significant bit first, and each sample is widened by
    `value * 255 / (2**bits - 1)`, which is libtiff's own widening. That is
    what puts one bit at 0 and 255 rather than at 0 and 1, and what leaves a
    bilevel page black and white rather than nearly black.

    The samples are what Pillow reads from the same files at the first
    sample, the first sample of the fifth byte, the last sample of the first
    row, and the first sample of the second row -- the last of which is what
    says the walk stepped by the row stride rather than reading the first row
    for every row, which is the one way this walk can be wrong without
    changing the row it is checked on.

    `tiff-gray1-lzw.tiff` is the bilevel page round tripped through libtiff
    with LZW selected, so the compressed path is the same picture.
    """
    expected = {
        "tiff-gray1.tiff": (255, 0, 0, 255),
        "tiff-gray1-lzw.tiff": (255, 0, 0, 255),
        "tiff-gray2.tiff": (0, 0, 255, 85),
        "tiff-gray4.tiff": (0, 136, 255, 17),
    }
    for name, (first, at_eight, last, second_row) in expected.items():
        clip, alpha = read_alpha(files=[fixture(name)])
        check(
            clip.format.name == "Gray8",
            f"{name}: a narrow gray page is handed out at eight bits (got {clip.format.name})",
        )
        check(
            (clip.width, clip.height) == (16, 8),
            f"{name}: the size is the one the header states (got {clip.width}x{clip.height})",
        )
        frame = clip.get_frame(0)
        plane = samples(frame)
        for index, want in ((0, first), (8, at_eight), (15, last), (16, second_row)):
            check(
                plane[index] == want,
                f"{name}: sample {index} is {want} (got {plane[index]})",
            )
        check(
            str(frame.props["ImgSeqOriginalColorType"]) == "L8",
            f"{name}: the label is the width the samples arrive at (got {frame.props['ImgSeqOriginalColorType']})",
        )
        check(
            samples(alpha.get_frame(0)) == [255] * 16 * 8,
            f"{name}: a gray page has no alpha, so the clip is opaque",
        )

    # The two bilevel fixtures differ in `PhotometricInterpretation` and in
    # nothing else, and the pinned decoder inverts the page that says zero is
    # white itself -- so a reader that expanded the samples and then forgot
    # the tag would hand out its twin's picture, which is the one way this
    # tag can be lost without any other check in this file noticing.
    black, _ = read_alpha(files=[fixture("tiff-gray1.tiff")])
    white, _ = read_alpha(files=[fixture("tiff-gray1-white.tiff")])
    black_samples = samples(black.get_frame(0))
    white_samples = samples(white.get_frame(0))
    check(
        black_samples != white_samples,
        "gray1: a page that says zero is white is not its twin's picture",
    )
    check(
        all(a == 255 - b for a, b in zip(black_samples, white_samples, strict=True)),
        "gray1: a page that says zero is white is the inverse of its twin",
    )

    # A sub-byte page carrying a predictor the reader cannot apply is refused
    # by the probe, naming the tag, rather than described and then refused at
    # the decode -- which is the shape `probe-agreement.py` exists to check.
    try:
        read_alpha(files=[fixture("tiff-gray1-pred.tiff")])
        check(False, "gray1: a sub-byte page with a predictor is refused")
    except Exception as error:
        check(
            "predictor" in str(error),
            f"gray1: the refusal names the tag (got {error})",
        )


def test_ycbcr_pages() -> None:
    """A ycbcr page is handed out as its own planes at the sampling it states.

    The three fixtures hold the same luma, stepping by eight across and
    thirty-two down, and a flat chroma of 128, so a unit read in the wrong
    order, a unit row read at the wrong stride or a chroma plane of the wrong
    size shows up as a sample the file does not state rather than as a picture
    that merely looks off. libtiff's own tiff2rgba reads the same three to
    neutral greys at exactly these levels, which is what says the packing is
    the format's and not this reader's.

    The coefficients and the reference levels decide `_Matrix` and `_Range`, and
    a page that states neither is bt.601 at full range, which is what the
    specification gives it: an absent tag is a value rather than an unknown.
    """
    luma = [16, 24, 32, 40, 48, 56, 64, 72]
    for name, format_name, size, chroma in [
        ("tiff-ycbcr-444.tiff", "YUV444P8", (37, 23), (37, 23)),
        ("tiff-ycbcr-422.tiff", "YUV422P8", (36, 22), (18, 22)),
        ("tiff-ycbcr-420.tiff", "YUV420P8", (36, 22), (18, 11)),
    ]:
        clip, alpha = read_alpha(files=[fixture(name)])
        check(
            clip.format.name == format_name,
            f"{name}: the sampling is handed out (got {clip.format.name})",
        )
        check(
            (clip.width, clip.height) == size,
            f"{name}: the size is the one the header states",
        )
        frame = clip.get_frame(0)
        luma_samples = samples(frame, 0)
        check(
            len(luma_samples) == size[0] * size[1],
            f"{name}: the luma plane is the size of the picture (got {len(luma_samples)})",
        )
        check(
            luma_samples[:8] == luma,
            f"{name}: the luma is the one written (got {luma_samples[:8]})",
        )
        check(
            luma_samples[clip.width] == 48,
            f"{name}: the second row starts a step down (got {luma_samples[clip.width]})",
        )
        for plane in (1, 2):
            chroma_samples = samples(frame, plane)
            check(
                len(chroma_samples) == chroma[0] * chroma[1],
                f"{name}: chroma plane {plane} is the subsampled size (got {len(chroma_samples)})",
            )
            check(
                set(chroma_samples) == {128},
                f"{name}: chroma plane {plane} is flat, so the file states no colour",
            )
        check(frame.props["_Matrix"] == 5, f"{name}: the matrix is bt.601")
        check(frame.props["_Range"] == 1, f"{name}: the range is full")
        check(
            samples(alpha.get_frame(0)) == [255] * (size[0] * size[1]),
            f"{name}: a ycbcr page has no alpha, so the clip is opaque",
        )

    # A page whose coefficients name another matrix, and one whose reference
    # levels leave headroom, which is the only thing that moves these two.
    for name, matrix, full_range in [
        ("tiff-ycbcr-709.tiff", 1, True),
        ("tiff-ycbcr-stated.tiff", 5, True),
        ("tiff-ycbcr-limited.tiff", 5, False),
    ]:
        frame = read_alpha(files=[fixture(name)])[0].get_frame(0)
        check(
            frame.props["_Matrix"] == matrix,
            f"{name}: _Matrix is {matrix} (got {frame.props['_Matrix']})",
        )
        check(
            frame.props["_Range"] == (1 if full_range else 0),
            f"{name}: _Range is {full_range} (got {frame.props['_Range']})",
        )

    # A page whose coefficients name no matrix VapourSynth can name is converted
    # to rgb with those coefficients rather than handed out as planes nothing can
    # label. libtiff's own tiff2rgba gives these channels for the same file, so
    # the arithmetic is checked against a reader rather than against itself.
    clip, _ = read_alpha(files=[fixture("tiff-ycbcr-rgb.tiff")])
    check(
        clip.format.name == "RGB24",
        f"ycbcr: a page whose coefficients cannot be named is rgb (got {clip.format.name})",
    )
    frame = clip.get_frame(0)
    plains = [samples(frame, plane) for plane in range(3)]
    for index, colour in enumerate([(0, 184, 0), (0, 150, 0), (0, 116, 32), (0, 82, 96)]):
        check(
            tuple(plane[index] for plane in plains) == colour,
            f"ycbcr: rgb pixel {index} is {colour} (got {tuple(plane[index] for plane in plains)})",
        )
    check(frame.props["_Matrix"] == 0, "ycbcr: an rgb frame states no matrix")
    check(frame.props["_Range"] == 1, "ycbcr: an rgb frame is full range")

    # Every page this reader cannot take is refused by the *probe*, so the
    # probe never promises a frame the decode would not produce.
    for name, said in [
        ("tiff-ycbcr-16bit.tiff", "16"),
        ("tiff-ycbcr-lzw.tiff", "compressed"),
    ]:
        try:
            read_alpha(files=[fixture(name)])
            check(False, f"ycbcr: {name} is refused")
        except Exception as error:
            check(
                said in str(error),
                f"ycbcr: {name} names why (got {error})",
            )


def test_bare_dib() -> None:
    """A bare DIB is read, which the `BM` check used to decline.

    A bare DIB is a bitmap with no file header at all: the file starts at the DIB
    header, which is the shape a `.dib` holds and the shape an icon's directory
    entry points at. The pair is the acceptance -- the fourteen bytes of file
    header are the only difference between the two files -- and the palette one
    fails first if an offset still counts those fourteen bytes. The last file is
    the negative case: the name is a claim a probe confirms, so a `.dib` that
    holds no DIB header is refused rather than read as a bitmap.
    """
    for dib, bmp in (
        ("dib-depth24.dib", "bmp-depth24.bmp"),
        ("dib-depth4.dib", "bmp-depth4.bmp"),
    ):
        bare_clip, bare_alpha = read_alpha(files=[fixture(dib)])
        whole_clip, whole_alpha = read_alpha(files=[fixture(bmp)])
        check(
            bare_clip.format.name == whole_clip.format.name,
            f"{dib}: the colour clip is the format {bmp} hands out"
            f" (got {bare_clip.format.name})",
        )
        check(
            (bare_clip.width, bare_clip.height)
            == (whole_clip.width, whole_clip.height),
            f"{dib}: the size is the one {bmp} states",
        )
        bare_frame = bare_clip.get_frame(0)
        whole_frame = whole_clip.get_frame(0)
        for plane in range(bare_clip.format.num_planes):
            check(
                samples(bare_frame, plane) == samples(whole_frame, plane),
                f"{dib}: plane {plane} is the one {bmp} hands out",
            )
        check(
            samples(bare_alpha.get_frame(0)) == samples(whole_alpha.get_frame(0)),
            f"{dib}: the alpha clip is the one {bmp} hands out",
        )
    try:
        core.imgseqs.ReadAlpha(files=[fixture("dib-not-a-dib.dib")])
    except Exception as error:
        check(True, f"a text file named .dib is refused ({str(error).strip()})")
    else:
        check(False, "a text file named .dib is refused")


def test_cursor() -> None:
    """A cursor is read, which its type word used to be mistaken for a depth.

    A cursor's directory is the icon's with two fields moved: where an icon states
    its colour planes and its bit depth, a cursor states the hot spot. The payload
    is the same one and the AND mask follows the icon's rule -- a set bit clears
    alpha and a clear one leaves what the payload stated -- so the alpha plane is
    the payload's own alpha multiplied by the mask rather than either one alone.
    ``ico-masked.ico`` and ``cur-masked.cur`` hold the same picture, so the pair
    is what says the two directories read alike.
    """
    icon_clip, icon_alpha = read_alpha(files=[fixture("ico-masked.ico")])
    cursor_clip, cursor_alpha = read_alpha(files=[fixture("cur-masked.cur")])
    check(
        cursor_clip.format.name == icon_clip.format.name,
        f"cursor: the colour clip is the format its icon twin hands out"
        f" (got {cursor_clip.format.name})",
    )
    check(
        (cursor_clip.width, cursor_clip.height)
        == (icon_clip.width, icon_clip.height),
        "cursor: the size is the one the directory states",
    )
    cursor_frame = cursor_clip.get_frame(0)
    icon_frame = icon_clip.get_frame(0)
    for plane in range(cursor_clip.format.num_planes):
        check(
            samples(cursor_frame, plane) == samples(icon_frame, plane),
            f"cursor: plane {plane} is the one its icon twin hands out",
        )
    # The mask clears the leftmost eight columns and the payload states 64 on the
    # left half and 255 on the right, so all three values have to be here.
    alpha = samples(cursor_alpha.get_frame(0))
    row = [
        0 if x < 8 else (255 if x >= cursor_clip.width // 2 else 64)
        for x in range(cursor_clip.width)
    ]
    check(
        alpha == row * cursor_clip.height,
        "cursor: the alpha plane is the payload's alpha times the mask"
        f" (got {sorted(set(alpha))})",
    )
    check(
        alpha == samples(icon_alpha.get_frame(0)),
        "cursor: the alpha clip is the one its icon twin hands out",
    )


def test_gray_alpha_pages() -> None:
    """A gray TIFF whose second sample is alpha is read, at three widths.

    The pinned decoder names such a page `Multiband` whatever its depth, so the
    reader takes the shape from the directory instead: two samples and an
    `ExtraSamples` value that names alpha. Every page has a one sample twin
    written by the same script, and Pillow reads the twins at every width -- a
    page of two samples it reads only at eight bits -- so the expected values
    below are Pillow's read of the twins, which the maker's `--check` prints.
    The gray sample runs along the row and the alpha one down the column, so a
    page whose samples were swapped, or read as one plane, is a wrong picture
    rather than a plausible one.
    """
    for name, top, format_name in (
        ("tiff-graya8.tiff", 255, "Gray8"),
        ("tiff-graya16.tiff", 65535, "Gray16"),
    ):
        clip, alpha_clip = read_alpha(files=[fixture(name)])
        check(
            clip.format.name == format_name,
            f"{name}: the colour clip is {format_name} (got {clip.format.name})",
        )
        check(
            clip.format.num_planes == 1,
            f"{name}: the colour clip is one gray plane (got {clip.format.num_planes})",
        )
        check(
            alpha_clip.format.name == format_name,
            f"{name}: the alpha clip is the same depth (got {alpha_clip.format.name})",
        )
        check(
            samples(clip.get_frame(0))
            == [x * top // 15 for _ in range(8) for x in range(16)],
            f"{name}: the colour plane is the first sample Pillow reads",
        )
        check(
            samples(alpha_clip.get_frame(0))
            == [y * top // 7 for y in range(8) for _ in range(16)],
            f"{name}: the alpha clip is the second sample Pillow reads",
        )
    # The float spelling and its associated twin: the two differ only in the
    # `ExtraSamples` tag, and the second sample is handed over as it is rather
    # than scaled by the first.
    for name in ("tiff-graya32f.tiff", "tiff-graya32f-associated.tiff"):
        clip, alpha_clip = read_alpha(files=[fixture(name)])
        check(
            clip.format.name == "GrayS",
            f"{name}: the colour clip is GrayS (got {clip.format.name})",
        )
        check(clip.format.num_planes == 1, f"{name}: the colour clip is one gray plane")
        check(
            alpha_clip.format.name == "GrayS",
            f"{name}: the alpha clip is GrayS (got {alpha_clip.format.name})",
        )
        check(
            almost(
                samples(clip.get_frame(0)),
                [x / 15 for _ in range(8) for x in range(16)],
            ),
            f"{name}: the colour plane is the first sample Pillow reads",
        )
        check(
            almost(
                samples(alpha_clip.get_frame(0)),
                [y / 7 for y in range(8) for _ in range(16)],
            ),
            f"{name}: the alpha clip is the second sample Pillow reads",
        )
    # A page of two samples whose extra sample does not say it is alpha is
    # refused rather than guessed at.
    try:
        core.imgseqs.ReadAlpha(files=[fixture("tiff-graya-noalpha.tiff")])
    except Exception as error:
        check(
            "which is not alpha" in str(error),
            f"a two sample gray page with no alpha is refused ({str(error).strip()})",
        )
    else:
        check(False, "a two sample gray page with no alpha is refused")


def test_gray_exr() -> None:
    """A flat grayscale openexr is read: `Y` is the colour plane, `A` the alpha.

    ImageMagick writes three or four channels whatever it is asked for, so the
    two fixtures are hand written; see ``tests/make-tiff-exr-fixtures.py``. A
    file without an `A` hands out an opaque alpha clip, which is what every file
    without one does.
    """
    clip, alpha_clip = read_alpha(files=[fixture("exr-gray.exr")])
    check(
        (clip.width, clip.height) == (2, 2),
        f"exr-gray: the size is the layer's own window (got {clip.width}x{clip.height})",
    )
    check(
        clip.format.name == "GrayS",
        f"exr-gray: the colour clip is GrayS (got {clip.format.name})",
    )
    check(clip.format.num_planes == 1, "exr-gray: the colour clip is one gray plane")
    check(
        almost(samples(clip.get_frame(0)), [0.25, 0.5, 1.75, 2.0]),
        f"exr-gray: the colour plane is the `Y` samples"
        f" (got {samples(clip.get_frame(0))})",
    )
    check(
        almost(samples(alpha_clip.get_frame(0)), [1.0, 1.0, 1.0, 1.0]),
        "exr-gray: a file without an `A` hands out an opaque alpha clip",
    )

    clip, alpha_clip = read_alpha(files=[fixture("exr-gray-alpha.exr")])
    check(
        clip.format.name == "GrayS",
        f"exr-gray-alpha: the colour clip is GrayS (got {clip.format.name})",
    )
    check(
        almost(samples(clip.get_frame(0)), [0.25, 0.5, 1.75, 2.0]),
        f"exr-gray-alpha: the colour plane is the `Y` samples"
        f" (got {samples(clip.get_frame(0))})",
    )
    check(
        almost(samples(alpha_clip.get_frame(0)), [0.125, 0.25, 0.875, 1.0]),
        f"exr-gray-alpha: the alpha clip is the `A` samples"
        f" (got {samples(alpha_clip.get_frame(0))})",
    )


def test_twelve_bit_pages() -> None:
    """A twelve bit subsampled page is handed out as the planes it holds.

    The two avif fixtures are encoded from the y4m sources
    ``tests/make-alpha-fixtures.py`` writes, which is the only ``avifenc`` input
    that is already yuv, and ``-q 100`` is libavif's lossless setting: their
    planes are the samples the script wrote, so a twelve bit sample that arrived
    left aligned in a sixteen bit word, or a chroma plane of the wrong size, is a
    wrong picture rather than a plausible one. The heic is ``heif-enc --bit-depth
    12`` from a neutral sixteen bit png, whose r,g,b-to-yuv conversion is the
    encoder's rather than exact, so only its chroma is stated exactly.
    """
    for name, format_name, chroma_size in (
        ("avif-yuv420p12.avif", "YUV420P12", (4, 3)),
        ("avif-yuv422p12.avif", "YUV422P12", (4, 6)),
    ):
        clip, _ = read_alpha(files=[fixture(name)])
        check(
            clip.format.name == format_name,
            f"{name}: the clip is {format_name} (got {clip.format.name})",
        )
        check(
            (clip.width, clip.height) == (8, 6),
            f"{name}: the clip is 8x6 (got {clip.width}x{clip.height})",
        )
        frame = clip.get_frame(0)
        chroma_width, chroma_height = chroma_size
        check(
            len(samples(frame, 1)) == chroma_width * chroma_height,
            f"{name}: the chroma plane is its own size",
        )
        check(
            frame.props.get("_Matrix") == 6,
            f"{name}: the bt.601 matrix the file states (got {frame.props.get('_Matrix')})",
        )
        check(
            frame.props.get("_Range") == 0,
            f"{name}: the limited range the file states (got {frame.props.get('_Range')})",
        )
        check(
            samples(frame) == list(range(48)),
            f"{name}: the luma plane is the samples the source wrote"
            f" (got {samples(frame)[:8]})",
        )

        def chroma(offset: int) -> list[int]:
            return [
                offset + 100 * ((x + y) % 2)
                for y in range(chroma_height)
                for x in range(chroma_width)
            ]

        check(
            samples(frame, 1) == chroma(100),
            f"{name}: the first chroma plane is the source's"
            f" (got {samples(frame, 1)})",
        )
        check(
            samples(frame, 2) == chroma(300),
            f"{name}: the second chroma plane is the source's"
            f" (got {samples(frame, 2)})",
        )
        check(
            "_ChromaLocation" not in frame.props,
            f"{name}: a file that states no chroma position gains no property",
        )

    # The heic of a neutral twelve bit source: the encoder's conversion puts the
    # luma a few codes either side of the source's 256..3616 and both chroma
    # planes at the exact 128 a neutral source has.
    clip, _ = read_alpha(files=[fixture("heic-yuv420p12.heic")])
    check(
        clip.format.name == "YUV420P12",
        f"heic-yuv420p12: the clip is YUV420P12 (got {clip.format.name})",
    )
    frame = clip.get_frame(0)
    check(
        (clip.width, clip.height) == (8, 6),
        f"heic-yuv420p12: the clip is 8x6 (got {clip.width}x{clip.height})",
    )
    check(
        len(samples(frame, 1)) == 4 * 3,
        "heic-yuv420p12: the chroma plane is its own size",
    )
    check(
        samples(frame) == [256, 740, 1218, 1692, 2179, 2653, 3131, 3615] * 6,
        f"heic-yuv420p12: the luma plane is the decoder's"
        f" (got {samples(frame)[:8]})",
    )
    check(samples(frame, 1) == [2048] * 12, "heic-yuv420p12: the first chroma plane")
    check(samples(frame, 2) == [2048] * 12, "heic-yuv420p12: the second chroma plane")


def test_heic_storing_av1() -> None:
    """A heic whose primary item is av1 is read, which was a path with no file.

    ``heic-av1.heic`` is the coded item of ``avif-yuv420p.avif`` in a container
    whose major brand is ``heic``, written by ``tests/make-alpha-fixtures.py``:
    the item is the same ``av01`` payload and the brand is what routes the file to
    the heif reader rather than to the avif one. libheif's own dav1d backend
    decodes an av1 item, so the file answers the plan's question -- whether this
    is a working path or a hole -- and the answer is that the picture is the one
    the avif it was cut from holds, plane for plane, through two libraries.
    """
    clip, alpha_clip = read_alpha(files=[fixture("heic-av1.heic")])
    check(
        clip.format.name == "YUV420P8",
        f"heic-av1: the colour clip is YUV420P8 (got {clip.format.name})",
    )
    check(
        (clip.width, clip.height) == (64, 48),
        f"heic-av1: the clip is 64x48 (got {clip.width}x{clip.height})",
    )
    frame = clip.get_frame(0)
    source = core.imgseqs.Read(files=[fixture("avif-yuv420p.avif")]).get_frame(0)
    for plane in range(3):
        check(
            samples(frame, plane) == samples(source, plane),
            f"heic-av1: plane {plane} is the one the avif it was cut from holds",
        )
    check(
        set(samples(alpha_clip.get_frame(0))) == {255},
        "heic-av1: a file with no alpha item hands out an opaque alpha clip",
    )


def main() -> None:
    for test in (
        test_rgba8,
        test_la8,
        test_la16,
        test_rgba16,
        test_dds_and_farbfeld,
        test_qoi,
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
        test_gif_stills,
        test_png_feature_sets,
        test_separated_inks,
        test_planar_pages,
        test_narrow_gray_pages,
        test_palette_pages,
        test_ycbcr_pages,
        test_core_header_bitmaps,
        test_bare_dib,
        test_cursor,
        test_gray_alpha_pages,
        test_gray_exr,
        test_twelve_bit_pages,
        test_heic_storing_av1,
    ):
        print(f"--- {test.__name__}")
        test()

    if FAILURES:
        print(f"\n{len(FAILURES)} check(s) failed")
        raise SystemExit(1)
    print("\nall checks passed")


if __name__ == "__main__":
    main()
