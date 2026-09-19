# vapoursynth-imageseqs

a rust vapoursynth source plugin for reading an ordered list of images as a
clip.

## install

the python wheel is plugin-only. it installs the native library at
`vapoursynth/plugins/` and does not add a python module.

```console
python -m pip install vapoursynth-imageseqs
```

until the package is published, download the matching `*-plugin` ci artifact
and copy its native file into your vapoursynth plugin directory:

| platform | file |
| --- | --- |
| windows | `vs_imageseqs.dll` |
| linux | `libvs_imageseqs.so` |
| macos arm64 | `libvs_imageseqs.dylib` |

if vapoursynth does not find it automatically, load it explicitly:

```python
core.std.LoadPlugin(r"path/to/vs_imageseqs.dll")
```

the ci artifact and wheel include `LICENSE`, `THIRD_PARTY_NOTICES`, and
`LICENSES/`.

## use

pass files in the order they should become frames:

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

`Read` returns the color clip. `ReadAlpha` returns a dictionary containing the
color clip and a separate gray alpha clip:

```python
result = core.imgseqs.ReadAlpha(files=[str(path) for path in files])
clip, alpha = result["clip"], result["alpha"]
```

### arguments

| argument | default | meaning |
| --- | --- | --- |
| `files` | required | ordered image paths |
| `fpsnum`, `fpsden` | `24/1` | output frame rate |
| `mismatch` | `False` | allow variable sizes and pixel formats |
| `apply_rotation` | `True` | apply the file's exif/codestream orientation |
| `debug` | `False` | log create and per-frame timings |
| `prefetch` | half the logical cores, capped at 4 | background decode workers; `0` disables lookahead |
| `prefetch_memory` | max of 192 MiB and one largest frame per worker | lookahead budget in MiB; `0` is invalid |

without `mismatch`, every file must have the same size and format. with it,
each frame keeps its own size and format.

rotation applies the displayed orientation by default. orientations 5–8 swap
width and height, and `ImgSeqOrientation` still reports the original code.
`apply_rotation=False` keeps the stored picture and stored size. a rotated and
an upright file therefore need `mismatch=True` when rotation is enabled.

`debug=True` logs probing, decoding, allocation, pixel conversion, properties,
and total frame time to the vapoursynth log.

## formats

| input | notes |
| --- | --- |
| png, jpeg, bmp, gif, ico, tiff | standard image decoder |
| dds | image's DXT1, DXT3, and DXT5 decoder |
| farbfeld | 16-bit RGBA input |
| webp | libwebp; lossy opaque files use `YUV420P8` |
| avif | dav1d; monochrome uses `Gray8`/`Gray10`/`Gray12`, color may use its YUV planes |
| heif, heic | libheif/libde265; monochrome uses `Gray8`/`Gray10`/`Gray12`, color may use its YUV planes |
| jxl | jpeg xl decoder |
| exr, hdr, pnm, qoi, tga | standard image decoder |

supported output includes gray and rgb at 8–16 bits, rgb 32-bit float, and the
YUV formats needed by the source: `YUV420P8`, `YUV420P10`, `YUV422P8`,
`YUV422P10`, `YUV444P8`, `YUV444P10`, and `YUV444P12`.

`Read` ignores alpha. `ReadAlpha` uses the source depth for alpha: `Gray8`–
`Gray16`, or `GrayS`. the alpha channel is the second channel of `LA` and the
fourth channel of `RGBA`. files without alpha get an opaque plane filled with
the format maximum (`255`, `1023`, `4095`, `65535`, or `1.0`).

both clips share the frame count, rate, and indexes. each file is decoded once,
and `mismatch` applies to both clips.

### nominal bit depth

when a container states 9–16 bits, the frame format names that depth and the
samples are right-aligned in its word. for example, a ten-bit avif is `RGB30`
with samples in `0..1023`, not `RGB48` with sixteen-bit-scaled samples. this
also applies to gray and alpha. files with no stated depth, 8-bit files,
16-bit files, and float files keep their decoder format.

### yuv output

the plugin keeps a file's own YUV planes when its container names a matrix that
vapoursynth understands:

- lossy webp without alpha: `YUV420P8`, `_Matrix=5`, `_Range=0`;
- color heif/heic/avif: the source planes, such as `YUV420P8` or `YUV444P10`,
  tagged from the file's matrix and range.

an unspecified matrix (`2`) or no matrix keeps the RGB frame the plugin builds.
lossless webp, webp with alpha, and other formats stay RGB or gray. use
`resize` when a downstream filter needs RGB:

```python
rgb = core.resize.Bicubic(clip, format=vs.RGB24)
```

the frame's `_Matrix` and `_Range` are used automatically. for an untagged
source, provide them explicitly:

```python
rgb = core.resize.Bicubic(
    clip,
    format=vs.RGB24,
    matrix_in_s="470bg",
    range_in_s="limited",
)
```

`zimg` cannot convert an odd-sized 4:2:0 frame directly. for a variable clip,
crop odd edges before conversion and add the border back. `FrameEval` can do
that per frame, then a final `resize` can declare one constant format for
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

`Crop` and `AddBorders` need a constant size and format, so use `CropAbs` for
variable clips. the example restores odd edges with black pixels; stack the
last real row and column instead if edge extension is preferred. a variable
clip is converted frame by frame, and resize arguments are ignored for RGB
frames.

## frame properties

| property | meaning |
| --- | --- |
| `ImgSeqPath` | original file path |
| `ImgSeqIndex` | input-list index |
| `ImgSeqOriginalColorType` | decoder's original color type |
| `ImgSeqHasICC` | whether the source has an ICC profile |
| `ImgSeqOrientation` | orientation code carried by the source |
| `ImgSeqAlpha` | `1` on frames from the alpha clip |

container color metadata is mapped to `_Primaries`, `_Transfer`, `_Matrix`,
`_Range`, and, when explicitly named, `_ChromaLocation`. sources include avif
and heif/heic `nclx`, png `cICP`, jxl codestream headers, and av1 sequence
headers. unknown or unspecified codes are left unset. RGB frames always keep
`_Matrix=0` and `_Range=1`; an ICC profile alone only affects
`ImgSeqHasICC`.

## performance

benchmark a sequence against [bestsource](https://github.com/vapoursynth/bestsource)
with:

```console
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --dir images --pattern "i_%04d.webp"
.venv/Scripts/python.exe tests/bench-imgseqs-vs-bestsource.vpy --reps 3 --extra --prefetch 16 --dir sandbox/webp --pattern "snek - p%03d.webp"
```

see [docs/BENCH.md](docs/BENCH.md) for the measurements. on the same 35 pages
stored in six containers, imgseqs is 2.73x faster than bestsource on jpeg
(4.98x including open), 1.37x faster on png, and 1.80x slower on webp because
ffmpeg slice-threads each vp8 frame while libwebp decodes one frame at a time.
that bestsource build cannot open avif, heic, or jxl.

clip creation only probes container metadata; decoding happens when frames are
requested, using the background pool. for a fair bestsource comparison, use
`cachemode=0`, `apply_rotation=False` on both plugins, and omit `fpsnum` and
`fpsden` because ffmpeg's image-sequence rate is 25 fps.
opening 35 avif pages costs about 2 ms, 35 heic pages about 6 ms, and the other
listed formats less in the recorded benchmark.

## build

requirements: rust/cargo, a local vcpkg install on windows, and python 3.12+.

```console
python -m pip install ".[dev]"
python -m build
```

linux and macos need these native packages:

```console
# debian/ubuntu
sudo apt-get install --yes cmake ninja-build pkg-config libdav1d-dev libde265-dev libwebp-dev

# macos with homebrew
brew install cmake ninja pkg-config dav1d libde265 webp
```

libwebp is linked from its static archive when available, so it is not needed
at runtime. dav1d and libde265 remain shared on unix; windows uses the static
vcpkg triplet.

see [THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES) and [LICENSES](LICENSES) for
native dependency obligations. the project itself is licensed under the
MPL-2.0; see [LICENSE](LICENSE).
