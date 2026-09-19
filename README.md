# vapoursynth-imageseqs

a (quite-fast) rust vapoursynth plugin for reading an ordered list of images as a clip.

## install

```console
// Soon:tm:
python -m pip install vapoursynth-imageseqs
```

the wheel installs the native plugin at:

```text
vapoursynth/plugins/vs_imageseqs.dll
```

use the matching `.so` or `.dylib` on linux or macos. vapoursynth normally
finds plugins in this directory. if it does not, load the file explicitly:

```python
core.std.LoadPlugin(r"path/to/vs_imageseqs.dll")
```

## install without python packaging

download the matching `*-plugin` artifact from a ci run and copy its file to
your vapoursynth plugin directory. each artifact also includes the license
and notice files. the native files are:

- windows: `vs_imageseqs.dll`
- linux: `libvs_imageseqs.so`
- macos arm64: `libvs_imageseqs.dylib`

you can also keep the file anywhere and load it explicitly in your script:

```python
core.std.LoadPlugin(r"path/to/vs_imageseqs.dll")
```

then use `core.imgseqs.Read` as shown below.

## use

pass the files in the order they should become frames:

```python
from pathlib import Path

import vapoursynth as vs

core = vs.core
files = sorted(Path(r"I:\images").glob("*.png"))

clip = core.imgseqs.Read(
    files=[str(path) for path in files],
    fpsnum=24,
    fpsden=1,
    debug=False,
)
```

`Read` accepts:

- `files`: required, ordered image paths.
- `fpsnum` and `fpsden`: frame rate. defaults to `24/1`.
- `mismatch`: set to `True` to allow different sizes or pixel formats.
- `apply_rotation`: set to `False` to hand out the picture the file stores
  instead of the one its exif orientation describes. defaults to `True`.
- `debug`: set to `True` to log create and per-frame timing information.
- `prefetch`: number of background worker threads used to decode upcoming
  frames while the clip is read sequentially. `0` disables lookahead decoding.
  defaults to half the logical cores, capped at four.
- `prefetch_memory`: lookahead memory budget in MiB. defaults to the larger of
  192 MiB and one frame of the biggest image in the sequence per worker, so a
  deep lookahead on large images is not starved by a fixed ceiling. `0` is
  rejected; use `prefetch=0` to disable lookahead decoding.

by default, every image must have the same size and pixel format. with
`mismatch=True`, the clip uses variable format information and each frame
keeps its own size and format.

an orientation is applied by default: a page whose exif says 6 comes out rotated
90 degrees clockwise, and orientations 5 to 8 swap the frame's width and height.
the code is read from the file's exif, or from the codestream header for a jpeg
xl, which states it there instead. `ImgSeqOrientation` still reports the code the
file carries, so a code other than 1 on a frame that is not the file's stored
size means the picture was transformed. `apply_rotation=False` hands out the
stored picture at the size the file stores; a folder that mixes a rotated page
with upright ones then fails at
creation unless `mismatch=True`, exactly as a folder of different sizes does.
This is one more thing to switch off when comparing against a plugin that does
not rotate.

with `debug=True`, timing messages are sent to the VapourSynth log. they
include probing, decoding, frame allocation, planar conversion, frame
properties, and total frame time.

supported output formats are gray 8/16-bit, rgb 8/16-bit, rgb 32-bit float,
and `YUV420P8` for a lossy webp file. `Read` ignores alpha channels; use
`ReadAlpha` to read them.

### common inputs

- `png`
- `jpeg`
- `bmp`
- `gif`
- `ico`
- `tiff`
- `webp` - via libwebp, lossy files without alpha as `YUV420P8`, see below
- `avif` - via dav1d, monochrome pages as `Gray8`/`Gray16`
- `heif`/`heic` - via libheif/libde265, monochrome pages as `Gray8`/`Gray16`
- `jxl (jpeg xl)` - via jxl
- `exr`
- `hdr`
- `pnm`
- `qoi`
- `tga`

### lossy webp comes back as yuv

a lossy webp with no alpha channel decodes into its own planes and comes back as
`YUV420P8`, tagged `_Matrix=5` (bt470bg) and `_Range=0` (limited), because that
is what the bitstream holds. lossless webp, a webp with an alpha channel, and
every other format keep the `RGB24`/`Gray8` output they always had.

a graph that needs rgb converts back once:

```python
clip = core.resize.Bicubic(clip, format=vs.RGB24, matrix_in_s="470bg", range_in_s="limited")
```

that one line already covers a folder in any mix of formats and sizes: `zimg`
converts a variable clip frame by frame, at each page's own size, and the
arguments are ignored for `RGB24` and `Gray8` frames. the exception is an odd
sized `4:2:0` page, which `zimg` refuses with `Resize error 1027`. this chain
handles those per frame and clamps the result to one format, which a filter that
needs a constant one (`std.GPUUpload`, and so `ogsov.AnalyzeVk`) requires:

```python
source = core.imgseqs.Read(files=files, mismatch=True)  # the clip the chain reads from
plain_rgb = core.resize.Bicubic(
    source, format=vs.RGB24, matrix_in_s="470bg", range_in_s="limited"
)


def chain(n=0, **_):  # FrameEval passes the frame number as a keyword
    frame = source.get_frame(n)  # this frame's format and size
    right = frame.width % 2 if frame.format.subsampling_w else 0
    bottom = frame.height % 2 if frame.format.subsampling_h else 0
    if not right and not bottom:
        return plain_rgb
    even = core.std.CropAbs(source, width=frame.width - right, height=frame.height - bottom)
    rgb = core.resize.Bicubic(even, format=vs.RGB24, matrix_in_s="470bg", range_in_s="limited")
    return core.std.AddBorders(rgb, right=right, bottom=bottom)


rgb = core.std.FrameEval(source, chain)
rgb = core.resize.Bicubic(rgb, format=vs.RGB24)  # FrameEval cannot promise a format
```

it costs 3 ms per frame over the plain read on `sandbox/mixed` and 5 ms on the
odd page webp set. keep `source` in its own variable and never rebind it: the
chain asks that clip for frames, so a chain that closes over a clip built on the
`FrameEval` (the analysed clip, say) asks the node producing the frame for that
frame, and the request hangs instead of raising. a clip of one format and one
size could crop with `std.Crop` instead of `std.CropAbs`; a `mismatch` clip
cannot, because `Crop` and `AddBorders` both want one constant format and size,
so such a folder is grouped in python by format *and* size first. the restored
border is black: stack the last real column and row on
(`StackHorizontal([rgb, core.std.Crop(rgb, left=rgb.width - 1)])`, then the same
vertically) to extend the edge instead.

a decoded frame already drops the last chroma row and column of an odd sized
page, because a `4:2:0` plane in a VapourSynth frame is `floor(size / 2)`.
[BENCH.md](docs/BENCH.md) has the measurements.

### colour metadata comes from the container

a file that states its own colour is tagged with it. `_Primaries` and
`_Transfer` are written for every family, and `_Matrix` and `_Range` for a yuv
frame — where the file's own values now replace the `470bg`/`limited` pair the
line above assumes, so read them instead of assuming them. the code points are
the H.273 numbers VapourSynth already uses, read from an `nclx` colour box in
avif and heif/heic, a `cICP` chunk in png, and the codestream header in jxl.

a code VapourSynth has no name for, and code 2 (`unspecified`), leave the
property unset rather than guessed at, which is also what a file with no colour
statement at all does: an untagged png is handed out exactly as it always was. an
rgb frame keeps `_Matrix=0` and `_Range=1` whatever the file says, because what
the file describes is the yuv it codes, and an ICC profile on its own is still
only `ImgSeqHasICC`.

### frame properties

- `ImgSeqPath` - the original file path
- `ImgSeqIndex` - the frame index in the input list
- `ImgSeqOriginalColorType` - the original image color type
- `ImgSeqHasICC` - whether the original image had an ICC profile
- `ImgSeqOrientation` - the orientation the file states, if any. this is the
  file's own answer even when `apply_rotation` changed the picture
- `ImgSeqAlpha` - `1` on frames of a clip created by `ReadAlpha`

### alpha clips

`ReadAlpha` accepts the same arguments as `Read` and returns the color clip
plus a separate gray alpha clip:

```python
result = core.imgseqs.ReadAlpha(files=[str(path) for path in files])
clip, alpha = result["clip"], result["alpha"]
```

a plugin function with several outputs returns a dictionary in python, keyed
by the names of its return type (`clip` and `alpha`).

alpha keeps the sample depth of the source: 8-bit input becomes `GRAY8`,
16-bit input becomes `GRAY16`, and float input becomes `GRAYS`. the alpha of
`LA` input is its second channel and the alpha of `RGBA` input is its fourth.
files without an alpha channel produce an opaque plane (`255`, `65535`, or
`1.0`), so an alpha clip always has a meaningful value.

both clips share the frame count, the frame rate, and the frame indexes, and
both are read from one decode per file, so asking for the alpha clip does not
decode the images twice. `mismatch` applies to both clips. the alpha clip is
marked with the `ImgSeqAlpha` property.

## performance

`tests/bench-imgseqs-vs-bestsource.vpy` reads one image sequence with `Read`
and with [bestsource](https://github.com/vapoursynth/bestsource), timing every
`get_frame` and printing a per-frame table plus the totals:

```console
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --dir images --pattern "i_%04d.webp"
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --extra --prefetch 16 --dir sandbox/webp --pattern "snek - p%03d.webp"
```

the measured results, split per image format, are in
[benchmarks](docs/BENCH.md). in short, on the same 35 pages saved in six
containers: imgseqs is 2.73x faster than bestsource on `jpeg` (4.98x once the
open is counted), 1.37x faster on `png`, and 2.65x slower on `webp`, where
ffmpeg threads a single vp8 frame across every core while `webp` is decoded in
one pure rust thread. bestsource cannot open `avif`, `heic` or `jxl` at all in
that build, and imgseqs reads a folder that mixes jpeg and png pages as one
clip.

creating a clip only reads what each container states about its file, so opening
35 avif pages of 130 MB costs 2 ms and 35 pages of any other of these formats
costs 5 ms at most. the decode happens per frame, in the background pool.

when comparing with bestsource, open it with `cachemode=0` and
`apply_rotation=False`, and pass `apply_rotation=False` to `Read` as well: the
two plugins rotate by default, but a fair read comparison wants neither of them
to. do not pass `fpsnum`/`fpsden` for image sequences: ffmpeg reads them at
25 fps, so any other rate resamples and silently drops every 25th image.

## build

the project needs rust/cargo, a local vcpkg install, and python 3.12 or
newer. the development extra contains the python build tools:

```console
python -m pip install ".[dev]"
python -m build
```

on linux and macos the native dependencies come from the system package
manager instead of vcpkg, the same list the ci installs:

```console
# debian/ubuntu
sudo apt-get install --yes cmake ninja-build pkg-config libdav1d-dev libde265-dev libwebp-dev
# macos, with homebrew
brew install cmake ninja pkg-config dav1d libde265 webp
```

libwebp is linked from its static archive when the development package installs
one, which both of the packages above do, so the plugin does not need libwebp at
run time; dav1d and libde265 stay shared libraries.

see [THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES) and [LICENSES](LICENSES) for
the native dependency obligations.

## license

the project is licensed under the MPL-2.0. see [license](LICENSE).
