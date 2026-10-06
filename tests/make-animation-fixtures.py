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


def parse_boxes(data: bytes) -> list[tuple[bytes, bytes]]:
    """Every top level box of `data`, as its kind and its payload."""
    found = []
    at = 0
    while at + 8 <= len(data):
        size = struct.unpack(">I", data[at : at + 4])[0]
        kind = data[at + 4 : at + 8]
        if size < 8:
            raise SystemExit(f"a box of size {size} is not one of these fixtures")
        found.append((kind, data[at + 8 : at + size]))
        at += size
    return found


def pack_box(kind: bytes, payload: bytes) -> bytes:
    return struct.pack(">I", 8 + len(payload)) + kind + payload


def pack_boxes(boxes: list[tuple[bytes, bytes]]) -> bytes:
    return b"".join(pack_box(kind, payload) for kind, payload in boxes)


def replaced(
    boxes: list[tuple[bytes, bytes]], kind: bytes, payload: bytes
) -> list[tuple[bytes, bytes]]:
    """`boxes` with the first box of `kind` holding `payload` instead."""
    out = []
    done = False
    for entry_kind, entry_payload in boxes:
        if entry_kind == kind and not done:
            out.append((kind, payload))
            done = True
        else:
            out.append((entry_kind, entry_payload))
    return out


def moved_chunk_offsets(
    boxes: list[tuple[bytes, bytes]], grown: int
) -> list[tuple[bytes, bytes]]:
    """Every chunk offset table in `boxes`, moved by `grown` bytes."""
    out = []
    for kind, payload in boxes:
        if kind == b"co64":
            raise SystemExit("a 64 bit chunk offset table is not one of these fixtures")
        if kind == b"stco":
            count = struct.unpack(">I", payload[4:8])[0]
            entries = struct.unpack(f">{count}I", payload[8 : 8 + 4 * count])
            payload = payload[:8] + struct.pack(f">{count}I", *[entry + grown for entry in entries])
        out.append((kind, payload))
    return out


def with_composition_offsets(source: Path, path: Path, offsets: list[int]) -> None:
    """Writes `source` with a composition-to-sample box in its first track.

    No encoder here writes one -- `avifenc` states composition times equal to
    decode times and writes no box at all -- so a track that composes its samples
    somewhere else has to be written rather than encoded. The box is version one,
    which is the one that can state a negative offset.

    The movie box sits in front of the media data, so growing it moves every
    chunk the sample tables point at: each track is rebuilt with its own sizes and
    every `stco` entry grows by exactly the box that was inserted. Nothing else
    about the file changes, so its pictures are the ones the source holds.
    """
    payload = bytes((1, 0, 0, 0)) + struct.pack(">I", len(offsets))
    payload += b"".join(struct.pack(">Ii", 1, offset) for offset in offsets)
    inserted = pack_box(b"ctts", payload)
    grown = len(inserted)

    top = parse_boxes(source.read_bytes())
    moov = next(entry for kind, entry in top if kind == b"moov")
    rewritten = []
    first = True
    for kind, track in parse_boxes(moov):
        if kind != b"trak":
            rewritten.append((kind, track))
            continue
        track_boxes = parse_boxes(track)
        mdia = parse_boxes(next(entry for kind, entry in track_boxes if kind == b"mdia"))
        minf = parse_boxes(next(entry for kind, entry in mdia if kind == b"minf"))
        stbl = parse_boxes(next(entry for kind, entry in minf if kind == b"stbl"))
        stbl = moved_chunk_offsets(stbl, grown)
        if first:
            # The spec puts `ctts` after `stts` and before the rest of the table.
            after = next(index for index, (kind, _) in enumerate(stbl) if kind == b"stts") + 1
            stbl.insert(after, (b"ctts", payload))
            first = False
        minf = replaced(minf, b"stbl", pack_boxes(stbl))
        mdia = replaced(mdia, b"minf", pack_boxes(minf))
        rewritten.append((b"trak", pack_boxes(replaced(track_boxes, b"mdia", pack_boxes(mdia)))))
    top = replaced(top, b"moov", pack_boxes(rewritten))
    # The items live in the media data, so their offsets move with it.
    top = moved_items(top, grown)
    path.write_bytes(pack_boxes(top))
    print(f"  + {path.name} ({path.stat().st_size} bytes) ctts {offsets}")


def moved_track_chunks(
    track_boxes: list[tuple[bytes, bytes]], grown: int
) -> list[tuple[bytes, bytes]]:
    """One track's boxes with every chunk offset in its sample table moved."""
    out = []
    for kind, payload in track_boxes:
        if kind == b"mdia":
            mdia = parse_boxes(payload)
            minf = parse_boxes(next(entry for kind, entry in mdia if kind == b"minf"))
            stbl = parse_boxes(next(entry for kind, entry in minf if kind == b"stbl"))
            stbl = moved_chunk_offsets(stbl, grown)
            minf = replaced(minf, b"stbl", pack_boxes(stbl))
            mdia = replaced(mdia, b"minf", pack_boxes(minf))
            payload = pack_boxes(mdia)
        out.append((kind, payload))
    return out


def moved_item_offsets(payload: bytes, grown: int) -> bytes:
    """One `iloc` box's payload with every item base offset moved by `grown`.

    An item's extents are located from its base offset, so an item in the media
    data moves with the media data: the same arithmetic the chunk offsets take,
    applied to the other box that points into the file.
    """
    version = payload[0]
    offset_size = payload[4] >> 4
    length_size = payload[4] & 0xF
    base_size = payload[5] >> 4
    index_size = (payload[5] & 0xF) if version >= 1 else 0
    # A version zero item is an id and a reference index; one and two widen the
    # id and add a reserved word.
    item_header = 4 if version == 0 else 6
    count = struct.unpack(">H", payload[6:8])[0]
    out = bytearray(payload[:8])
    at = 8
    for _ in range(count):
        out += payload[at : at + item_header]
        at += item_header
        if base_size:
            base = int.from_bytes(payload[at : at + base_size], "big") + grown
            out += base.to_bytes(base_size, "big")
            at += base_size
        extents = struct.unpack(">H", payload[at : at + 2])[0]
        out += payload[at : at + 2]
        at += 2
        for _ in range(extents):
            if index_size:
                out += payload[at : at + index_size]
                at += index_size
            if base_size:
                # The extent is located from the item's own base offset, which is
                # the field that moved.
                out += payload[at : at + offset_size + length_size]
            else:
                # With no base offset the extent offset is the file offset itself,
                # so it moves with the media data.
                offset = int.from_bytes(payload[at : at + offset_size], "big") + grown
                out += offset.to_bytes(offset_size, "big")
                out += payload[at + offset_size : at + offset_size + length_size]
            at += offset_size + length_size
    out += payload[at:]
    return bytes(out)


def moved_items(top: list[tuple[bytes, bytes]], grown: int) -> list[tuple[bytes, bytes]]:
    """The top level boxes with every item base offset in `meta` moved."""
    out = []
    for kind, payload in top:
        if kind == b"meta":
            # `meta` is a full box: four bytes of version and flags, then boxes.
            boxes_ = parse_boxes(payload[4:])
            boxes_ = [
                (entry_kind, moved_item_offsets(entry, grown) if entry_kind == b"iloc" else entry)
                for entry_kind, entry in boxes_
            ]
            payload = payload[:4] + pack_boxes(boxes_)
        out.append((kind, payload))
    return out


def with_edit_list(source: Path, path: Path, media_time: int, segment_duration: int) -> None:
    """Writes `source` with its first track's edit list replaced by one edit.

    An edit list is where a file says which part of its media it displays, and the
    entry this writes plays the media from `media_time` for `segment_duration`
    ticks. The box is version one, which is what the encoders here write and the
    one that can state a media time of -1 for an empty edit.

    The box changes size, so the movie box does too and every chunk offset moves
    with it: the arithmetic is [`with_composition_offsets`]'s.
    """
    payload = (
        bytes((1, 0, 0, 0))
        + struct.pack(">I", 1)
        + struct.pack(">QqHH", segment_duration, media_time, 1, 0)
    )
    top = parse_boxes(source.read_bytes())
    moov = next(entry for kind, entry in top if kind == b"moov")
    rewritten = []
    grown = 0
    first = True
    for kind, track in parse_boxes(moov):
        if kind != b"trak":
            rewritten.append((kind, track))
            continue
        track_boxes = parse_boxes(track)
        if first:
            before = len(pack_boxes(track_boxes))
            edts = parse_boxes(next(entry for kind, entry in track_boxes if kind == b"edts"))
            edts = replaced(edts, b"elst", payload)
            track_boxes = replaced(track_boxes, b"edts", pack_boxes(edts))
            grown = len(pack_boxes(track_boxes)) - before
            first = False
        rewritten.append((b"trak", pack_boxes(moved_track_chunks(track_boxes, grown))))
    top = replaced(top, b"moov", pack_boxes(rewritten))
    # The items live in the media data, so their offsets move with it.
    top = moved_items(top, grown)
    path.write_bytes(pack_boxes(top))
    print(
        f"  + {path.name} ({path.stat().st_size} bytes)"
        f" elst media_time={media_time} duration={segment_duration}"
    )


def without_sample_timings(source: Path, path: Path) -> None:
    """Writes `source` with its first track's sample tables emptied.

    That is the `moov` a fragmented file has: its samples are not in the movie box
    at all -- they are in the fragments that follow it -- so the `stts` and `stsz`
    boxes state no samples and the chunk offsets state no chunks. This reader takes
    its timeline from those tables, and libheif takes its own the same way, so a
    fragmented sequence is a movie with nothing to play rather than a sequence
    with a timeline; see the plan's note on fragments.
    """
    stts = bytes(4) + struct.pack(">I", 0)
    stsz = bytes(8) + struct.pack(">I", 0)
    # No chunks either: a fragmented movie box states the samples it does not
    # hold nowhere, so its sample-to-chunk and chunk-offset tables are empty too,
    # which is also what keeps the container consistent for a reader that checks
    # one table against another.
    empty = bytes(4) + struct.pack(">I", 0)
    top = parse_boxes(source.read_bytes())
    moov = next(entry for kind, entry in top if kind == b"moov")
    rewritten = []
    grown = 0
    first = True
    for kind, track in parse_boxes(moov):
        if kind != b"trak":
            rewritten.append((kind, track))
            continue
        track_boxes = parse_boxes(track)
        if first:
            before = len(pack_boxes(track_boxes))
            mdia = parse_boxes(next(entry for kind, entry in track_boxes if kind == b"mdia"))
            minf = parse_boxes(next(entry for kind, entry in mdia if kind == b"minf"))
            stbl = parse_boxes(next(entry for kind, entry in minf if kind == b"stbl"))
            stbl = replaced(stbl, b"stts", stts)
            stbl = replaced(stbl, b"stsz", stsz)
            stbl = replaced(stbl, b"stsc", empty)
            stbl = replaced(stbl, b"stco", empty)
            minf = replaced(minf, b"stbl", pack_boxes(stbl))
            mdia = replaced(mdia, b"minf", pack_boxes(minf))
            track_boxes = replaced(track_boxes, b"mdia", pack_boxes(mdia))
            grown = len(pack_boxes(track_boxes)) - before
            first = False
        rewritten.append((b"trak", pack_boxes(moved_track_chunks(track_boxes, grown))))
    top = replaced(top, b"moov", pack_boxes(rewritten))
    # The items live in the media data, so their offsets move with it.
    top = moved_items(top, grown)
    path.write_bytes(pack_boxes(top))
    print(f"  + {path.name} ({path.stat().st_size} bytes) no sample tables ({grown})")


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

        # A track that states composition offsets presents its samples where the
        # box says rather than where they were decoded. The first sample is
        # composed forty ticks before zero, which the reader clamps, and the
        # second a hundred after its decode time, so the holds become 180, 70, 110
        # and 240 ms rather than 80, 170, 110 and 240.
        with_composition_offsets(
            FIXTURES / "animation.avif",
            FIXTURES / "animation-ctts.avif",
            [-40, 100, 0, 0],
        )

        # An edit list is where a file says which part of its media it displays.
        # The first of these is the identity's shorter cousin: it ends the track
        # at 300 of its 600 ticks. The second starts 250 ticks into the media,
        # which is a leading skip this reader refuses rather than plays.
        with_edit_list(
            FIXTURES / "animation.avif",
            FIXTURES / "animation-elst-short.avif",
            0,
            300,
        )
        with_edit_list(
            FIXTURES / "animation.avif",
            FIXTURES / "animation-elst-skip.avif",
            250,
            600,
        )
        # The `moov` a fragmented file has: its samples are in the fragments after
        # it rather than in the movie box, so the tables state none. libheif has no
        # fragment parsing at all, so there is no timeline for either reader here.
        without_sample_timings(
            FIXTURES / "animation.avif",
            FIXTURES / "animation-fragmented.avif",
        )

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
