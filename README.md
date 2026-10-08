# vapoursynth-imageseqs

[![uv powered](https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/astral-sh/uv/main/assets/badge/v0.json)](https://github.com/astral-sh/uv)
[![License](https://img.shields.io/github/license/noaione/vs-imageseqs)](https://github.com/noaione/vs-imageseqs/blob/master/LICENSE)
![VapourSynth version](https://img.shields.io/badge/vapoursynth-%3E%3DR79-blue) [![build](https://github.com/noaione/vs-imageseqs/actions/workflows/build.yml/badge.svg)](https://github.com/noaione/vs-imageseqs/actions/workflows/build.yml) [![rust tests](https://github.com/noaione/vs-imageseqs/actions/workflows/rust-tests.yml/badge.svg)](https://github.com/noaione/vs-imageseqs/actions/workflows/rust-tests.yml)

`vapoursynth-imageseqs` is a VapourSynth plugin that reads image files in the
order you provide and turns them into a clip.

## what it does

- reads an ordered list of image files
- provides a color clip and, when requested, a separate alpha clip
- keeps supported image bit depths and color formats
- can return the source's YUV planes for supported formats
- reads image orientation and color information
- decodes frames in the background to help playback
- plays animated files, contributing the pictures they display
- writes a frame as a PNG when a graph asks for it

## install

Install the wheel with pip:

```console
python -m pip install vapoursynth-imageseqs
```

The wheel installs the plugin and its manifest under VapourSynth's plugin
directory. It does not install VapourSynth itself. You need VapourSynth R79 or
newer.

You can also download a plugin zip from the project's releases or CI artifacts.
Copy the included `imageseqs` folder into VapourSynth's plugin directory. If it
does not load automatically, call `LoadPlugin` with the path to your plugin file:

```python
core.std.LoadPlugin(r"path/to/plugin-library")
```

Use the plugin file for your system:

| system | plugin file |
| --- | --- |
| Windows | `vs_imageseqs.dll` |
| Linux | `libvs_imageseqs.so` |
| macOS arm64 | `libvs_imageseqs.dylib` |

Linux and macOS wheels bundle the `dav1d` and `libde265` libraries they need,
under the plugin's `lib/` folder, and so do the standalone ZIPs. Windows builds
link them statically instead.
Linux wheels are published for both C libraries: `manylinux_2_28` for glibc
systems and `musllinux_1_2` for musl ones such as Alpine. A wheel built for one
cannot load on the other. The musl wheel also bundles the C++ runtime
(`libstdc++`, `libgcc_s`) that the embedded libheif links, because auditwheel's
musl policy promises the host only `libc` and `libz`.

## quick start

Pass the files in the order you want them to appear in the clip:

```python
from pathlib import Path

import vapoursynth as vs

core = vs.core
files = sorted(Path(r"I:\images").glob("*.png"))

clip = core.imgseqs.Read(
    files=[str(path) for path in files],
    fpsnum=24,
    fpsden=1,
)
```

`Read` returns the color clip. `ReadAlpha` returns the color clip and a separate
gray clip containing alpha:

```python
result = core.imgseqs.ReadAlpha(files=[str(path) for path in files])
clip = result["clip"]
alpha = result["alpha"]
```

Files without alpha get a fully opaque alpha frame.

## options

| option | default | what it does |
| --- | --- | --- |
| `files` | required | Image paths, in frame order. |
| `fpsnum`, `fpsden` | `24`, `1` | Set the clip's frame rate. |
| `mismatch` | `False` | Allow files with different sizes or pixel formats. |
| `apply_rotation` | `True` | Apply the orientation stored in the image. |
| `icc_profile` | `False` | Copy embedded ICC profile bytes to the `ICCProfile` frame property. |
| `debug` | `False` | Write timing information to the VapourSynth log. |
| `prefetch` | half of your logical CPU cores, up to 4 | Set the number of background decode workers. Use `0` to disable lookahead. |
| `prefetch_memory` | 192 MiB minimum, more for large images | Set the memory limit for frames waiting to be read. `0` is invalid. |

Without `mismatch=True`, all files must have the same size and pixel format.
With it, each frame can keep its own size and format. Downstream filters may
still require you to convert the clip to one consistent format.

The plugin applies image orientation by default. Some orientations swap the
frame's width and height. Set `apply_rotation=False` to keep the stored image
size and pixel order.

## supported formats

| formats | notes |
| --- | --- |
| PNG, JPEG, BMP, GIF, ICO, TIFF | Standard image formats. PNG and GIF can be animated. |
| DDS | DXT1, DXT3, and DXT5 images. |
| Farbfeld | 16-bit RGBA images. |
| WebP | Lossy opaque images can be returned as YUV. Can be animated. |
| AVIF | Monochrome images can be gray; color images may keep their YUV planes. Can be an image sequence. |
| HEIF, HEIC | Monochrome images can be gray; color images may keep their YUV planes. Can be an image sequence. |
| JPEG XL | Decoded by the JPEG XL library. Can be animated. |
| JPEG 2000 | JP2, J2K, JPF, JPX, and J2C files. Some files can be returned as YUV. |
| EXR, HDR, PNM, QOI, TGA | Standard image formats. |

The plugin supports gray and RGB images from 8 to 16 bits, 32-bit float RGB,
and several YUV formats. For image formats that state a 9–15-bit depth, the
plugin uses the matching VapourSynth format, such as `Gray12` or `RGB30` for a
10-bit RGB image.

## animated images

An animated file contributes the pictures it displays to the same clip instead
of a single frame. GIF, APNG, WebP, JPEG XL, AVIF and HEIF/HEIC are supported
this way; multi-page TIFF and ICO are not, because their pages are documents
rather than a timeline.

The clip stays constant rate at the `fpsnum`/`fpsden` you ask for, and each
output frame shows whatever the file displays at that instant:

- a picture held for longer than one output tick is repeated across the ticks
  it covers.
- a picture whose delay is zero or absent is held for one output tick rather
  than dropped.
- a picture that falls entirely between two ticks is not shown; one that
  becomes visible part way through a tick is shown from that tick.

Timing uses exact rational arithmetic, so a fractional rate and a delay that
does not divide it both land where the file says they should. A file's loop
count is ignored: each listed path plays once, and every frame keeps its own
file's path, `ImgSeqIndex` and `ImgSeqAnimationIndex`.

`ReadAlpha` returns the logical canvas a viewer would show, including pixels
cleared by a frame's disposal. One exception: a colour-only read of a HEIF or
HEIC sequence still decodes its linked alpha track, because the library exposes
no way to skip that work. Seeking backwards replays from the file's own
checkpoint where the format has one, and only a bounded window of decoded
pictures is kept, so memory does not grow with the animation's length.

Size and format follow `mismatch` as they do for stills, so a list that mixes
an animation with a still of another size needs `mismatch=True`.

## alpha and color

`Read` returns color only. `ReadAlpha` adds a separate gray alpha clip, at the
source's bit depth where possible; files without alpha get an opaque plane
filled with the maximum value for that depth.

Color information from the file's metadata becomes VapourSynth frame
properties, and no ICC transform is applied to the pixels. Set
`icc_profile=True` to also copy the raw profile bytes into `ICCProfile`;
`ImgSeqHasICC` reports whether the source has a profile either way.

You can use [VapourSynth-ICCConvert](https://github.com/YomikoR/VapourSynth-ICCConvert) plugin to apply the embedded ICC profile.

When a supported source is YUV, the plugin may return those planes directly.
VapourSynth's resize filters can convert them to RGB and read the frame's color
properties:

```python
rgb = core.resize.Bicubic(clip, format=vs.RGB24)
```

For a source without color metadata, set the input matrix and range yourself.
See the [VapourSynth resize documentation](https://www.vapoursynth.com/doc/functions/video/resize.html)
for the available values.

`zimg` cannot convert an odd-sized 4:2:0 frame, which a variable-size clip can
easily contain. This example crops the odd edge of each frame before
converting and adds it back afterward, ending with one fixed format for
filters such as `std.GPUUpload`:

```python
source = core.imgseqs.Read(files=files, mismatch=True)
plain_rgb = core.resize.Bicubic(source, format=vs.RGB24)

def convert(n=0, **_):
    frame = source.get_frame(n)
    right = frame.width % 2 if frame.format.subsampling_w else 0
    bottom = frame.height % 2 if frame.format.subsampling_h else 0
    if not right and not bottom:
        return plain_rgb
    even = core.std.CropAbs(
        source,
        width=frame.width - right,
        height=frame.height - bottom,
    )
    rgb = core.resize.Bicubic(even, format=vs.RGB24)
    return core.std.AddBorders(rgb, right=right, bottom=bottom)

rgb = core.std.FrameEval(source, convert)
rgb = core.resize.Bicubic(rgb, format=vs.RGB24)
```

The restored edge is black here; stack the last real row and column instead to
extend the picture. `CropAbs` accepts variable-size clips, while `Crop` and
`AddBorders` need a fixed size and format.

## writing pngs

`PNGWrite` is the other end of `Read`: it returns a node whose frames are the
input's, and asking for one writes that frame as a PNG before the request
returns. Creating the node writes nothing, so a frame you never ask for is
never written.

```python
writer = core.imgseqs.PNGWrite(
    rgb,
    output_path=r"exports\page%04d.png",
)

with writer.get_frame(0):
    pass  # exports\page0000.png exists now

for frame in range(writer.num_frames):
    with writer.get_frame(frame):
        pass
```

| option | default | what it does |
| --- | --- | --- |
| `clip` | required | The clip to write. Gray, RGB or YUV at 8 to 16 bits, or float gray or RGB. |
| `output_path` | required | A filename for a one-frame clip, or a `%d`/`%06d` template. `%%` is a literal percent sign. |
| `alpha` | `None` | A matching gray clip whose samples become the alpha channel. |
| `always_save` | `False` | Write a frame again instead of skipping one this writer already saved. |
| `overwrite` | `False` | Replace a destination that is already there. |
| `compression` | `6` | `0` to `9`. Every level is lossless; higher is smaller and slower. |
| `start_number` | `0` | Added to the writer's frame index when the path is numbered. |
| `icc_profile` | `False` | Embed the raw `ICCProfile` frame property in the file. |
| `depth` | the frame's | The PNG's stored bits per sample: `1`, `2`, `4`, `8` or `16`. |
| `matrix` | the frame's | An H.273 matrix code to read a YUV clip with, when the frame states none. |
| `debug` | `False` | Write timing information to the VapourSynth log. |

The number in the path is the writer node's own frame index, not any index the
source wrote, so trimming, reversing or splicing the input cannot make two
frames share a destination. A relative `output_path` is resolved when the node
is created, and its directory has to exist already.

Without `depth`, a frame of 9 to 15 bits is stored as a 16-bit PNG whose high
source precision, with an `sBIT` chunk stating how many bits are meaningful.
A ten-bit white is therefore stored as 65535 rather than 98% of the scale, and
shifting the stored sample right by 6 gives the ten-bit sample back. Reading
the file with `Read` gives the 16-bit PNG the file is, not a ten-bit frame,
because PNG states no depth of its own.

A YUV clip is converted to RGB for you, because a lossy WebP, a colour AVIF and
a colour HEIC arrive as the YUV planes their container coded:

```python
yuv = core.imgseqs.Read(files=[r"pages\page001.webp"])
writer = core.imgseqs.PNGWrite(yuv, output_path=r"exports\page%04d.png")
```

The matrix is the frame's own `_Matrix`, which every reader here writes, and
`_Range` says whether its samples are limited or full. `matrix=6` names one for
a clip you built yourself that states none. Chroma is upsampled with a bilinear
filter from the sample position `_ChromaLocation` names, or the left-sited
position `resize` defaults to, and the picture is the one
`core.resize.Bilinear` gives, sample for sample. Only the
non-constant-luminance matrices are converted: a constant-luminance frame, the
SMPTE 2085 one and the chromaticity-derived ones are refused by name.

The conversion is vectorised where the processor has AVX2 or AVX-512 and is the
same loop otherwise, which is 12 to 42 times the loop it replaces: a 4:2:0 page
costs about 1.0 to 1.4 ns a pixel, which is `core.resize`'s own order, so a
graph can convert here rather than putting a `resize` in front. Both paths
write byte for byte the same files. It is also the route for a frame `resize`
refuses, such as a 4:2:0 picture with an odd height.

A float clip is a value in [0, 1] rather than a code. It is written at 16 bits,
or at the width `depth` asks for; a value outside the range is clamped, and a
NaN is written as the smallest sample.

`depth` names the word the file stores. `16` widens an 8- or 10-bit frame
exactly, `8` gives up precision by rounding, and `1`, `2` or `4` pack a gray
frame, which is how a bilevel or a 16-colour page is written and how a page a
low-bit reader expanded is written back to the word it came from.

A written frame carries `ImgSeqPNGWritePath`, `ImgSeqPNGWriteSaved` and
`ImgSeqPNGWritePerformed`, so a caller can tell a write from a skip. The write
goes to a temporary file beside the destination and is only moved into place
once it is complete, so a failed encode never leaves a partial file where the
destination should be.

A consumer that keeps several frames in flight gets several encodes at once.
`writer.frames(prefetch=6)` and six open `get_frame_async` futures both finish
a sequence about 4.7 times faster than asking for one frame at a time, with
identical files. `vspipe` already pulls ahead, so a pipe needs nothing extra.

## frame properties

| property | meaning |
| --- | --- |
| `ImgSeqPath` | Source file path. |
| `ImgSeqIndex` | Position of the file in `files`. |
| `ImgSeqAnimationIndex` | Position of the displayed picture within its file. Only written for animated files. |
| `ImgSeqOriginalColorType` | Color type the source file states. |
| `ImgSeqOrientation` | Orientation code stored in the source. |
| `ImgSeqHasICC` | Whether the source contains an ICC profile. |
| `ICCProfile` | Raw profile bytes when `icc_profile=True`. |
| `ImgSeqAlpha` | Set to `1` on frames from the alpha clip. |
| `ImgSeqPNGWritePath` | Destination a `PNGWrite` frame was written to. |
| `ImgSeqPNGWriteSaved` | `1` when this writer has published this frame. |
| `ImgSeqPNGWritePerformed` | `1` when this evaluation wrote it, `0` when it was skipped. |

The plugin also sets standard VapourSynth color properties when the source
provides values it can represent.

## performance

The plugin probes image metadata when it creates a clip and decodes pixels when
frames are requested. Background workers can read upcoming frames. See the
[benchmark notes](https://github.com/noaione/vs-imageseqs/blob/master/docs/BENCH.md)
for results and instructions to compare against BestSource.

## build from source

You need Rust 1.94 or newer, Python 3.12 or newer, CMake, Ninja, and `pkg-config`.
You also need the codec libraries for your operating system, and `nasm` on x86
and x86-64, which the webp decoder assembles its own routines with.

On Debian or Ubuntu:

```console
sudo apt-get install --yes cmake ninja-build pkg-config nasm libdav1d-dev libde265-dev
```

To build from source on macOS with Homebrew:

```console
brew install cmake ninja pkg-config nasm dav1d libde265
```

Then build the wheel:

```console
python -m pip install ".[dev]"
python -m build
```

Windows builds use vcpkg. See [Linux build and validation](https://github.com/noaione/vs-imageseqs/blob/master/docs/LINUX-BUILD.md)
for details about Linux release builds.

## license

The plugin is licensed under the Mozilla Public License 2.0. See [LICENSE](https://github.com/noaione/vs-imageseqs/blob/master/LICENSE).
The included native libraries have their own licenses. See
[THIRD_PARTY_NOTICES](https://github.com/noaione/vs-imageseqs/blob/master/THIRD_PARTY_NOTICES)
and [LICENSES](https://github.com/noaione/vs-imageseqs/tree/master/LICENSES) for details.
