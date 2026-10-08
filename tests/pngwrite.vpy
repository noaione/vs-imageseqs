"""Validates ``PNGWrite`` against the fixtures in ``tests/fixtures``.

The PNG reader below is written here rather than taken from a library, because
the point of these checks is that an independent reader agrees with what the
writer produced: the chunk sequence, the stored depth, the ``sBIT`` and ``cICP``
chunks and every sample are read straight out of the file and compared with the
VapourSynth frames the writer was given.

Run from the repository root after ``cargo build --release``, with the
repository's virtual environment active:

    python tests/pngwrite.vpy
"""

from __future__ import annotations

import hashlib
import os
import shutil
import struct
import sys
import zlib
from concurrent.futures import ThreadPoolExecutor
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
OUTPUT = ROOT / "target" / "pngwrite-validator"


class ValidationPolicy(vs.EnvironmentPolicy):
    """Keep an installed plugin from shadowing the binary under test."""

    def on_policy_registered(self, special_api):
        self.api = special_api
        self.environment = special_api.create_environment(
            vs.CoreCreationFlags.DISABLE_AUTO_LOADING
        )

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


def directory(name: str) -> Path:
    path = OUTPUT / name
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True)
    return path


def raised(call) -> str | None:
    """Calls `call`, returning its error message or None when it returned."""
    try:
        call()
    except vs.Error as error:
        return str(error)
    except Exception as error:
        return f"{type(error).__name__}: {error}"
    return None


# MARK: PNG reader


PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
CHANNELS = {0: 1, 2: 3, 4: 2, 6: 4}


def _paeth(left: int, up: int, up_left: int) -> int:
    estimate = left + up - up_left
    distance_left = abs(estimate - left)
    distance_up = abs(estimate - up)
    distance_up_left = abs(estimate - up_left)
    if distance_left <= distance_up and distance_left <= distance_up_left:
        return left
    if distance_up <= distance_up_left:
        return up
    return up_left


def _unfilter(raw: bytes, stride: int, bpp: int, height: int) -> bytes:
    out = bytearray()
    previous = bytearray(stride)
    at = 0
    for _ in range(height):
        kind = raw[at]
        at += 1
        line = bytearray(raw[at : at + stride])
        at += stride
        if kind == 1:
            for index in range(bpp, stride):
                line[index] = (line[index] + line[index - bpp]) & 0xFF
        elif kind == 2:
            for index in range(stride):
                line[index] = (line[index] + previous[index]) & 0xFF
        elif kind == 3:
            for index in range(stride):
                left = line[index - bpp] if index >= bpp else 0
                line[index] = (line[index] + ((left + previous[index]) >> 1)) & 0xFF
        elif kind == 4:
            for index in range(stride):
                left = line[index - bpp] if index >= bpp else 0
                up = previous[index]
                up_left = previous[index - bpp] if index >= bpp else 0
                line[index] = (line[index] + _paeth(left, up, up_left)) & 0xFF
        elif kind != 0:
            raise AssertionError(f"unknown PNG filter {kind}")
        out += line
        previous = line
    return bytes(out)


class Png:
    """The parts of a produced PNG these checks look at."""

    def __init__(self, path: Path):
        self.path = Path(path)
        data = self.path.read_bytes()
        if data[:8] != PNG_SIGNATURE:
            raise AssertionError(f"{self.path} is not a PNG")
        self.chunks: list[bytes] = []
        self.rows: list[list[int]] = []
        self.sbit: bytes | None = None
        self.cicp: bytes | None = None
        self.icc: bytes | None = None
        self.icc_name: str | None = None
        idat = bytearray()
        at = 8
        while at < len(data):
            length = struct.unpack_from(">I", data, at)[0]
            kind = data[at + 4 : at + 8]
            body = data[at + 8 : at + 8 + length]
            stored = struct.unpack_from(">I", data, at + 8 + length)[0]
            if stored != zlib.crc32(kind + body) & 0xFFFFFFFF:
                raise AssertionError(f"{self.path}: {kind!r} has a wrong CRC")
            self.chunks.append(kind)
            if kind == b"IHDR":
                (
                    self.width,
                    self.height,
                    self.depth,
                    self.color_type,
                    compression,
                    filtering,
                    self.interlace,
                ) = struct.unpack(">IIBBBBB", body)
                if (compression, filtering, self.interlace) != (0, 0, 0):
                    raise AssertionError(f"{self.path}: unsupported PNG layout")
            elif kind == b"sBIT":
                self.sbit = body
            elif kind == b"cICP":
                self.cicp = body
            elif kind == b"iCCP":
                name, _, rest = body.partition(b"\x00")
                self.icc_name = name.decode("latin-1")
                if rest[:1] != b"\x00":
                    raise AssertionError(f"{self.path}: unknown iCCP method")
                self.icc = zlib.decompress(rest[1:])
            elif kind == b"IDAT":
                idat += body
            at += 12 + length
        self.channels = CHANNELS[self.color_type]
        self.sample_bytes = self.depth // 8
        # A depth below a byte packs several samples into one, and png pads the
        # last byte of a row rather than the row itself.
        self.stride = (self.width * self.channels * self.depth + 7) // 8
        self.packed = _unfilter(
            zlib.decompress(bytes(idat)),
            self.stride,
            max(1, self.channels * self.sample_bytes),
            self.height,
        )
        for row in range(self.height):
            base = row * self.stride
            samples: list[int] = []
            for index in range(self.width * self.channels):
                if self.depth == 8:
                    samples.append(self.packed[base + index])
                elif self.depth == 16:
                    samples.append(
                        struct.unpack_from(">H", self.packed, base + index * 2)[0]
                    )
                else:
                    per_byte = 8 // self.depth
                    byte = self.packed[base + index // per_byte]
                    samples.append(
                        (byte >> (8 - self.depth * (index % per_byte + 1)))
                        & ((1 << self.depth) - 1)
                    )
            self.rows.append(samples)

    def samples(self) -> list[int]:
        return [sample for row in self.rows for sample in row]

    def row_bytes(self, row: int) -> bytes:
        """One row as the file packs it, before any unpacking."""
        return self.packed[row * self.stride : (row + 1) * self.stride]

    def chunk_order_ok(self) -> bool:
        """sBIT and cICP have to precede IDAT, whatever else is around them."""
        first_idat = self.chunks.index(b"IDAT")
        return all(
            self.chunks.index(kind) < first_idat
            for kind in (b"sBIT", b"cICP", b"iCCP")
            if kind in self.chunks
        )


# MARK: frame readers


def plane_samples(frame, plane: int) -> list[int]:
    """The samples of one plane, in the order the PNG row interleaves them."""
    raw = frame[plane].tobytes()
    if frame.format.bytes_per_sample == 1:
        return list(raw)
    return [struct.unpack_from("<H", raw, at)[0] for at in range(0, len(raw), 2)]


def widen(sample: int, bits: int) -> int:
    """The sample the writer stores for a frame of `bits` bits."""
    # Only nine through fifteen bits are stored wider than the frame is: an
    # eight bit frame is an eight bit PNG and a sixteen bit one is unchanged.
    if bits <= 8 or bits >= 16:
        return sample
    return widen_to(sample, bits, 16)


def expected_rows(color, alpha, bits: int) -> list[list[int]]:
    planes = [plane_samples(color, plane) for plane in range(color.format.num_planes)]
    if alpha is not None:
        planes.append(plane_samples(alpha, 0))
    width, height = color.width, color.height
    rows = []
    for row in range(height):
        samples = []
        for column in range(width):
            for plane in planes:
                samples.append(widen(plane[row * width + column], bits))
        rows.append(samples)
    return rows


def frame_hash(frame) -> str:
    digest = hashlib.sha256()
    for plane in range(frame.format.num_planes):
        digest.update(frame[plane].tobytes())
    return digest.hexdigest()


def interleaved(frame) -> list[list[int]]:
    """The samples of a fully sampled frame, in the order a PNG row holds them."""
    planes = [plane_samples(frame, plane) for plane in range(frame.format.num_planes)]
    rows = []
    for row in range(frame.height):
        samples = []
        for column in range(frame.width):
            for plane in planes:
                samples.append(plane[row * frame.width + column])
        rows.append(samples)
    return rows


def worst_sample(got: list[list[int]], want: list[list[int]], shift: int = 0) -> int:
    """The largest difference between two row lists, one sample at a time.

    `shift` moves a stored sample back down to the reference's own depth, which
    is what a widened word needs: ten bits of picture in a sixteen bit sample
    repeat themselves in the low six bits.
    """
    worst = 0
    for row, samples in enumerate(got):
        for index, sample in enumerate(samples):
            worst = max(worst, abs((sample >> shift) - want[row][index]))
    return worst


def packed_row(samples: list[int], bits: int) -> bytes:
    """One row as png packs it, most significant bits first."""
    out = bytearray()
    accumulator = 0
    for index, sample in enumerate(samples):
        accumulator = (accumulator << bits) | sample
        if (index + 1) * bits % 8 == 0:
            out.append(accumulator & 0xFF)
            accumulator = 0
    if len(samples) * bits % 8:
        out.append((accumulator << (8 - len(samples) * bits % 8)) & 0xFF)
    return bytes(out)


def widen_to(sample: int, bits: int, stored: int) -> int:
    """The sample a `bits` bit value becomes in a `stored` bit word.

    The high bits are the sample and the low ones repeat it, so the largest
    value of the narrow word is the largest of the wide one.
    """
    shift = stored - bits
    low = sample >> (2 * bits - stored) if 2 * bits >= stored else 0
    return ((sample << shift) | low) & ((1 << stored) - 1)


def refused(clip, path, **arguments) -> str | None:
    """Requests one frame, returning the error message or None when it wrote."""
    return raised(lambda: write(clip, path, **arguments))


def write(clip, path, **arguments):
    """Creates a writer and requests one frame, returning the frame."""
    writer = core.imgseqs.PNGWrite(clip, output_path=str(path), **arguments)
    with writer.get_frame(0) as frame:
        properties = dict(frame.props)
    return properties


def color_type_of(format, alpha: bool) -> int:
    if format.color_family is vs.GRAY:
        return 4 if alpha else 0
    return 6 if alpha else 2


# MARK: checks


def test_roundtrip() -> None:
    """Every supported color type comes back sample for sample."""
    cases = [
        ("gray8", "png-plain-gray8.png", False),
        ("rgb8", "png-plain-rgb8.png", False),
        ("gray16", "tiff-gray16.tiff", False),
        ("rgb16", "tiff-rgb16.tiff", False),
        ("gray8-opaque-alpha", "alpha-l8.png", True),
        ("gray8-alpha", "alpha-la8.png", True),
        ("rgba8", "alpha-rgba8.png", True),
        ("rgba16", "alpha-rgba16.png", True),
        ("gray16-alpha", "tiff-gray16.tiff", True),
    ]
    out = directory("roundtrip")
    for name, source, with_alpha in cases:
        destination = out / f"{name}.png"
        if with_alpha:
            result = core.imgseqs.ReadAlpha(files=[fixture(source)])
            clip, alpha = result["clip"], result["alpha"]
        else:
            clip = core.imgseqs.Read(files=[fixture(source)])
            alpha = None
        expected_color = clip.get_frame(0)
        expected_alpha = alpha.get_frame(0) if alpha is not None else None
        bits = expected_color.format.bits_per_sample
        properties = write(clip, destination, alpha=alpha, overwrite=True)

        png = Png(destination)
        check(
            png.depth == (8 if bits <= 8 else 16),
            f"{name}: the stored depth is {png.depth} for a {bits} bit frame",
        )
        check(
            png.color_type == color_type_of(expected_color.format, alpha is not None),
            f"{name}: the stored color type is {png.color_type}",
        )
        check(
            png.rows == expected_rows(expected_color, expected_alpha, bits),
            f"{name}: every sample is the frame's sample",
        )
        check(
            (png.sbit is not None) == (9 <= bits <= 15),
            f"{name}: sBIT is {'present' if 9 <= bits <= 15 else 'absent'} at {bits} bits",
        )
        check(png.chunk_order_ok(), f"{name}: auxiliary chunks precede IDAT")
        check(
            properties["ImgSeqPNGWritePath"] == str(destination),
            f"{name}: the receipt names the destination",
        )
        check(
            int(properties["ImgSeqPNGWriteSaved"]) == 1
            and int(properties["ImgSeqPNGWritePerformed"]) == 1,
            f"{name}: the receipt reports the write",
        )
        # The output frame is the input frame: the writer hands the pixels back
        # and only adds what it did with them.
        with core.imgseqs.PNGWrite(
            clip, output_path=str(out / f"{name}-again.png"), alpha=alpha, overwrite=True
        ).get_frame(0) as written:
            check(
                frame_hash(written) == frame_hash(expected_color),
                f"{name}: the returned frame is the input frame",
            )
            check(
                written.props["ImgSeqPath"] == expected_color.props["ImgSeqPath"],
                f"{name}: the input properties survive the writer",
            )
        check(
            expected_color.props["ImgSeqPath"] == fixture(source),
            f"{name}: the source property is what the reader wrote",
        )
        expected_color.close()
        if expected_alpha is not None:
            expected_alpha.close()


def test_widening() -> None:
    """An intermediate depth is stored at sixteen bits, exactly."""
    cases = [
        ("gray10", "jxl-gray10.jxl"),
        ("gray12", "jxl-gray12.jxl"),
    ]
    out = directory("widening")
    for name, source in cases:
        clip = core.imgseqs.Read(files=[fixture(source)])
        frame = clip.get_frame(0)
        bits = frame.format.bits_per_sample
        check(9 <= bits <= 15, f"{name}: the fixture is {bits} bits")
        destination = out / f"{name}.png"
        write(clip, destination, overwrite=True)
        png = Png(destination)
        check(png.depth == 16, f"{name}: a {bits} bit frame is stored at 16 bits")
        check(
            png.sbit == bytes([bits]),
            f"{name}: sBIT states {bits} significant bits, got {png.sbit!r}",
        )
        expected = expected_rows(frame, None, bits)
        check(png.rows == expected, f"{name}: every sample is the widened source")
        # The high bits are the source, so the store is reversible; and the
        # largest source sample becomes the full scale rather than short of it.
        source_maximum = (1 << bits) - 1
        recovered = [sample >> (16 - bits) for sample in png.samples()]
        check(
            recovered == plane_samples(frame, 0),
            f"{name}: the source sample is recoverable from the high bits",
        )
        if source_maximum in plane_samples(frame, 0):
            check(
                max(png.samples()) == 0xFFFF,
                f"{name}: the largest source sample is fully opaque or white",
            )
        frame.close()

    # An rgb frame at ten bits rides the same path, with the alpha beside it.
    result = core.imgseqs.ReadAlpha(files=[fixture("jxl-rgba10.jxl")])
    color, alpha = result["clip"], result["alpha"]
    frame = color.get_frame(0)
    alpha_frame = alpha.get_frame(0)
    destination = out / "rgba10.png"
    write(color, destination, alpha=alpha, overwrite=True)
    png = Png(destination)
    check(png.depth == 16 and png.color_type == 6, "rgba10: stored as 16 bit RGBA")
    check(png.sbit == bytes([10, 10, 10, 10]), "rgba10: sBIT states every channel")
    check(
        png.rows == expected_rows(frame, alpha_frame, 10),
        "rgba10: every sample is the widened source",
    )
    alphas = [row[3] for row in png.rows]
    if max(plane_samples(alpha_frame, 0)) == 1023:
        check(max(alphas) == 0xFFFF, "rgba10: a fully opaque source is opaque")
    frame.close()
    alpha_frame.close()


def test_compression() -> None:
    """Every level is lossless and level 0 stores rather than compresses."""
    out = directory("compression")
    clip = core.imgseqs.Read(files=[fixture("png-plain-rgb8.png")])
    expected = expected_rows(clip.get_frame(0), None, 8)
    sizes = {}
    for level in range(10):
        destination = out / f"level{level}.png"
        write(clip, destination, compression=level, overwrite=True)
        png = Png(destination)
        check(
            png.rows == expected,
            f"compression {level}: decodes to the source samples",
        )
        check(
            destination.stat().st_size > 0,
            f"compression {level}: the file is complete",
        )
        sizes[level] = destination.stat().st_size
    check(
        sizes[0] == max(sizes.values()),
        f"compression 0 is the largest file ({sizes[0]} of {max(sizes.values())})",
    )
    check(
        sizes[9] <= sizes[1],
        f"compression 9 is no larger than 1 ({sizes[9]} <= {sizes[1]})",
    )
    for level in (-1, 10, 100):
        message = raised(
            lambda level=level: core.imgseqs.PNGWrite(
                clip, output_path=str(out / "bad.png"), compression=level
            )
        )
        check(
            message is not None and "compression" in message,
            f"compression {level} is refused ({message})",
        )


def test_metadata() -> None:
    """The chunks a file states are the ones the frame actually holds."""
    out = directory("metadata")
    cicp = core.imgseqs.Read(files=[fixture("cicp-rgb8.png")])
    frame = cicp.get_frame(0)
    primaries = int(frame.props["_Primaries"])
    transfer = int(frame.props["_Transfer"])
    frame.close()
    destination = out / "cicp.png"
    write(cicp, destination, overwrite=True)
    png = Png(destination)
    check(
        png.cicp == bytes([primaries, transfer, 0, 1]),
        f"cICP states {primaries}/{transfer} with the matrix fixed at zero, got {png.cicp!r}",
    )
    # A frame whose matrix names a conversion is not what the chunk describes.
    converted = core.std.SetFrameProp(cicp, prop="_Matrix", intval=1)
    write(converted, out / "cicp-matrix.png", overwrite=True)
    check(
        Png(out / "cicp-matrix.png").cicp is None,
        "a yuv matrix keeps cICP out of the file",
    )
    limited = core.std.SetFrameProp(cicp, prop="_Range", intval=0)
    write(limited, out / "cicp-limited.png", overwrite=True)
    check(
        Png(out / "cicp-limited.png").cicp is None,
        "a limited range frame keeps cICP out of the file",
    )

    # An ICC profile is stored, not applied, and only when it was asked for.
    profile_clip = core.imgseqs.Read(files=[fixture("icc-rgb8.png")], icc_profile=True)
    profile = profile_clip.get_frame(0).props["ICCProfile"]
    write(profile_clip, out / "icc-on.png", icc_profile=True, overwrite=True)
    png = Png(out / "icc-on.png")
    check(png.icc == bytes(profile), "the embedded profile is byte for byte the frame's")  # pyright: ignore[reportArgumentType]
    check(png.icc_name is not None, "the profile has a name")
    write(profile_clip, out / "icc-off.png", overwrite=True)
    check(
        Png(out / "icc-off.png").icc is None,
        "no profile is embedded unless it was asked for",
    )
    # Eight bit and sixteen bit frames keep the depth they have, so a profile
    # that describes the eight bit picture is still describing the pixels.
    check(
        Png(out / "icc-on.png").rows == expected_rows(profile_clip.get_frame(0), None, 8),
        "the profiled file decodes to the same samples",
    )


def test_paths() -> None:
    """Destination templates, numbering and the no-clobber rule."""
    out = directory("paths")
    sequence = core.std.BlankClip(width=8, height=8, format=vs.GRAY8, length=4)

    # Creating the node writes nothing at all.
    writer = core.imgseqs.PNGWrite(sequence, output_path=str(out / "lazy%02d.png"))
    check(
        list(out.iterdir()) == [],
        "creating the writer writes nothing",
    )
    for index in (2, 0, 3):
        with writer.get_frame(index):
            pass
    check(
        sorted(path.name for path in out.iterdir())
        == ["lazy00.png", "lazy02.png", "lazy03.png"],
        "the requested frames are the files that exist",
    )

    # start_number moves the numbering, not the frame index.
    numbered = directory("paths-numbered")
    writer = core.imgseqs.PNGWrite(
        sequence, output_path=str(numbered / "frame%06d.png"), start_number=1000
    )
    with writer.get_frame(1) as frame:
        check(
            str(numbered / "frame001001.png") == frame.props["ImgSeqPNGWritePath"],
            "start_number is added to the frame index",
        )
    check((numbered / "frame001001.png").is_file(), "the numbered destination exists")

    # A literal destination names one frame.
    single = core.std.BlankClip(width=8, height=8, format=vs.GRAY8, length=1)
    write(single, out / "literal.png")
    check((out / "literal.png").is_file(), "a one frame clip may use a literal path")

    bad = {
        "an empty path": "",
        "a literal path for a sequence": str(out / "literal.png"),
        "an unknown substitution": str(out / "frame%q.png"),
        "a truncated substitution": str(out / "frame%"),
        "a padding wider than a number": str(out / "frame%099999999999999999999999d.png"),
        "a directory that does not exist": str(out / "missing" / "frame%02d.png"),
    }
    for description, path in bad.items():
        arguments = {}
        clip = sequence if description != "a one frame clip" else single
        message = raised(
            lambda path=path, clip=clip: core.imgseqs.PNGWrite(
                clip, output_path=path, **arguments
            )
        )
        check(message is not None, f"{description} is refused ({message})")

    message = raised(
        lambda: core.imgseqs.PNGWrite(
            sequence,
            output_path=str(out / "overflow%02d.png"),
            start_number=2**63 - 1,
        )
    )
    check(
        message is not None and "overflow" in message,
        f"a numbering overflow is refused at creation ({message})",
    )

    # A destination that is already there is an error, not a silent success,
    # and it is still there afterwards.
    destination = out / "existing.png"
    destination.write_bytes(b"not a png")
    message = raised(lambda: write(single, destination))
    check(
        message is not None and str(destination) in message,
        f"an existing destination is refused by name ({message})",
    )
    check(
        destination.read_bytes() == b"not a png",
        "the existing destination is untouched after the failure",
    )
    # ... unless replacing it was asked for.
    properties = write(single, destination, overwrite=True)
    check(
        int(properties["ImgSeqPNGWritePerformed"]) == 1 and Png(destination).depth == 8,
        "overwrite replaces the existing destination",
    )
    # A frame that failed is not recorded as saved, so a later request retries.
    destination.write_bytes(b"not a png")
    message = raised(lambda: write(single, destination))
    check(message is not None, "the second refusal happens too")
    write(single, destination, overwrite=True)
    check(destination.read_bytes()[:8] == PNG_SIGNATURE, "and the retry succeeds")

    # An output directory that disappears between creation and the request
    # is an error rather than a silent success, and the frame it refused is
    # not remembered as saved, so a later request writes it once the
    # directory is back.
    vanishing = OUTPUT / "paths-vanishing"
    if vanishing.exists():
        shutil.rmtree(vanishing)
    vanishing.mkdir(parents=True)
    writer = core.imgseqs.PNGWrite(
        sequence, output_path=str(vanishing / "frame%02d.png")
    )
    shutil.rmtree(vanishing)
    message = raised(lambda: writer.get_frame(0))
    check(
        message is not None and str(vanishing) in message,
        f"a destination directory that is gone fails the write ({message})",
    )
    vanishing.mkdir(parents=True)
    with writer.get_frame(0) as frame:
        check(
            int(frame.props["ImgSeqPNGWritePerformed"]) == 1,
            "the frame is written once the directory is back",
        )
    check((vanishing / "frame00.png").is_file(), "the retried write lands")

    # Two writer nodes over one destination cannot both claim it: the
    # publication is a linking create, so exactly one of two racing writers
    # publishes and the other is told the destination is there. Neither
    # leaves its temporary file behind.
    race = directory("paths-race")
    template = race / "same.png"
    racers = [
        core.imgseqs.PNGWrite(single, output_path=str(template)),
        core.imgseqs.PNGWrite(single, output_path=str(template)),
    ]

    def claim(writer):
        try:
            with writer.get_frame(0):
                return None
        except vs.Error as error:
            return str(error)

    with ThreadPoolExecutor(max_workers=2) as pool:
        outcomes = list(pool.map(claim, racers))
    check(
        sum(outcome is None for outcome in outcomes) == 1,
        f"exactly one of two racing writers publishes ({outcomes})",
    )
    check(
        sum(outcome is not None for outcome in outcomes) == 1,
        "the other racing writer is told the destination exists",
    )
    check(template.is_file(), "the winner's file is there")
    check(
        not any(path.name.endswith(".pngwrite") for path in race.iterdir()),
        "neither writer leaves a temporary file behind",
    )

    # A unicode directory and a relative path resolved at creation.
    unicode_directory = OUTPUT / "paths-unicode" / "漫画"
    unicode_directory.mkdir(parents=True, exist_ok=True)
    write(single, unicode_directory / "ページ.png", overwrite=True)
    check(
        Png(unicode_directory / "ページ.png").depth == 8,
        "a unicode destination is written",
    )

    relative = directory("paths-relative")
    (relative / "moved").mkdir()
    previous = Path.cwd()
    os.chdir(relative)
    try:
        writer = core.imgseqs.PNGWrite(
            sequence,
            output_path=os.path.join("moved", "frame%02d.png"),  # ruff: ignore[os-path-join]
            overwrite=True,
        )
    finally:
        os.chdir(previous)
    # The template was resolved while the node was created, so the directory
    # that is current at frame request time cannot move the destination.
    elsewhere = directory("paths-elsewhere")
    os.chdir(elsewhere)
    try:
        with writer.get_frame(0):
            pass
    finally:
        os.chdir(previous)
    check(
        (relative / "moved" / "frame00.png").is_file(),
        "a relative path resolves where the writer was created",
    )
    check(
        list(elsewhere.iterdir()) == [],
        "the directory that was current at request time is untouched",
    )


def test_repeat() -> None:
    """A saved frame is skipped, and always_save turns that off."""
    out = directory("repeat")
    sequence = core.std.BlankClip(width=8, height=8, format=vs.GRAY8, length=2)
    destination = out / "frame%02d.png"

    writer = core.imgseqs.PNGWrite(sequence, output_path=str(destination))
    with writer.get_frame(0) as frame:
        check(int(frame.props["ImgSeqPNGWritePerformed"]) == 1, "the first request writes")
    first = (out / "frame00.png").stat()
    # The node keeps its own frame cache, which is what makes a second request
    # for one frame return the first evaluation's frame instead of evaluating
    # again. Clearing it is what puts the skip path under test.
    writer.clear_cache()
    with writer.get_frame(0) as frame:
        check(
            int(frame.props["ImgSeqPNGWriteSaved"]) == 1
            and int(frame.props["ImgSeqPNGWritePerformed"]) == 0,
            "a repeated request skips the write",
        )
    check(
        (out / "frame00.png").stat().st_mtime_ns == first.st_mtime_ns,
        "the skipped frame leaves the file alone",
    )

    always = core.imgseqs.PNGWrite(
        sequence, output_path=str(out / "always%02d.png"), always_save=True, overwrite=True
    )
    for _ in range(2):
        always.clear_cache()
        with always.get_frame(0) as frame:
            check(
                int(frame.props["ImgSeqPNGWritePerformed"]) == 1,
                "always_save writes again",
            )

    # always_save does not imply the right to replace a destination.
    guarded = core.imgseqs.PNGWrite(
        sequence, output_path=str(out / "guarded%02d.png"), always_save=True
    )
    with guarded.get_frame(0):
        pass
    guarded.clear_cache()
    message = raised(lambda: guarded.get_frame(0))
    check(
        message is not None and "guarded00.png" in message,
        f"always_save without overwrite refuses the second write ({message})",
    )


def test_errors() -> None:
    """Unsupported and inconsistent inputs are refused with a reason."""
    out = directory("errors")
    # A yuv or a float clip is converted rather than refused, and `test_yuv`
    # and `test_float` are where those two conversions are checked.

    color = core.imgseqs.Read(files=[fixture("png-plain-gray8.png")])
    small = core.std.BlankClip(width=4, height=4, format=vs.GRAY8, length=1)
    message = raised(
        lambda: core.imgseqs.PNGWrite(
            color, output_path=str(out / "small.png"), alpha=small
        )
    )
    check(
        message is not None and "size" in message,
        f"a mismatched alpha size is refused ({message})",
    )
    deep = core.std.BlankClip(width=37, height=23, format=vs.GRAY16, length=1)
    message = raised(
        lambda: core.imgseqs.PNGWrite(color, output_path=str(out / "deep.png"), alpha=deep)
    )
    check(
        message is not None and "depth" in message,
        f"a mismatched alpha depth is refused ({message})",
    )
    shorter = core.std.Trim(small, 0, 0)
    message = raised(
        lambda: core.imgseqs.PNGWrite(
            core.std.BlankClip(width=4, height=4, format=vs.GRAY8, length=3),
            output_path=str(out / "short.png"),
            alpha=shorter,
        )
    )
    check(
        message is not None and "frame count" in message,
        f"a mismatched alpha frame count is refused ({message})",
    )
    yuv4 = core.imgseqs.Read(files=[fixture("avif-yuv420p.avif")])
    message = raised(
        lambda: core.imgseqs.PNGWrite(
            color, output_path=str(out / "yuv-alpha.png"), alpha=yuv4
        )
    )
    check(
        message is not None and "gray alpha" in message.lower(),
        f"an alpha clip that is not gray is refused ({message})",
    )

    # A clip of varying frames is accepted frame by frame, and a frame the
    # writer cannot hand out is refused with the reason for that frame.
    mixed = core.imgseqs.Read(
        files=[fixture("png-plain-rgb8.png"), fixture("png-plain-gray8.png")],
        mismatch=True,
    )
    writer = core.imgseqs.PNGWrite(
        mixed, output_path=str(out / "mixed%02d.png"), overwrite=True
    )
    with writer.get_frame(0) as written:
        check(
            int(written.props["ImgSeqPNGWritePerformed"]) == 1,
            "a variable clip writes an rgb frame",
        )
    with writer.get_frame(1):
        pass
    check(
        Png(out / "mixed00.png").color_type == 2
        and Png(out / "mixed01.png").color_type == 0,
        "each frame of a variable clip is written as its own format",
    )
    # A frame of another format or size is converted frame by frame, so one
    # clip writes an rgb frame and then a yuv one as r,g,b of its own size.
    converted = core.imgseqs.Read(
        files=[fixture("png-plain-rgb8.png"), fixture("avif-yuv420p.avif")],
        mismatch=True,
    )
    writer = core.imgseqs.PNGWrite(
        converted, output_path=str(out / "converted%02d.png"), overwrite=True
    )
    with writer.get_frame(0):
        pass
    with writer.get_frame(1):
        pass
    first = Png(out / "converted00.png")
    second = Png(out / "converted01.png")
    check(
        first.color_type == 2 and first.depth == 8,
        "the rgb frame of a variable clip is written as it is",
    )
    check(
        second.color_type == 2 and second.depth == 8,
        "and the yuv frame of another size is converted to r,g,b",
    )
    check(
        (first.width, first.height) == (37, 23)
        and (second.width, second.height) == (64, 48),
        f"every frame keeps its own size ({(first.width, first.height)} and {(second.width, second.height)})",
    )


def test_yuv() -> None:
    """A yuv frame becomes the r,g,b a PNG stores."""
    out = directory("yuv")
    # The readers hand a lossy webp out as 4:2:0 at bt.470bg and limited range,
    # and a colour avif as its own sampling at the matrix its container states,
    # so these four are every sampling at both words and both ranges.
    for name, matrix, ranges, sampling, shift in [
        ("avif-yuv420p.avif", "170m", "full", (1, 1), 0),
        ("avif-yuv422p.avif", "170m", "full", (1, 0), 0),
        ("avif-yuv444p10.avif", "170m", "full", (0, 0), 6),
        ("orientation-6.webp", "470bg", "limited", (1, 1), 0),
    ]:
        clip = core.imgseqs.Read(files=[fixture(name)], prefetch=0)
        check(
            (clip.format.subsampling_w, clip.format.subsampling_h) == sampling,
            f"{name} is subsampled the way this check assumes",
        )
        with clip.get_frame(0) as frame:
            props = dict(frame.props)
        bits = clip.format.bits_per_sample
        reference = core.resize.Bilinear(
            clip,
            format=vs.RGB24 if bits <= 8 else vs.RGB30,
            matrix_in_s=matrix,
            range_in_s=ranges,
        )
        path = out / f"{name}.png"
        write(clip, path)
        png = Png(path)
        with reference.get_frame(0) as frame:
            want = interleaved(frame)
        check(
            png.depth == (8 if bits <= 8 else 16),
            f"{name}: a {bits} bit yuv frame becomes a {png.depth} bit png",
        )
        check(
            worst_sample(png.rows, want, shift) <= 1,
            f"{name}: every sample is the one resize produces (worst {worst_sample(png.rows, want, shift)})",
        )
        # The chunk describes the r,g,b the file holds rather than the yuv the
        # frame was coded as, so a frame that states primaries and a transfer
        # gets those beside full range r,g,b instead of its own matrix.
        if "_Primaries" in props:
            stated = bytes([int(props["_Primaries"]), int(props["_Transfer"]), 0, 1])  # pyright: ignore[reportArgumentType]
            check(
                png.cicp == stated,
                f"{name}: cICP is the frame's colour as full range rgb ({list(png.cicp or b'')})",
            )
        else:
            check(png.cicp is None, f"{name}: a frame that states no colour writes no cICP")
        if bits > 8:
            check(
                png.sbit == bytes([bits] * 3),
                f"{name}: sBIT states the {bits} bits of picture ({list(png.sbit or b'')})",
            )
        check(png.chunk_order_ok(), f"{name}: the chunks precede IDAT")

    # A yuv frame that states no matrix has to be told which one it is, and the
    # argument is also how one whose container states the wrong matrix is read
    # as the right one, which is what resize's own matrix_in is for.
    plain = core.std.BlankClip(
        width=8, height=8, length=1, format=vs.YUV444P8, color=[100, 130, 140]
    )
    with plain.get_frame(0) as frame:
        if "_Matrix" in frame.props:
            plain = core.std.SetFrameProp(plain, prop="_Matrix", delete=True)  # pyright: ignore[reportCallIssue]
    with plain.get_frame(0) as frame:
        check("_Matrix" not in frame.props, "the yuv clip used here states no matrix")
    message = refused(plain, out / "no-matrix.png")
    check(
        message is not None and "matrix=" in message,
        f"a yuv frame that states no matrix names the argument ({message})",
    )
    path = out / "argued-matrix.png"
    write(plain, path, matrix=1)
    reference = core.resize.Bilinear(
        plain, format=vs.RGB24, matrix_in_s="709", range_in_s="limited"
    )
    with reference.get_frame(0) as frame:
        want = interleaved(frame)
    check(
        worst_sample(Png(path).rows, want) <= 1,
        "the matrix argument is the matrix the conversion reads with",
    )
    # A code this writer does not convert is refused by name rather than
    # converted as though it were another one.
    for code, said in [
        (2, "states none"),
        (10, "constant luminance"),
        (13, "constant luminance"),
        (14, "constant luminance"),
        (3, "not one VapourSynth names"),
    ]:
        message = refused(
            core.std.SetFrameProp(plain, prop="_Matrix", intval=code),
            out / f"matrix-{code}.png",
        )
        check(
            message is not None and said in message,
            f"the matrix code {code} is refused because it {said} ({message})",
        )
    # Where a subsampled frame's chroma samples sit is the other thing a
    # container states, and every position is a picture of its own.
    colored = core.imgseqs.Read(files=[fixture("avif-yuv420p.avif")], prefetch=0)
    positions = []
    for code, name in [(0, "left"), (1, "center"), (2, "top_left"), (5, "bottom")]:
        located = core.std.SetFrameProp(colored, prop="_ChromaLocation", intval=code)
        path = out / f"chromaloc-{name}.png"
        write(located, path)
        png = Png(path)
        positions.append(tuple(tuple(row) for row in png.rows))
        reference = core.resize.Bilinear(
            located,
            format=vs.RGB24,
            matrix_in_s="170m",
            range_in_s="full",
            chromaloc_in_s=name,
        )
        with reference.get_frame(0) as frame:
            want = interleaved(frame)
        check(
            worst_sample(png.rows, want) <= 1,
            f"chroma at the {name} position is where resize reads it",
        )
    check(
        len(set(positions)) > 1,
        "the chroma position changes the picture",
    )
    # A yuv colour clip beside the gray alpha clip its container carries.
    result = core.imgseqs.ReadAlpha(files=[fixture("alpha-yuv420p.avif")])
    color, alpha_clip = result["clip"], result["alpha"]
    path = out / "yuv-alpha.png"
    write(color, path, alpha=alpha_clip)
    png = Png(path)
    check(
        png.color_type == 6 and png.channels == 4,
        f"a yuv colour clip beside a gray alpha clip is rgba ({png.color_type})",
    )
    reference = core.resize.Bilinear(
        color, format=vs.RGB24, matrix_in_s="170m", range_in_s="full"
    )
    with reference.get_frame(0) as frame:
        want = interleaved(frame)
    with alpha_clip.get_frame(0) as frame:
        alpha_plane = plane_samples(frame, 0)
    worst = 0
    for row in range(png.height):
        for column in range(png.width):
            for channel in range(3):
                worst = max(
                    worst,
                    abs(
                        png.rows[row][column * 4 + channel]
                        - want[row][column * 3 + channel]
                    ),
                )
            worst = max(
                worst,
                abs(
                    png.rows[row][column * 4 + 3]
                    - alpha_plane[row * png.width + column]
                ),
            )
    check(worst <= 1, f"a converted yuv colour keeps its alpha (worst {worst})")


def test_float() -> None:
    """A float frame is a value in [0, 1] rather than a code."""
    out = directory("float")
    # The whole mapping, and the values that are not a picture at all: a NaN is
    # written as the smallest sample and an infinity clamps like any other value
    # outside the range.
    base = core.std.BlankClip(width=2, height=1, length=1, format=vs.GRAYS, color=0.0)
    for expression, want in [
        ("0.0", 0),
        ("0.25", 16384),
        ("0.5", 32768),
        ("1.0", 65535),
        ("2.0", 65535),
        ("-1.0", 0),
        ("1 0 /", 65535),
        ("0 0 /", 0),
    ]:
        clip = core.std.Expr([base], expression)
        path = out / f"expr-{expression.replace(' ', '').replace('/', 'x')}.png"
        write(clip, path)
        check(
            Png(path).rows == [[want] * 2],
            f"a float frame of '{expression}' is the sample {want} ({Png(path).rows})",
        )
    # An eight bit word is asked for rather than assumed.
    path = out / "gray-at-8.png"
    write(core.std.Expr([base], "0.5"), path, depth=8)
    png = Png(path)
    check(
        png.depth == 8 and png.rows == [[128] * 2],
        f"a float half asked for at eight bits ({png.rows})",
    )
    # r,g,b is the same mapping a channel.
    colour = core.std.BlankClip(
        width=2, height=1, length=1, format=vs.RGBS, color=[1.0, 0.5, 0.0]
    )
    path = out / "rgb.png"
    write(colour, path)
    check(
        Png(path).rows == [[65535, 32768, 0, 65535, 32768, 0]],
        f"a float rgb frame writes every channel ({Png(path).rows})",
    )
    # A float colour clip takes the float alpha a container pairs it with, and
    # a mixed pair is refused because png gives every sample one word.
    path = out / "rgb-alpha.png"
    write(
        colour,
        path,
        alpha=core.std.BlankClip(width=2, height=1, length=1, format=vs.GRAYS, color=0.5),
    )
    check(
        Png(path).rows == [[65535, 32768, 0, 32768, 65535, 32768, 0, 32768]],
        f"a float alpha is interleaved after the colour ({Png(path).rows})",
    )
    message = refused(
        colour,
        out / "mixed-alpha.png",
        alpha=core.std.BlankClip(width=2, height=1, length=1, format=vs.GRAY8, color=128),
    )
    check(
        message is not None and "float" in message,
        f"an integer alpha beside a float colour is refused ({message})",
    )
    # A float yuv frame holds no matrix a writer could read, so it is refused
    # rather than converted under a guess.
    message = refused(
        core.std.BlankClip(
            width=2, height=1, length=1, format=vs.YUV444PS, color=[0.5, 0.5, 0.5]
        ),
        out / "yuv-float.png",
    )
    check(
        message is not None and "float" in message,
        f"a float yuv clip is refused ({message})",
    )


def test_depth() -> None:
    """The depth argument names the word the PNG stores."""
    out = directory("depth")
    # Sixteen bits asked for at eight gives up precision by rounding, and the
    # samples of a real page are what says the rounding is to nearest.
    colour = core.imgseqs.Read(files=[fixture("tiff-gray16.tiff")], prefetch=0)
    with colour.get_frame(0) as frame:
        source = plane_samples(frame, 0)
    path = out / "gray16-at-8.png"
    write(colour, path, depth=8)
    png = Png(path)
    want = [
        [
            round(sample * 255 / 65535)
            for sample in source[row * png.width : (row + 1) * png.width]
        ]
        for row in range(png.height)
    ]
    check(
        png.depth == 8 and png.rows == want,
        "a sixteen bit frame asked for at eight bits is scaled down to it",
    )
    # Eight, ten and twelve bits asked for at sixteen are the exact widening,
    # and the chunk still says how many of those bits are the source's.
    for name, bits in [
        ("png-plain-gray8.png", 8),
        ("jxl-gray10.jxl", 10),
        ("jxl-gray12.jxl", 12),
    ]:
        clip = core.imgseqs.Read(files=[fixture(name)], prefetch=0)
        with clip.get_frame(0) as frame:
            source = plane_samples(frame, 0)
        path = out / f"{name}-at-16.png"
        write(clip, path, depth=16)
        png = Png(path)
        want = [
            [
                widen_to(sample, bits, 16)
                for sample in source[row * png.width : (row + 1) * png.width]
            ]
            for row in range(png.height)
        ]
        check(
            png.depth == 16 and png.rows == want,
            f"{name}: depth=16 is the exact widening of {bits} bits",
        )
        check(
            png.sbit == bytes([bits]),
            f"{name}: sBIT states the {bits} bits of picture ({list(png.sbit or b'')})",
        )
    # A depth below a byte is one gray plane, and a file a low bit reader
    # expanded is packed back into the word it came from, byte for byte.
    for name in ("png-gray1.png", "png-gray2.png", "png-gray4.png"):
        original = Png(FIXTURES / name)
        clip = core.imgseqs.Read(files=[fixture(name)], prefetch=0)
        path = out / f"{name}-repacked.png"
        write(clip, path, depth=original.depth)
        png = Png(path)
        check(
            png.depth == original.depth and png.rows == original.rows,
            f"{name}: the {original.depth} bit picture the reader expanded is packed back",
        )
        check(
            all(
                png.row_bytes(row) == packed_row(original.rows[row], original.depth)
                for row in range(png.height)
            ),
            f"{name}: every row is packed most significant bit first",
        )
        check(png.sbit is None, f"{name}: a packed depth states no sBIT")
    # The depths no png stores, and the shapes it does not pack that way.
    gray = core.std.BlankClip(width=4, height=1, length=1, format=vs.GRAY8, color=0)
    for depth in (0, 3, 5, 6, 7, 9, 15, 17, 32):
        message = raised(
            lambda: core.imgseqs.PNGWrite(
                gray, output_path=str(out / f"bad-{depth}.png"), depth=depth
            )
        )
        check(
            message is not None and "1, 2, 4, 8 or 16" in message,
            f"depth={depth} is refused ({message})",
        )
    for label, clip, arguments in [
        (
            "rgb",
            core.std.BlankClip(
                width=4, height=1, length=1, format=vs.RGB24, color=[0, 0, 0]
            ),
            {},
        ),
        (
            "rgb with alpha",
            gray,
            {
                "alpha": core.std.BlankClip(
                    width=4, height=1, length=1, format=vs.GRAY8, color=255
                )
            },
        ),
        ("yuv", core.imgseqs.Read(files=[fixture("avif-yuv420p.avif")], prefetch=0), {}),
    ]:
        message = refused(
            clip, out / f"packed-{label.split()[0]}.png", depth=1, **arguments
        )
        check(
            message is not None and "gray plane" in message,
            f"a packed depth on a {label} clip is refused ({message})",
        )
    # A float frame is scaled into a packed word by the same rule: the value is
    # a fraction of the scale, so a half is the largest code a bit can hold.
    path = out / "float-at-1.png"
    write(
        core.std.BlankClip(width=4, height=1, length=1, format=vs.GRAYS, color=0.5),
        path,
        depth=1,
    )
    check(
        Png(path).rows == [[1, 1, 1, 1]],
        f"a float half asked for at one bit is the largest code ({Png(path).rows})",
    )


def test_concurrent() -> None:
    """Out of order and simultaneous requests each produce their own file."""
    out = directory("concurrent")
    sequence = core.imgseqs.Read(files=[fixture("png-plain-rgb8.png")] * 4)
    frame = sequence.get_frame(0)
    expected = expected_rows(frame, None, 8)
    frame.close()
    writer = core.imgseqs.PNGWrite(
        sequence, output_path=str(out / "frame%02d.png"), overwrite=True
    )

    def request(index: int) -> bool:
        with writer.get_frame(index) as written:
            return int(written.props["ImgSeqPNGWritePerformed"]) == 1

    with ThreadPoolExecutor(max_workers=4) as pool:
        results = list(pool.map(request, [0, 1, 2, 3]))
    check(
        len(results) == 4,
        "four simultaneous requests returned",
    )
    for index in range(4):
        path = out / f"frame{index:02d}.png"
        check(path.is_file() and Png(path).rows == expected, f"frame {index} is complete")


def main() -> int:
    if not PLUGIN.is_file():
        print(f"FAIL - the plugin is not built at {PLUGIN}")
        return 1
    print(f"validating {PLUGIN}")
    OUTPUT.mkdir(parents=True, exist_ok=True)
    for test in (
        test_roundtrip,
        test_widening,
        test_compression,
        test_metadata,
        test_paths,
        test_repeat,
        test_errors,
        test_yuv,
        test_float,
        test_depth,
        test_concurrent,
    ):
        print(f"--- {test.__name__}")
        test()
    if FAILURES:
        print(f"\n{len(FAILURES)} check(s) failed")
        for failure in FAILURES:
            print(f"  - {failure}")
        return 1
    print("\nall checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
