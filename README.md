# vapoursynth-imageseqs

[![License](https://img.shields.io/github/license/noaione/vs-imageseqs)](https://github.com/noaione/vs-imageseqs/blob/master/LICENSE)
![VapourSynth version](https://img.shields.io/badge/vapoursynth-%3E%3DR79-blue)
[![Build](https://github.com/noaione/vs-imageseqs/actions/workflows/build.yml/badge.svg)](https://github.com/noaione/vs-imageseqs/actions/workflows/build.yml)
[![Rust tests](https://github.com/noaione/vs-imageseqs/actions/workflows/rust-tests.yml/badge.svg)](https://github.com/noaione/vs-imageseqs/actions/workflows/rust-tests.yml)

A native VapourSynth plugin for reading ordered image sequences and writing
PNG frames. File types are detected from their contents, so filenames do not
need the matching extension.

## install

Install the platform-specific wheel:

```console
python -m pip install vapoursynth-imageseqs
```

You need VapourSynth R79 or newer. The wheel installs the plugin and manifest
in VapourSynth's plugin directory; it does not install VapourSynth or provide
a Python import package.

You can also download a plugin ZIP from [Releases](https://github.com/noaione/vs-imageseqs/releases)
or the [build workflow](https://github.com/noaione/vs-imageseqs/actions/workflows/build.yml).
Copy its `imageseqs` folder into your VapourSynth plugin directory. If the
plugin does not load automatically, load the library directly:

```python
core.std.LoadPlugin(r"path/to/plugin-library")
```

## read images

Pass paths in the order you want them in the clip:

```python
from pathlib import Path
import vapoursynth as vs

core = vs.core
files = sorted(Path(r"I:\images").glob("*.png"))

clip = core.imgseqs.Read(files=[str(path) for path in files])
```

Each still image contributes one frame. `Read` returns the color clip.
`ReadAlpha` returns both the color clip and a separate gray alpha clip. Images
without alpha get a fully opaque alpha frame:

```python
result = core.imgseqs.ReadAlpha(files=[str(path) for path in files])
clip = result["clip"]
alpha = result["alpha"]
```

### `Read` and `ReadAlpha` options

| option | default | description |
| --- | --- | --- |
| `files` | required | Image paths, in frame order. |
| `fpsnum`, `fpsden` | `24`, `1` | Output frame rate. |
| `mismatch` | `False` | Allow frames with different sizes or pixel formats. |
| `apply_rotation` | `True` | Apply the orientation stored in each image. Some orientations swap width and height. |
| `icc_profile` | `False` | Copy embedded ICC bytes to the `ICCProfile` frame property. Pixels are not color transformed. |
| `prefetch` | automatic, up to 4 workers | Background workers for upcoming frames. Set to `0` to disable lookahead. |
| `prefetch_memory` | automatic, at least 192 MiB | Memory budget for prefetched frames, in MiB. Must be positive. |
| `debug` | `False` | Write timing information to the VapourSynth log. |

By default, all frames must have the same size and pixel format. Set
`mismatch=True` to allow variation; downstream filters may still need a
consistent clip, so convert it before using those filters.

### supported formats

PNG (including APNG), JPEG, BMP, GIF, ICO, TIFF, DDS, Farbfeld, WebP, AVIF,
HEIF/HEIC, JPEG XL, JPEG 2000, EXR, HDR, PNM, QOI, and TGA.

Animated GIF, APNG, WebP, JPEG XL, AVIF, and HEIF/HEIC contribute their
displayed timeline at the requested frame rate. Each file plays once; its loop
count is ignored. A picture held across multiple output ticks appears in each
corresponding frame.

Gray and RGB images keep their supported bit depth, including depths from 9 to
15 bits. Compatible WebP, AVIF, HEIF/HEIC, and JPEG 2000 images may be returned
as YUV planes instead of RGB. Monochrome images are returned as gray.

### color and metadata

The plugin applies image orientation by default and reports the source's color
metadata through VapourSynth frame properties. It does not apply ICC color
transforms. Use `icc_profile=True` to copy the raw profile into `ICCProfile`;
`ImgSeqHasICC` reports whether a profile exists either way.

You can then utilize [VapourSynth-ICCConvert](https://github.com/YomikoR/VapourSynth-ICCConvert)
to do the ICC color transform.

VapourSynth resize filters can convert YUV clips to RGB using the frame's color
properties:

```python
rgb = core.resize.Bicubic(clip, format=vs.RGB24)
```

For clips without matrix metadata, provide the correct matrix to the resize
filter. See the [VapourSynth resize documentation](https://www.vapoursynth.com/doc/functions/video/resize.html).

Useful source properties include `ImgSeqPath`, `ImgSeqIndex`,
`ImgSeqAnimationIndex` (for animated files), `ImgSeqOrientation`, and
`ImgSeqHasICC`. Standard color properties are set when the file provides
values VapourSynth supports.

## write PNGs

`PNGWrite` creates a node with the same frames as its input clip. Requesting a
frame writes it as a PNG; creating the node alone writes no PNG:

```python
writer = core.imgseqs.PNGWrite(
    clip,
    output_path=r"exports\frame%04d.png",
)

for n in range(writer.num_frames):
    with writer.get_frame(n):
        pass
```

To keep each source file's own name, give `output_dir` instead of
`output_path`:

```python
writer = core.imgseqs.PNGWrite(clip, output_dir=r"exports")
```

The directory is created recursively. `output_path` and `output_dir` are
mutually exclusive.

Use a literal path for a one-frame clip, or a numbered path such as `%d`,
`%04d`, or `%06d` for a longer clip. `%%` writes a literal percent sign. The
`output_path` directory must already exist. With `output_dir`, a still is
written as `<stem>.png` from `ImgSeqPath`; an animated file's output frames
use the writer's own frame index, `<stem>-0000.png`, `<stem>-0001.png`, and so
on, because the picture index repeats while a picture is held.

| option | default | description |
| --- | --- | --- |
| `clip` | required | Gray, RGB, or YUV integer frames from 8 to 16 bits, or float gray/RGB. |
| `output_path` | `None` | One filename or a numbered filename template. Required unless `output_dir` is given. |
| `output_dir` | `None` | Directory to write into; a still is named from `ImgSeqPath`, an animated frame from the writer's index; created recursively. Required unless `output_path` is given. |
| `alpha` | `None` | Matching gray clip to write as alpha. |
| `depth` | `8` for 8-bit input, otherwise `16` | PNG sample depth: `1`, `2`, `4`, `8`, or `16`. Packed depths are for gray clips. |
| `compression` | `6` | Lossless compression level from `0` to `9`. |
| `start_number` | `0` | Number added to the writer's frame index. Only with `output_path`. |
| `always_save` | `False` | Write a frame again if this writer has already saved it. |
| `overwrite` | `False` | Allow replacing an existing destination. |
| `icc_profile` | `False` | Embed the frame's raw `ICCProfile` property. |
| `matrix` | frame metadata | Matrix code to use when converting YUV without matrix metadata. |
| `debug` | `False` | Write timing information to the VapourSynth log. |

The writer converts supported YUV formats to RGB using the frame's matrix,
range, and chroma location. If the frame has no matrix, pass `matrix=` or
convert it before writing. Float samples are treated as values from 0 to 1 and
clamped to that range. For 9–15-bit input, the default 16-bit PNG preserves the
source precision; `depth=8` reduces it by rounding.

## build from source

Build a wheel with Python 3.12 or newer, Rust 1.94 or newer, CMake, Ninja, and
pkg-config. Native codec dependencies vary by platform; the Linux release build
setup is documented in the
[Linux build guide](https://github.com/noaione/vs-imageseqs/blob/master/docs/LINUX-BUILD.md).

```console
python -m pip install ".[dev]"
python -m build
```

## license

The plugin is licensed under the Mozilla Public License 2.0. See
[LICENSE](https://github.com/noaione/vs-imageseqs/blob/master/LICENSE) and the
[third-party notices](https://github.com/noaione/vs-imageseqs/blob/master/THIRD_PARTY_NOTICES)
for the licenses of bundled libraries.
