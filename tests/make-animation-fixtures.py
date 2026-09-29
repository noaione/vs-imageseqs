#!/usr/bin/env python3
"""Generate small animation fixtures for the animated-image source tests.

Requires Pillow, cjxl, avifenc, and heif-enc on PATH. The script writes only
the final animations into tests/fixtures; intermediate PNGs live in a temporary
directory.
"""

from __future__ import annotations

import shutil
import struct
import subprocess
import tempfile
import zlib
from binascii import crc32
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent
FIXTURES = ROOT / "fixtures"
SIZE = (16, 12)
COLORS = [
    (235, 55, 45, 255),
    (45, 190, 85, 190),
    (45, 100, 235, 255),
    (245, 200, 35, 120),
]
DURATIONS_MS = [80, 170, 110, 240]


def require_tool(name: str) -> str:
    path = shutil.which(name)
    if path is None:
        raise SystemExit(f"required encoder not found on PATH: {name}")
    return path


def make_frames(directory: Path) -> list[Image.Image]:
    frames: list[Image.Image] = []
    for index, color in enumerate(COLORS):
        frame = Image.new("RGBA", SIZE, (0, 0, 0, 0))
        draw = ImageDraw.Draw(frame)
        # Each presentation has a stable background marker and a moving block.
        # The transparent gaps make alpha handling observable after compositing.
        draw.rectangle((0, 0, 3, 3), fill=(20 + index * 50, 25, 30, 255))
        x = 2 + index * 3
        draw.rectangle((x, 3, x + 5, 9), fill=color)
        draw.point((15, 11), fill=(255, 255, 255, 255))
        frames.append(frame)
        frame.save(directory / f"frame-{index:02}.png")
    return frames


def png_chunk(kind: bytes, data: bytes) -> bytes:
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", crc32(body) & 0xFFFFFFFF)


def write_rgba16_apng(path: Path) -> None:
    """Write a two-frame 16-bit RGBA APNG using only the Python standard lib."""
    width, height = 4, 3
    frames = [
        [(0x1234, 0x2345, 0x3456, 0xFFFF) if (x + y) % 2 == 0 else (0, 0, 0, 0)
         for y in range(height) for x in range(width)],
        [(0xABCD, 0xBCDE, 0xCDEF, 0x8000) if x >= 2 else (0, 0, 0, 0)
         for y in range(height) for x in range(width)],
    ]

    def compressed_frame(pixels: list[tuple[int, int, int, int]]) -> bytes:
        rows = bytearray()
        for y in range(height):
            rows.append(0)  # PNG filter: None
            for rgba in pixels[y * width:(y + 1) * width]:
                rows.extend(struct.pack(">HHHH", *rgba))
        return zlib.compress(rows)

    output = bytearray(b"\x89PNG\r\n\x1a\n")
    output += png_chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 16, 6, 0, 0, 0))
    output += png_chunk(b"acTL", struct.pack(">II", len(frames), 0))
    sequence = 0
    for index, pixels in enumerate(frames):
        control = struct.pack(">IIIIIHHBB", sequence, width, height, 0, 0, 100 + index * 100, 1000, 0, 0)
        output += png_chunk(b"fcTL", control)
        sequence += 1
        compressed = compressed_frame(pixels)
        if index == 0:
            output += png_chunk(b"IDAT", compressed)
        else:
            output += png_chunk(b"fdAT", struct.pack(">I", sequence) + compressed)
            sequence += 1
    output += png_chunk(b"IEND", b"")
    path.write_bytes(output)


def main() -> None:
    cjxl = require_tool("cjxl")
    avifenc = require_tool("avifenc")
    heif_enc = require_tool("heif-enc")
    FIXTURES.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="imgseqs-animation-") as temp:
        work = Path(temp)
        frames = make_frames(work)
        frame_paths = [work / f"frame-{index:02}.png" for index in range(len(frames))]

        # Pillow writes full-canvas frames with explicit per-frame timing,
        # blend, and disposal metadata. Alpha varies between presentations.
        frames[0].save(
            FIXTURES / "animation.gif",
            save_all=True,
            append_images=frames[1:],
            duration=DURATIONS_MS,
            loop=0,
            disposal=[1, 2, 3, 1],
            transparency=0,
            optimize=False,
        )
        frames[0].save(
            FIXTURES / "animation.png",
            save_all=True,
            append_images=frames[1:],
            duration=DURATIONS_MS,
            loop=0,
            disposal=[0, 1, 2, 0],
            blend=[0, 1, 1, 0],
            optimize=False,
        )
        frames[0].save(
            FIXTURES / "animation.webp",
            save_all=True,
            append_images=frames[1:],
            duration=DURATIONS_MS,
            loop=0,
            lossless=True,
            quality=100,
            method=6,
        )
        write_rgba16_apng(FIXTURES / "animation-rgba16.png")

        # cjxl reads the APNG as a timed animation and writes a lossless JXL.
        subprocess.run(
            [cjxl, str(FIXTURES / "animation.png"), str(FIXTURES / "animation.jxl"), "-d", "0"],
            check=True,
        )

        # Sequence encoders use a 1000 Hz timeline and the same 4 source frames.
        # avifenc applies each duration to following inputs, so interleave each
        # duration and its frame explicitly rather than using encode_sequence.
        avif_command = [avifenc]
        for duration, frame in zip(DURATIONS_MS, frame_paths, strict=True):
            avif_command.extend(["--duration", str(duration), str(frame)])
        avif_command.extend([
            "--timescale",
            "1000",
            "--repetition-count",
            "0",
            "--lossless",
            str(FIXTURES / "animation.avif"),
        ])
        subprocess.run(avif_command, check=True)

        # libheif's CLI assigns one duration to every sequence frame. Keep its
        # total duration at 600 ms; AVIF covers varying per-frame durations.
        # The numbered filename pattern makes it read all four frames in order.
        sequence_pattern = str(work / "frame-00.png")
        subprocess.run(
            [
                heif_enc,
                "--sequence",
                "--timebase",
                "1000",
                "--duration",
                "150",
                "--repetitions",
                "1",
                "--lossless",
                sequence_pattern,
                "-o",
                str(FIXTURES / "animation.heic"),
            ],
            check=True,
        )


if __name__ == "__main__":
    main()
