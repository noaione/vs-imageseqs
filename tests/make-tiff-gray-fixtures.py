r"""Writes the low-bit grayscale TIFF fixtures into tests/fixtures, and checks them.

    .\.venv\Scripts\python.exe tests/make-tiff-gray-fixtures.py [--check]

It writes `tiff-gray1.tiff`, `tiff-gray1-lzw.tiff`, `tiff-gray1-pred.tiff`,
`tiff-gray1-white.tiff`, `tiff-gray2.tiff` and `tiff-gray4.tiff`, which are
the fixtures `src/formats/tiff.rs`'s narrow gray tests and `tests/readalpha.vpy`'s
`test_narrow_gray_pages` read.

It also writes the gray+alpha pages -- `tiff-graya8.tiff`, `tiff-graya16.tiff`,
`tiff-graya32f.tiff`, `tiff-graya32f-associated.tiff` and the negative
`tiff-graya-noalpha.tiff` -- which are what the gray+alpha tests in
`src/formats/tiff.rs` and `tests/readalpha.vpy` read. They are the same writer
with a second sample interleaved, and the thirty-two bit ones state a
`SampleFormat` of three so the samples are floats.

The candidate table in plan 34 names low-bit grayscale TIFF as needing an
individual decision, and a decision needs files. Pillow has no four bit gray mode
-- and its "1" mode is stored as eight bit gray, so it cannot write a bilevel
TIFF either -- so the container is written here by hand: a minimal uncompressed
TIFF, which is the shape every writer emits.

The one compressed fixture is not written by hand, because a file that says LZW
and holds uncompressed bytes is a lie a reader may or may not forgive. It is the
hand written bilevel page round tripped through libtiff with LZW selected, so the
container is genuine and only the compression came from a writer.

Each fixture is a 16x8 page over every value its width can hold, so a wrong
expansion is a wrong picture rather than a plausible one, and a bilevel fixture
is half black and half white rather than an even spread that could hide a swap.
`--check` prints what Pillow reads, which is what the validator and the unit
tests compare against.
"""

from __future__ import annotations

import hashlib
import pathlib
import struct
import sys

FIXTURES = pathlib.Path("tests/fixtures")

WIDTH, HEIGHT = 16, 8

SHORT, LONG = 3, 4

# ImageWidth, ImageLength, BitsPerSample, Compression, PhotometricInterpretation,
# StripOffsets, SamplesPerPixel, RowsPerStrip, StripByteCounts, Predictor.
# The order matters: a TIFF directory is sorted by tag, and a reader is entitled
# to rely on it.
TAGS = [256, 257, 258, 259, 262, 273, 277, 278, 279, 317]

COMPRESSION_NONE = 1

BLACK_IS_ZERO, WHITE_IS_ZERO = 1, 0

# ImageWidth, ImageLength, BitsPerSample, Compression, PhotometricInterpretation,
# StripOffsets, SamplesPerPixel, RowsPerStrip, StripByteCounts,
# PlanarConfiguration, ExtraSamples, SampleFormat. A directory is sorted by tag.
GRAY_ALPHA_TAGS = [256, 257, 258, 259, 262, 273, 277, 278, 279, 284, 338, 339]

# The one sample twin of a gray+alpha page: the same tags without the extra
# sample, and without a second value on BitsPerSample or SampleFormat.
SAMPLE_TAGS = [256, 257, 258, 259, 262, 273, 277, 278, 279, 339]

UNASSOCIATED_ALPHA, ASSOCIATED_ALPHA = 1, 2
SAMPLE_FORMAT_UINT, SAMPLE_FORMAT_FLOAT = 1, 3

NAMES = [
    "tiff-gray1.tiff",
    "tiff-gray1-lzw.tiff",
    "tiff-gray1-pred.tiff",
    "tiff-gray1-white.tiff",
    "tiff-gray2.tiff",
    "tiff-gray4.tiff",
    "tiff-graya8.tiff",
    "tiff-graya16.tiff",
    "tiff-graya32f.tiff",
    "tiff-graya32f-associated.tiff",
    "tiff-graya-noalpha.tiff",
    "tiff-graya8-gray.tiff",
    "tiff-graya8-alpha.tiff",
    "tiff-graya16-gray.tiff",
    "tiff-graya16-alpha.tiff",
    "tiff-graya32f-gray.tiff",
    "tiff-graya32f-alpha.tiff",
]


def pack(rows: list[list[int]], bits: int) -> bytes:
    """Pack samples most significant bit first, each row padded to a byte."""
    out = bytearray()
    per_byte = 8 // bits
    for row in rows:
        for start in range(0, WIDTH, per_byte):
            byte = 0
            for offset in range(per_byte):
                value = row[start + offset] if start + offset < WIDTH else 0
                byte |= (value & ((1 << bits) - 1)) << (8 - bits * (offset + 1))
            out.append(byte)
    return bytes(out)


def write_tiff(
    path: pathlib.Path,
    bits: int,
    rows: list[list[int]],
    *,
    predictor: int = 1,
    photometric: int = BLACK_IS_ZERO,
) -> pathlib.Path:
    """A minimal little endian TIFF holding one uncompressed strip."""
    raster = pack(rows, bits)
    entries = len(TAGS)
    ifd_offset = 8
    raster_offset = ifd_offset + 2 + entries * 12 + 4

    values = {
        256: (SHORT, WIDTH),
        257: (SHORT, HEIGHT),
        258: (SHORT, bits),
        259: (SHORT, COMPRESSION_NONE),
        262: (SHORT, photometric),
        273: (LONG, raster_offset),
        277: (SHORT, 1),
        278: (SHORT, HEIGHT),
        279: (LONG, len(raster)),
        317: (SHORT, predictor),
    }

    header = b"II" + struct.pack("<HI", 42, ifd_offset)
    directory = struct.pack("<H", entries)
    for tag in TAGS:
        kind, value = values[tag]
        if kind == SHORT:
            directory += struct.pack("<HHIHH", tag, kind, 1, value, 0)
        else:
            directory += struct.pack("<HHII", tag, kind, 1, value)
    directory += struct.pack("<I", 0)

    path.write_bytes(header + directory + raster)
    return path


def gray_alpha_raster(
    gray: list[list[float]], alpha: list[list[float]], bits: int
) -> bytes:
    """The two samples of every pixel, interleaved the way the format stores them."""
    out = bytearray()
    for y in range(HEIGHT):
        for x in range(WIDTH):
            for value in (gray[y][x], alpha[y][x]):
                if bits == 8:
                    out.append(int(value))
                elif bits == 16:
                    out += struct.pack("<H", int(value))
                else:
                    out += struct.pack("<f", float(value))
    return bytes(out)


def gray_alpha_values(bits: int) -> tuple[list[list[float]], list[list[float]]]:
    """The gray ramp and the alpha ramp a gray+alpha fixture holds.

    The gray sample runs along the row and the alpha one down the column, so a
    page whose samples were swapped, or read as one plane, is a wrong picture
    rather than a plausible one.
    """
    if bits == 32:
        gray = [[x / (WIDTH - 1) for x in range(WIDTH)] for _ in range(HEIGHT)]
        alpha = [[y / (HEIGHT - 1) for _ in range(WIDTH)] for y in range(HEIGHT)]
    else:
        top = (1 << bits) - 1
        gray = [[(x * top) // (WIDTH - 1) for x in range(WIDTH)] for _ in range(HEIGHT)]
        alpha = [[(y * top) // (HEIGHT - 1) for _ in range(WIDTH)] for y in range(HEIGHT)]
    return gray, alpha


def write_gray_alpha_tiff(
    path: pathlib.Path,
    bits: int,
    gray: list[list[float]],
    alpha: list[list[float]],
    *,
    extra: int = UNASSOCIATED_ALPHA,
) -> pathlib.Path:
    """A minimal little endian TIFF holding one gray page with an alpha sample."""
    raster = gray_alpha_raster(gray, alpha, bits)
    entries = len(GRAY_ALPHA_TAGS)
    ifd_offset = 8
    raster_offset = ifd_offset + 2 + entries * 12 + 4
    sample_format = SAMPLE_FORMAT_FLOAT if bits == 32 else SAMPLE_FORMAT_UINT
    values = {
        256: (SHORT, [WIDTH]),
        257: (SHORT, [HEIGHT]),
        258: (SHORT, [bits, bits]),
        259: (SHORT, [COMPRESSION_NONE]),
        262: (SHORT, [BLACK_IS_ZERO]),
        273: (LONG, [raster_offset]),
        277: (SHORT, [2]),
        278: (SHORT, [HEIGHT]),
        279: (LONG, [len(raster)]),
        284: (SHORT, [1]),
        338: (SHORT, [extra]),
        339: (SHORT, [sample_format, sample_format]),
    }

    return write_directory(path, GRAY_ALPHA_TAGS, values, raster)


def write_directory(
    path: pathlib.Path, tags: list[int], values: dict, raster: bytes
) -> pathlib.Path:
    """A one strip little endian TIFF: the header, the directory, then the raster.

    A SHORT of one or two values fits inside the four byte value field, with the
    values first and the rest padded, as the format stores them; every other tag
    here is one LONG. A directory is sorted by tag, which `tags` is.
    """
    entries = len(tags)
    ifd_offset = 8
    header = b"II" + struct.pack("<HI", 42, ifd_offset)
    directory = struct.pack("<H", entries)
    for tag in tags:
        kind, numbers = values[tag]
        if kind == SHORT:
            packed = b"".join(struct.pack("<H", number) for number in numbers)
            directory += struct.pack("<HHI", tag, kind, len(numbers))
            directory += packed.ljust(4, b"\x00")
        else:
            directory += struct.pack("<HHII", tag, kind, 1, numbers[0])
    directory += struct.pack("<I", 0)

    path.write_bytes(header + directory + raster)
    return path


def write_sample_tiff(
    path: pathlib.Path,
    bits: int,
    rows: list[list[float]],
    *,
    sample_format: int = SAMPLE_FORMAT_UINT,
) -> pathlib.Path:
    """A minimal little endian TIFF holding one gray page of whole byte samples.

    This is the one sample twin of a gray+alpha page. Pillow reads a page of one
    sample at every width and a page of two only at eight bits, so the twins are
    what an independent decoder can check the two samples against.
    """
    out = bytearray()
    for y in range(HEIGHT):
        for x in range(WIDTH):
            value = rows[y][x]
            if bits == 8:
                out.append(int(value))
            elif bits == 16:
                out += struct.pack("<H", int(value))
            else:
                out += struct.pack("<f", float(value))
    raster = bytes(out)
    entries = len(SAMPLE_TAGS)
    raster_offset = 8 + 2 + entries * 12 + 4
    values = {
        256: (SHORT, [WIDTH]),
        257: (SHORT, [HEIGHT]),
        258: (SHORT, [bits]),
        259: (SHORT, [COMPRESSION_NONE]),
        262: (SHORT, [BLACK_IS_ZERO]),
        273: (LONG, [raster_offset]),
        277: (SHORT, [1]),
        278: (SHORT, [HEIGHT]),
        279: (LONG, [len(raster)]),
        339: (SHORT, [sample_format]),
    }
    return write_directory(path, SAMPLE_TAGS, values, raster)

def ramp(bits: int) -> list[list[int]]:
    """Every value the width can hold, cycling so one row holds several."""
    values = [v * 255 // ((1 << bits) - 1) for v in range(1 << bits)]
    return [[values[(x + y) % len(values)] for x in range(WIDTH)] for y in range(HEIGHT)]


def bilevel() -> list[list[int]]:
    """Half black and half white, which is what a fax page is."""
    return [[1 if x < WIDTH // 2 else 0 for x in range(WIDTH)] for _ in range(HEIGHT)]


def build() -> None:
    write_tiff(FIXTURES / "tiff-gray1.tiff", 1, bilevel())
    write_tiff(FIXTURES / "tiff-gray1-pred.tiff", 1, bilevel(), predictor=2)
    write_tiff(FIXTURES / "tiff-gray1-white.tiff", 1, bilevel(), photometric=WHITE_IS_ZERO)
    write_tiff(FIXTURES / "tiff-gray2.tiff", 2, ramp(2))
    write_tiff(FIXTURES / "tiff-gray4.tiff", 4, ramp(4))

    # Gray pages whose second sample is alpha, at the three widths a sample can
    # be. The eight bit one is the only one Pillow reads -- its table names
    # `(II, 1, (1,), 1, (8, 8), (2,))` and nothing else -- so that one is
    # associated alpha and the rest are ImageMagick's, which reads all four.
    # The thirty-two bit pair is once unassociated and once associated, because
    # the tag is the part that must not be guessed.
    gray8, alpha8 = gray_alpha_values(8)
    write_gray_alpha_tiff(
        FIXTURES / "tiff-graya8.tiff", 8, gray8, alpha8, extra=ASSOCIATED_ALPHA
    )
    gray16, alpha16 = gray_alpha_values(16)
    write_gray_alpha_tiff(
        FIXTURES / "tiff-graya16.tiff", 16, gray16, alpha16,
        extra=ASSOCIATED_ALPHA,
    )
    gray32, alpha32 = gray_alpha_values(32)
    write_gray_alpha_tiff(FIXTURES / "tiff-graya32f.tiff", 32, gray32, alpha32)
    write_gray_alpha_tiff(
        FIXTURES / "tiff-graya32f-associated.tiff", 32, gray32, alpha32,
        extra=ASSOCIATED_ALPHA,
    )
    # The one sample twins of every gray+alpha page. Pillow reads a page of one
    # sample at every width and a page of two only at eight bits, so the twins
    # are what an independent decoder can check each sample of a two sample page
    # against.
    write_sample_tiff(FIXTURES / "tiff-graya8-gray.tiff", 8, gray8)
    write_sample_tiff(FIXTURES / "tiff-graya8-alpha.tiff", 8, alpha8)
    write_sample_tiff(FIXTURES / "tiff-graya16-gray.tiff", 16, gray16)
    write_sample_tiff(FIXTURES / "tiff-graya16-alpha.tiff", 16, alpha16)
    write_sample_tiff(
        FIXTURES / "tiff-graya32f-gray.tiff", 32, gray32,
        sample_format=SAMPLE_FORMAT_FLOAT,
    )
    write_sample_tiff(
        FIXTURES / "tiff-graya32f-alpha.tiff", 32, alpha32,
        sample_format=SAMPLE_FORMAT_FLOAT,
    )
    # The negative case: two samples whose extra sample does not say it is
    # alpha, which this reader refuses rather than guessing at.
    write_gray_alpha_tiff(
        FIXTURES / "tiff-graya-noalpha.tiff", 8, gray8, alpha8, extra=0
    )

    # The compressed one is libtiff's, because only a real compressor can write
    # a real compressed stream.
    from PIL import Image

    page = Image.open(FIXTURES / "tiff-gray1.tiff")
    page.save(FIXTURES / "tiff-gray1-lzw.tiff", compression="tiff_lzw")

def check() -> None:
    from PIL import Image

    for name in NAMES:
        path = FIXTURES / name
        try:
            image = Image.open(path)
        except Exception as error:  # a shape Pillow's table does not name
            print(f"{name}: pillow will not open it ({error.__class__.__name__})")
            continue
        bands = image.getbands()
        if len(bands) > 1:
            # A gray page with alpha is two bands, and converting it to "L"
            # would drop the one this fixture is about.
            raw = image.tobytes()
            print(
                f"{name}: pillow {image.mode} {image.size} bands={bands}"
                f" bits={image.tag_v2.get(258)} extra={image.tag_v2.get(338)}"  # pyright: ignore[reportAttributeAccessIssue]
                f" format={image.tag_v2.get(339)}"  # pyright: ignore[reportAttributeAccessIssue]
                f" {hashlib.sha256(raw).hexdigest()[:16]} bytes={len(raw)}"
            )
            for index, band in enumerate(bands):
                values = list(image.getdata(index))
                print(f"  {band}: first/last={values[0]}/{values[WIDTH - 1]} second={values[1]}")
            continue
        gray = image.convert("L")
        digest = hashlib.sha256(bytes(gray.tobytes())).hexdigest()[:16]
        corners = [gray.getpixel((0, 0)), gray.getpixel((WIDTH - 1, 0))]
        print(
            f"{name}: pillow {image.mode} {gray.size}"
            f" bits={image.tag_v2.get(258)} comp={image.tag_v2.get(259)}"  # pyright: ignore[reportAttributeAccessIssue]
            f" photo={image.tag_v2.get(262)} {digest} first/last={corners}"  # pyright: ignore[reportAttributeAccessIssue]
        )


if __name__ == "__main__":
    if "--check" not in sys.argv:
        build()
    check()
