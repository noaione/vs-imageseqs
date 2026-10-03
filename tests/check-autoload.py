"""Run in a clean environment after installing the wheel, with autoload enabled."""

import hashlib
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
loaded = Path(vs.core.imgseqs.plugin_path).resolve()
library = Path(filename)
assert loaded in {
    (directory / f"{library.stem}{variant}{library.suffix}").resolve()
    for variant in ("", ".avx2", ".avx512")
}
assert loaded.is_file()
fixture = Path(__file__).parent / "fixtures" / "alpha-rgb8.png"
frame = vs.core.imgseqs.Read(files=[str(fixture.resolve())], prefetch=0).get_frame(0)
assert frame.width > 0 and frame.height > 0
print(f"manifest autoload and frame decode passed: {loaded}")

# Still AVIF uses dav1d directly, while an AVIF sequence needs libheif's own
# AV1 backend. Decode every output of the installed wheel so a build that
# omitted that backend cannot pass by reading only a still PNG.
animation = Path(__file__).parent / "fixtures" / "animation.avif"
outputs = vs.core.imgseqs.ReadAlpha(files=[str(animation.resolve())], prefetch=0)
colour, alpha = outputs["clip"], outputs["alpha"]
assert colour.num_frames == alpha.num_frames == 14
assert (colour.width, colour.height) == (16, 12)
hashes = []
for index in range(colour.num_frames):
    frame = colour.get_frame(index)
    pixels = b"".join(
        frame[plane].tobytes() for plane in range(frame.format.num_planes)
    )
    hashes.append(hashlib.sha256(pixels).digest())
    alpha_frame = alpha.get_frame(index)
    assert (alpha_frame.width, alpha_frame.height) == (16, 12)
assert len({hashes[index] for index in (0, 2, 6, 9)}) == 4
print("installed plugin AVIF sequence color and alpha decode passed")
