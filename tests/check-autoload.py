"""Run in a clean environment after installing the wheel, with autoload enabled."""

import sys
from pathlib import Path

import vapoursynth as vs

filename = {
    "win32": "vs_imageseqs.dll",
    "darwin": "libvs_imageseqs.dylib",
}.get(sys.platform, "libvs_imageseqs.so")
directory = Path(vs.get_plugin_dir()) / "imageseqs"  # pyright: ignore[reportAttributeAccessIssue]
assert (directory / "manifest.vs").read_text().splitlines() == [
    "[VapourSynth Manifest V1]", Path(filename).stem,
]
assert Path(vs.core.imgseqs.plugin_path).resolve() == (directory / filename).resolve()
fixture = Path(__file__).parent / "fixtures" / "alpha-rgb8.png"
frame = vs.core.imgseqs.Read(files=[str(fixture.resolve())], prefetch=0).get_frame(0)
assert frame.width > 0 and frame.height > 0
print(f"manifest autoload and frame decode passed: {directory / filename}")
