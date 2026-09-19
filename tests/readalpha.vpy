"""Validates ``ReadAlpha`` against the fixtures made by ``make-alpha-fixtures.py``.

Run from the repository root after ``cargo build --release``:

    .venv\\Scripts\\python.exe tests\\readalpha.vpy
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
    """Colour files inside a heif or avif container keep their colour format."""
    files = [fixture("alpha-rgba8.heic"), fixture("alpha-rgba8.avif")]
    clip, alpha = read_alpha(files=files, mismatch=True)
    # The heif uses 4:2:0 chroma, so its blue plane rounds up by one sample.
    blue = {"heic": [4, 7, 10, 13, 16, 19], "avif": [3, 6, 9, 12, 15, 18]}
    for index, label in enumerate(("heic", "avif")):
        color = clip.get_frame(index)
        frame = alpha.get_frame(index)
        check(
            color.format.name == "RGB24",
            f"color {label}: the color clip stays RGB24 (got {color.format.name})",
        )
        check(samples(color, 0) == [1, 4, 7, 10, 13, 16], f"color {label}: red plane")
        check(samples(color, 1) == [2, 5, 8, 11, 14, 17], f"color {label}: green plane")
        check(samples(color, 2) == blue[label], f"color {label}: blue plane")
        check(samples(frame) == [9, 8, 7, 6, 5, 4], f"color {label}: alpha plane")


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
