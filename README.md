# vapoursynth-imageseqs

a rust vapoursynth plugin for reading an ordered list of images as a clip.

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
)
```

`Read` accepts:

- `files`: required, ordered image paths.
- `fpsnum` and `fpsden`: frame rate. defaults to `24/1`.
- `mismatch`: set to `True` to allow different sizes or pixel formats.

by default, every image must have the same size and pixel format. with
`mismatch=True`, the clip uses variable format information and each frame
keeps its own size and format.

supported output formats are gray 8/16-bit, rgb 8/16-bit, and rgb 32-bit
float. alpha channels are ignored.

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
