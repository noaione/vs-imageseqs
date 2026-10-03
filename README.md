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
- process animated files contribute their displayed pictures to the clip.

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

Linux and macOS release wheels include the required `dav1d` and `libde265`
libraries. The macOS wheel and standalone ZIP carry them in
`imageseqs/lib/`. Windows builds use static libraries.

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

An animated file in `files` contributes its displayed pictures to the same clip
rather than one frame. A still file still contributes exactly one frame.

Animated GIF, APNG, animated WebP, animated JPEG XL, and AVIF and HEIF/HEIC
image sequences are supported. Multi-page TIFF and ICO are not part of this:
their pages are documents and alternatives rather than a timeline.

The clip stays constant rate at the `fpsnum`/`fpsden` you ask for, and each
output frame shows the picture the file displays at that instant:

- a picture held for longer than one output tick is repeated across the ticks
  it covers.
- a picture that falls entirely between two ticks is not shown.
- a picture whose delay is zero or absent is held for one output tick rather
  than dropped.
- a picture that becomes visible part way through a tick is shown from that
  tick.

The timing is exact rational arithmetic, so a fractional `fpsnum`/`fpsden` and a
delay that does not divide it both land where the file says they should. A
file's loop count is ignored: each listed path plays once, and the next file
starts where the previous one ends.

Every frame a path contributes carries that file's own metadata. `ImgSeqIndex`
is the picture's position within its file, so an animation's frames are numbered
from zero within that animation. Frame size, format and `mismatch` behave as
they do for stills, so a list that mixes an animation with a still of another
size needs `mismatch=1`.

Alpha is composited the same way the format defines it: `ReadAlpha` returns the
logical canvas a viewer would show, including cleared pixels after a frame's
disposal. One exception is worth knowing: a colour-only read of a HEIF or HEIC
sequence still decodes that sequence's linked alpha track, because the library
used for it exposes no way to skip that work.

A backward seek re-reads from the file's own checkpoints where the format has
them, and otherwise replays from the start of that file. Only a bounded window
of decoded pictures is held, so memory does not grow with the length of an
animation.

## alpha and color

`Read` returns color only. `ReadAlpha` returns a separate gray alpha clip. Its
bit depth matches the source where possible. Images without alpha receive an
opaque plane, filled with the maximum value for that depth.

The plugin reads color information from supported file metadata and adds it to
VapourSynth frame properties. It does not change pixel values to apply an ICC
color transform. Set `icc_profile=True` to include the raw ICC profile bytes in
`ICCProfile`. The `ImgSeqHasICC` property reports whether the source contains a
profile, whether or not you export the bytes.

When a supported source is YUV, the plugin may return those planes directly.
VapourSynth's resize filters can convert them to RGB and read the frame's color
properties:

```python
rgb = core.resize.Bicubic(clip, format=vs.RGB24)
```

For a source without color metadata, set the input matrix and range yourself.
See the [VapourSynth resize documentation](https://www.vapoursynth.com/doc/functions/video/resize.html)
for the available values.

`zimg`, which VapourSynth uses for resizing, cannot convert an odd-sized 4:2:0
frame directly. For a variable-size clip, crop odd edges before conversion and
add a border back afterward. This example handles each frame with `FrameEval`
and ends with one fixed format for filters such as `std.GPUUpload`:

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

The example fills the restored edge with black. To extend the picture instead,
stack the last real row and column. `CropAbs` works with variable-size clips,
whereas `Crop` and `AddBorders` need a fixed size and format.

## frame properties

| property | meaning |
| --- | --- |
| `ImgSeqPath` | Source file path. |
| `ImgSeqIndex` | File's position in the input list. |
| `ImgSeqOriginalColorType` | Color type reported by the decoder. |
| `ImgSeqOrientation` | Orientation code stored in the source. |
| `ImgSeqHasICC` | Whether the source contains an ICC profile. |
| `ICCProfile` | Raw profile bytes when `icc_profile=True`. |
| `ImgSeqAlpha` | Set to `1` on frames from the alpha clip. |

The plugin also sets standard VapourSynth color properties when the source
provides values it can represent.

## performance

The plugin probes image metadata when it creates a clip and decodes pixels when
frames are requested. Background workers can read upcoming frames. See the
[benchmark notes](https://github.com/noaione/vs-imageseqs/blob/master/docs/BENCH.md)
for results and instructions to compare against BestSource.

## build from source

You need Rust 1.94 or newer, Python 3.12 or newer, CMake, Ninja, and `pkg-config`.
You also need the codec libraries for your operating system.

On Debian or Ubuntu:

```console
sudo apt-get install --yes cmake ninja-build pkg-config libdav1d-dev libde265-dev libwebp-dev
```

To build from source on macOS with Homebrew:

```console
brew install cmake ninja pkg-config dav1d libde265 webp
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
