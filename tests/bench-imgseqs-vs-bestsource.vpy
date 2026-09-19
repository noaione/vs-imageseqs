"""Compares imgseqs with bestsource reading the same image sequence.

Usage from the repository root, with the repository's virtual environment
active:

    python tests/bench-imgseqs-vs-bestsource.vpy
    python tests/bench-imgseqs-vs-bestsource.vpy --frames 10 --extra

Every configuration reads the same ordered files once per repetition and every
``get_frame`` is timed on its own, so the table shows where the time goes per
image and the totals show how the lookahead pools compare. The order of the
configurations is reversed on every other repetition so a busy period of the
machine cannot consistently favour one of them, and each clip is rebuilt for
every repetition so no decoded frame survives from the previous pass.

bestsource is opened with ``cachemode=0`` (never read or write an index on
disk), ``apply_rotation=False`` (imgseqs records the orientation but never
transforms the picture) and no fps override. Do not pass ``fpsnum``/``fpsden``
to it: ffmpeg opens image sequences at 25 fps, so asking for a different rate
makes it resample and silently drop every 25th image (163 files -> 156
frames). Its ``cachesize``, ``threads`` and ``maxdecoders`` stay at the plugin
defaults. Note that opening a bestsource clip indexes the sequence, which for
images means reading every file, so its ``open`` cost is not free.

bestsource can only be pointed at a printf pattern, so reading a part of a
long sequence stages hardlinks of the selected files in ``target/bench/`` and
hands those same files to imgseqs.

a configuration that cannot read the format at all, for example bestsource on
jxl, is reported and left out of the tables instead of aborting the run. the
measured results live in ``docs/BENCH.md``.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import statistics
import time
from pathlib import Path
from typing import Callable, Optional

import vapoursynth as vs
from natsort import natsorted

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_PLUGIN = ROOT / "target" / "release" / "vs_imageseqs.dll"
DEFAULT_PATTERN = "i_%04d.webp"

core = vs.core


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dir", type=Path, help="directory holding the images")
    parser.add_argument("--pattern", default=DEFAULT_PATTERN, help="printf pattern bestsource opens")
    parser.add_argument("--select", help="glob used to pick the files, defaults to the glob of --pattern")
    parser.add_argument("--plugin", type=Path, default=DEFAULT_PLUGIN, help="imgseqs library to load")
    parser.add_argument("--frames", type=int, default=0, help="images to read, 0 reads the whole sequence")
    parser.add_argument("--reps", type=int, default=2, help="timed passes per configuration")
    parser.add_argument("--extra", action="store_true", help="also run imgseqs without lookahead and bestsource with one thread")
    parser.add_argument("--prefetch", type=int, default=None, help="also run imgseqs with this many lookahead workers")
    return parser.parse_args()


def glob_for(pattern: str) -> str:
    """The glob that matches the files a printf pattern numbers."""
    return re.sub(r"%0?\d*d", "*", pattern)


def matching_files(directory: Path, glob: str) -> list[Path]:
    """The sequence files, ordered the way the numbers in their names go."""
    images = natsorted(directory.glob(glob), key=lambda path: path.stem)
    if not images:
        raise SystemExit(f"no files match {glob!r} in {directory}")
    return images


def first_number(files: list[Path], pattern: str) -> Optional[int]:
    """The number of the first file, or None when the pattern cannot say it.

    A selection that mixes extensions, for example jpeg and png in one folder,
    cannot be described by a single pattern, so bestsource has to sit that run
    out.
    """
    for candidate in range(10):
        if pattern % candidate == files[0].name:
            return candidate
    return None


def staged_files(files: list[Path], pattern: str, staging: Path) -> list[Path]:
    """Link the selected files into a directory holding only those files.

    bestsource takes a printf pattern rather than a file list, so a partial run
    needs numbering that stops at the last file to read. Hardlinks cost no disk
    space and the fall back covers a staging directory on another volume.
    """
    if staging.exists():
        for stale in staging.iterdir():
            stale.unlink()
    staging.mkdir(parents=True, exist_ok=True)

    links = []
    for number, path in enumerate(files):
        link = staging / (pattern % number)
        try:
            os.link(path, link)
        except OSError:
            shutil.copy2(path, link)
        links.append(link)
    return links


def open_imgseqs(files: list[Path], prefetch: Optional[int]) -> vs.VideoNode:
    arguments: dict = {"files": [str(path) for path in files], "mismatch": True}
    if prefetch is not None:
        arguments["prefetch"] = prefetch
    return core.imgseqs.Read(**arguments)


def open_bestsource(directory: Path, pattern: str, first: int, threads: Optional[int]) -> vs.VideoNode:
    arguments: dict = {
        "source": str(directory / pattern),
        "start_number": first,
        "cachemode": 0,
        "apply_rotation": False,
    }
    if threads is not None:
        arguments["threads"] = threads
    return core.bs.VideoSource(**arguments)


class Config:
    """One way of reading the sequence, named by what it changes."""

    def __init__(self, label: str, open_clip: Callable[[], vs.VideoNode]) -> None:
        self.label = label
        self.open_clip = open_clip
        self.runs: list[list[float]] = []
        self.open_times: list[float] = []
        self.widths: list[int] = []
        self.frames = 0
        self.info = ""
        self.failure: Optional[str] = None

    @property
    def best_run(self) -> list[float]:
        return min(self.runs, key=sum)

    def measure(self) -> None:
        """Time one pass, with the clip built outside the timed section."""
        started = time.perf_counter()
        clip = self.open_clip()
        self.open_times.append((time.perf_counter() - started) * 1000.0)
        self.frames = clip.num_frames
        if not self.info:
            self.info = f"{clip.format.name} {clip.width}x{clip.height} {clip.num_frames}f"

        times = []
        for index in range(clip.num_frames):
            started = time.perf_counter()
            frame = clip.get_frame(index)
            times.append((time.perf_counter() - started) * 1000.0)
            if not self.runs:
                self.widths.append(frame.width)
        self.runs.append(times)
        del clip


def check_alignment(configs: list[Config]) -> None:
    """Confirm every configuration read the same frames in the same order.

    bestsource decodes to a subsampled format, so its width is the even width
    below the one imgseqs reports for the same file.
    """
    reference = configs[0]
    for config in configs[1:]:
        if config.frames != reference.frames:
            print(f"warning: {config.label} returned {config.frames} frames, {reference.label} returned {reference.frames}")
            continue
        differing = [
            index
            for index in range(reference.frames)
            if config.widths[index] // 2 != reference.widths[index] // 2
        ]
        if differing:
            print(f"warning: {config.label} and {reference.label} disagree on the width of frames {differing[:10]}")
        else:
            print(f"{config.label} read the same {config.frames} frames as {reference.label}")


def main() -> None:
    args = parse_args()
    core.std.LoadPlugin(str(args.plugin.resolve()))

    selection = args.select or glob_for(args.pattern)
    images = matching_files(args.dir, selection)
    files = images if args.frames <= 0 else images[: args.frames]
    if len(files) != len(images):
        staging = ROOT / "target" / "bench" / "bs-seq"
        print(f"staging the first {len(files)} of {len(images)} files as hardlinks in {staging}")
        files = staged_files(files, args.pattern, staging)
    first = first_number(files, args.pattern)
    directory = files[0].parent

    configs = [Config("imgseqs", lambda: open_imgseqs(files, None))]
    if first is None:
        print(f"{args.pattern!r} does not describe {files[0].name}, so bestsource is left out")
    else:
        configs.append(Config("bestsource", lambda: open_bestsource(directory, args.pattern, first, None)))
    if args.extra:
        configs.append(Config("imgseqs p=0", lambda: open_imgseqs(files, 0)))
        if first is not None:
            configs.append(Config("bestsource t=1", lambda: open_bestsource(directory, args.pattern, first, 1)))
    if args.prefetch is not None:
        workers = args.prefetch
        configs.append(Config(f"imgseqs p={workers}", lambda: open_imgseqs(files, workers)))

    bestsource = core.bs.VideoSource.plugin
    start = "" if first is None else f", first number {first}"
    print(f"sequence: {directory / selection} ({len(files)} files{start})")
    print(f"imgseqs: {args.plugin.resolve()}")
    print(f"bestsource: {bestsource.identifier} {bestsource.name} r{bestsource.version.major}")
    print()

    for repetition in range(args.reps):
        order = configs if repetition % 2 == 0 else list(reversed(configs))
        for config in order:
            if config.failure:
                continue
            try:
                config.measure()
            except vs.Error as error:
                # A plugin that cannot read the format at all, for example
                # bestsource on jxl, should not hide the other numbers.
                config.failure = str(error)
                print(f"  {config.label} cannot read this sequence: {error}")
                continue
            print(f"  rep {repetition + 1} {config.label}: {sum(config.runs[-1]) / 1000.0:.3f} s")
    print()

    usable = [config for config in configs if not config.failure]
    if not usable:
        raise SystemExit("no configuration could read this sequence")
    if len(usable) < len(configs):
        print("only " + ", ".join(config.label for config in usable) + " read this sequence")
    for config in usable:
        print(f"{config.label:<16} {config.info} | open {min(config.open_times):.1f} ms")
    print()
    check_alignment(usable)

    count = min(config.frames for config in usable)
    width = max(len("frame"), *(len(config.label) for config in usable))
    print()
    print(f"per frame timings in ms ({count} frames)")
    print(f"{'frame':>{width}}" + "".join(f"{config.label:>{width + 2}}" for config in usable))
    for index in range(count):
        row = f"{index:>{width}}"
        row += "".join(f"{config.best_run[index]:>{width + 2}.2f}" for config in usable)
        print(row)

    print()
    best = {config.label: config.best_run[:count] for config in usable}
    summary = [
        ("total s", lambda config: f"{sum(best[config.label]) / 1000.0:.3f}"),
        ("open s", lambda config: f"{min(config.open_times) / 1000.0:.3f}"),
        ("open+total s", lambda config: f"{(sum(best[config.label]) + min(config.open_times)) / 1000.0:.3f}"),
        ("median ms", lambda config: f"{statistics.median(best[config.label]):.2f}"),
        ("min ms", lambda config: f"{min(best[config.label]):.2f}"),
        ("max ms", lambda config: f"{max(best[config.label]):.2f}"),
    ]
    for name, value in summary:
        print(f"{name:>{width}}" + "".join(f"{value(config):>{width + 2}}" for config in usable))

    print()
    for config in usable:
        print(f"{config.label:<{width}} reps: " + " ".join(f"{sum(run[:count]) / 1000.0:.3f}" for run in config.runs))

    if len(usable) < 2:
        return

    reference = usable[1]
    faster = usable[0]
    print()
    for name, seconds in (
        ("frames", lambda config: sum(best[config.label]) / 1000.0),
        ("with open", lambda config: (sum(best[config.label]) + min(config.open_times)) / 1000.0),
    ):
        mine, theirs = seconds(faster), seconds(reference)
        winner, loser, ratio = (faster, reference, theirs / mine) if mine <= theirs else (reference, faster, mine / theirs)
        print(f"{winner.label} is {ratio:.2f}x faster than {loser.label} ({name})")


if __name__ == "__main__":
    main()
