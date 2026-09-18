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
- `debug`: set to `True` to log create and per-frame timing information.
- `prefetch`: number of background worker threads used to decode upcoming
  frames while the clip is read sequentially. `0` disables lookahead decoding.
  defaults to half the logical cores, capped at four.

by default, every image must have the same size and pixel format. with
`mismatch=True`, the clip uses variable format information and each frame
keeps its own size and format.

with `debug=True`, timing messages are sent to the VapourSynth log. they
include probing, decoding, frame allocation, planar conversion, frame
properties, and total frame time.

supported output formats are gray 8/16-bit, rgb 8/16-bit, and rgb 32-bit
float. `Read` ignores alpha channels; use `ReadAlpha` to read them.

common supported inputs include:
- `png`
- `jpeg`
- `bmp`
- `gif`
- `ico`
- `tiff`
- `webp`
- `avif` - via dav1d
- `heif`/`heic` - via libheif/libde265
- `jxl (jpeg xl)` - via jxl-rs
- `exr`
- `hdr`
- `pnm`
- `qoi`
- `tga`

frames include these properties:

- `ImgSeqPath` - the original file path
- `ImgSeqIndex` - the frame index in the input list
- `ImgSeqOriginalColorType` - the original image color type
- `ImgSeqHasICC` - whether the original image had an ICC profile
- `ImgSeqOrientation` - the original image orientation, if any
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
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --dir "I:/Manga/Yuri Love Story/source/v02" --pattern "Yuri Love Story - v02 - p%03d.jpg"
```

the measured results, split per image format, are in
[benchmarks](docs/BENCH.md). in short: imgseqs reads a 130 file jpeg set 1.75x
faster than bestsource and a 130 file png set 1.32x faster once the open is
counted, while a 163 file webp set goes the other way, because ffmpeg threads a
single vp8 frame across every core and `webp` is decoded in one pure rust
thread. it also reads a folder that mixes jpeg and png pages as one clip, and
reads `jxl`, which this bestsource build cannot open at all.

when comparing with bestsource, open it with `cachemode=0` and
`apply_rotation=False`, and do not pass `fpsnum`/`fpsden` for image sequences:
ffmpeg reads them at 25 fps, so any other rate resamples and silently drops
every 25th image.

## build

the project needs rust/cargo, a local vcpkg install, and python 3.12 or
newer. the development extra contains the python build tools:

```console
python -m pip install ".[dev]"
python -m build
```

see [THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES) and [LICENSES](LICENSES) for
the native dependency obligations.

## license

the project is licensed under the MPL-2.0. see [license](LICENSE).
