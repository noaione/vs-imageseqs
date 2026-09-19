"""Validates ``ReadAlpha`` against the fixtures made by ``make-alpha-fixtures.py``.

Run from the repository root after ``cargo build --release``, with the
repository's virtual environment active:

    python tests/readalpha.vpy
"""

from __future__ import annotations

import struct
from pathlib import Path

import vapoursynth as vs

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
PLUGIN = ROOT / "target" / "release" / "vs_imageseqs.dll"
FIXTURES = HERE / "fixtures"

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
    check(frame.props["ImgSeqPath"].endswith("alpha-rgba8.png"), "rgba8: alpha frame keeps ImgSeqPath")
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


def test_monochrome_containers() -> None:
    """A monochrome heif and avif hand out one plane per sample.

    ``mono-alpha.png`` holds ``8 + 11x + 23y`` grey and ``240 - 9x - 13y`` alpha.
    The eight bit containers bring those numbers back unchanged in a ``Gray8``
    colour clip and a ``Gray8`` alpha clip. The ten bit avif stores each sample
    four times over, and dav1d hands the ten bit samples back left aligned in
    sixteen bits, so its planes are the same numbers times 256 within the
    rounding of that encode. Both avifs report three channels through the
    ``image`` decoder and are described from their container instead, so this
    covers that probe, the format it corrects and the writer that fills a grey
    plane from the wider buffer the decoder returns.
    """
    grey = [8 + 11 * x + 23 * y for y in range(MONO_HEIGHT) for x in range(MONO_WIDTH)]
    alpha_plane = [
        240 - 9 * x - 13 * y for y in range(MONO_HEIGHT) for x in range(MONO_WIDTH)
    ]
    cases = [
        ("mono-alpha.heic", "heic", "Gray8", 1, 0),
        ("mono-alpha.avif", "avif", "Gray8", 1, 0),
        ("mono-alpha-10.avif", "avif 10 bit", "Gray16", 256, 256),
    ]
    files = [fixture(name) for name, *_ in cases]
    clip, alpha = read_alpha(files=files, mismatch=True)
    check(len(clip) == len(files), "mono: every file becomes one frame")
    for index, (_, label, format_name, scale, tolerance) in enumerate(cases):
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
            almost(samples(color), [value * scale for value in grey], tolerance),
            f"mono {label}: the gray plane",
        )
        check(
            almost(samples(frame), [value * scale for value in alpha_plane], tolerance),
            f"mono {label}: the alpha plane",
        )
        check(frame.props["ImgSeqAlpha"] == 1, f"mono {label}: alpha frames carry ImgSeqAlpha")

    # The png pair the containers were encoded from has to agree with them.
    plain_clip, plain_alpha = read_alpha(files=[fixture("mono-alpha.png")])
    check(samples(plain_clip.get_frame(0)) == grey, "mono: the source png keeps the same gray plane")
    check(samples(plain_alpha.get_frame(0)) == alpha_plane, "mono: the source png keeps the same alpha")


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
            core.imgseqs.ReadAlpha(**arguments)
        except Exception as error:
            check(True, f"errors: {label} is rejected ({str(error).strip()})")
        else:
            check(False, f"errors: {label} is rejected")


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


def main() -> None:
    for test in (
        test_rgba8,
        test_la8,
        test_la16,
        test_rgba16,
        test_float,
        test_opaque_fill,
        test_alpha_presence_mixed,
        test_variable_depth,
        test_read_is_unchanged,
        test_monochrome_containers,
        test_color_containers,
        test_yuv_containers,
        test_color_metadata,
        test_orientation,
        test_orientation_yuv,
        test_orientation_jxl,
        test_prefetch_variants,
        test_interleaved_consumers,
        test_errors,
    ):
        print(f"--- {test.__name__}")
        test()

    if FAILURES:
        print(f"\n{len(FAILURES)} check(s) failed")
        raise SystemExit(1)
    print("\nall checks passed")


if __name__ == "__main__":
    main()
