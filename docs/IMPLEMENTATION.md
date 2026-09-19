# Rust VapourSynth Image Sequence Reader

## Goal

Implement a native VapourSynth image-sequence source plugin in Rust.

The source should accept an explicit ordered list of image files and expose them as frames of a single `VideoNode`.

Example:

```python
clip = core.imgseqs.Read(
    files=[
        "/images/001.png",
        "/images/002.png",
        "/images/003.webp",
        "/images/004.jxl",
    ],
    fpsnum=24,
    fpsden=1,
)
```

The plugin should be image-oriented rather than behaving like a general video demuxer.

Each file corresponds directly to one VapourSynth frame:

```text
frame 0 -> files[0]
frame 1 -> files[1]
frame 2 -> files[2]
...
```

This makes random access and parallel decoding straightforward.

---

# Technology Stack

Recommended stack:

```text
Rust
│
├── vapoursynth4-rs
│     ├── Filter
│     ├── CoreRef
│     ├── VideoFrame
│     ├── MapRef
│     └── declare_plugin!
│
├── image-rs
│     ├── PNG
│     ├── JPEG
│     ├── WebP
│     ├── TIFF
│     ├── AVIF
│     ├── EXR
│     └── other supported formats
│
└── jxl-rs
      └── JPEG XL
```

Use:

```toml
[lib]
crate-type = ["cdylib"]
```

The result can then be loaded as a normal VapourSynth plugin.

---

# VapourSynth Binding

Use:

```text
vapoursynth4-rs
```

rather than implementing the raw VapourSynth C ABI manually.

`vapoursynth4-rs` already provides the abstractions needed for the plugin:

```rust
Filter
CoreRef
VideoFrame
MapRef
FrameContext
ActivationReason
declare_plugin!
```

A source filter would roughly look like:

```rust
struct ImageSequence {
    images: Box<[ImageInfo]>,
    video_info: VideoInfo,
}

impl Filter for ImageSequence {
    type Error = ImgSeqError;
    type FrameType = VideoFrame;
    type FilterData = ();

    fn create(
        input: MapRef,
        output: MapRef,
        _data: Option<Box<Self::FilterData>>,
        mut core: CoreRef,
    ) -> Result<(), Self::Error> {
        todo!()
    }

    fn get_frame(
        &self,
        n: i32,
        reason: ActivationReason,
        _frame_data: *mut *mut std::ffi::c_void,
        _ctx: FrameContext,
        mut core: CoreRef,
    ) -> Result<Option<VideoFrame>, Self::Error> {
        if reason != ActivationReason::Initial {
            return Ok(None);
        }

        let image = decode(&self.images[n as usize])?;

        let frame = image_to_vs_frame(
            &image,
            &mut core,
        )?;

        Ok(Some(frame))
    }

    const NAME: &'static CStr = c"Read";

    const ARGS: &'static CStr =
        c"files:data[];fpsnum:int:opt;fpsden:int:opt;mismatch:int:opt;debug:int:opt;";

    const RETURN_TYPE: &'static CStr =
        c"clip:vnode;";
}
```

Registration can use:

```rust
declare_plugin!(
    c"xyz.n4o.imgseqs",
    c"imgseqs",
    c"Rust-based Image Sequence Reader",
    (1, 0),
    vapoursynth4_rs::VAPOURSYNTH_API_VERSION,
    0,
    (ImageSequence, None),
);
```

## VapourSynth API Version Caveat

At the time of this design, `vapoursynth4-rs` supports the regular VapourSynth API through API 4.2.

This is sufficient for a normal CPU image source.

Newer R80 Vulkan functionality is separate and is not required for this plugin.

If GPU-resident image output is added later, the R80-specific API may need either:

* newer `vapoursynth4-rs` support; or
* direct use of the lower-level bindings for those APIs.

---

# Native Plugin API

Keep the native API simple:

```python
core.imgseqs.Read(
    files=[...],
    fpsnum=24,
    fpsden=1,
    mismatch=False,
    debug=False,
)
```

Suggested native signature:

```text
Read(
    files:data[];
    fpsnum:int:opt;
    fpsden:int:opt;
    mismatch:int:opt;
    debug:int:opt;
) -> clip:vnode
```

Do not make the native plugin responsible for:

```text
directory enumeration
glob matching
natural sorting
recursive search
```

Those are more conveniently handled by Python.

For example:

```python
from pathlib import Path

files = sorted(
    str(path)
    for path in Path("/images").glob("*")
)

clip = core.imgseqs.Read(files=files)
```

A Python helper package can later provide:

```python
imgseqs.from_folder(
    "/images",
    pattern="*",
    natural_sort=True,
)
```

---

## Decoder Backend Plan

Use `image-rs` as the primary decoder abstraction, with dedicated backends for formats where a specialized implementation provides better coverage or behavior.

```text
image-rs
├── PNG
├── JPEG
├── WebP
├── TIFF
├── GIF
├── BMP
├── AVIF
├── EXR
├── DDS
├── Farbfeld
├── PNM
├── QOI
├── TGA
├── ICO
└── HDR

JPEG XL
└── jxl-rs / image-rs integration

HEIF / HEIC
└── libheif-rs
    └── libheif
        └── libde265 for HEVC/HEIC decoding

JPEG 2000
└── jpeg2k
    └── OpenJPEG
```

### image-rs Configuration

Use `image-rs` primarily for decoding:

```toml
image = { version = "0.25.10", default-features = false, features = [
    "avif-native",
    "bmp",
    "dds",
    "exr",
    "ff",
    "gif",
    "hdr",
    "ico",
    "jpeg",
    "png",
    "pnm",
    "qoi",
    "tga",
    "tiff",
    "webp",
    "rayon",
] }
```

### Format Routing

Format detection should be based primarily on file signatures rather than extensions.

Conceptually:

```rust
match detect_format(path)? {
    ImageFormat::JpegXl => decode_jxl(path),
    ImageFormat::Heif => decode_heif(path),
    _ => decode_image_rs(path),
}
```

The resulting decoder-specific output should be normalized into the plugin's internal `DecodedImage` representation before conversion into a VapourSynth frame.

Format routing itself is `image`'s: `ImageReader::with_guessed_format` sniffs the
signatures and `into_decoder` routes to the codec that registered for them, the
`image` hook of `libheif-rs` and `jxl-image-rs-integration` included. The modules
in `src/formats/` are picked by extension instead, because what they answer is
about the container and not about the samples: a monochrome avif is a file
`image` reads, but one whose format only its `av1C` box states. A module asked
about a file that is not its own answers `None`, and `decoder::probe` and
`decoder::decode` then leave the file to `image`.

### JPEG XL

Use `jxl-rs`, preferably through its `image-rs` integration where practical.

JPEG XL should therefore reuse the normal image decoding pipeline instead of implementing a completely separate VapourSynth path.

### HEIF / HEIC

Use `libheif-rs`.

HEIF is treated separately from AVIF:

```text
AVIF        -> image-rs avif-native
HEIF / HEIC -> libheif-rs
```

For Windows builds, `libheif` can be supplied through vcpkg, preferably using:

```text
x64-windows-static-md
```

HEIC decoding uses the HEVC decoder provided by the `libheif` dependency stack; HEVC encoding support is not required for this plugin.

Colour HEIF/HEIC is read through the `image-rs` decoding hook that `libheif-rs` ships (`libheif_rs::integration::image`), registered once in `src/decoder.rs`. That hook decodes into an interleaved buffer and requires `planes.interleaved` to exist, so it cannot return a monochrome image, which decodes into a single luma plane.

Monochrome HEIF/HEIC is therefore decoded directly through `libheif-rs` in `src/formats/heif.rs`, which asks for `ColorSpace::Monochrome` and packs the planes into the same interleaved layout the frame writer accepts. `src/formats/` holds one module per container that the `image` crate cannot express; `decoder::decode` asks each module whether it handles the image before falling back to `image-rs`.

AVIF still decodes through `image-rs`, but a monochrome avif is handed out in a
corrected format. `image-rs`'s avif decoder converts every file to r,g,b as it
decodes it, so it reports `Rgba8` for a page whose bitstream is monochrome and
the file would come back as `RGB24`, three times the bytes of the single plane it
holds. The container says what the samples are, so `src/formats/heif.rs` gained
`output_format(path, color_type)`, which the probe calls beside the webp one when
it has to build a decoder anyway: it reads the leading boxes of an avif and, when
the `av1C` sequence header sets `mono_chrome`, answers `Gray8` or `Gray16` plus
the alpha an `auxl`/`auxC` entry names, so a monochrome avif with alpha becomes
`La8`/`La16`. The pixels are the ones `image-rs` already produced and they were
always right — all three channels
hold the luma — so this changes the format, not the samples. Decoding avif
through `libheif` instead would copy the plane the file holds, but only a
`libheif` built with a decoder for av1 can read avif at all, and the windows
build linked here has none: `LibHeif::decoder_descriptors(16, None)` reports
`["libde265"]`, and enabling av1 in vcpkg's libheif would add libaom as a native
dependency. Correcting the format keeps one code path on every platform.

A frame can then have fewer planes than the buffer behind it, which is why
`pixel::write_planar` asks `planes_to_write` for the smaller of the two counts
instead of writing every plane the frame has: a `Gray8` frame with an `Rgba8`
buffer is one plane and three channels, and only the first channel is written.
The two counts disagree the other way only on a bug, which is what the error
reports.

The same boxes let the module answer the probe itself, through
`image_info(path)`: `ispe` agrees on a size, `av1C` gives the depth and whether
the samples are monochrome, `colr` of type `prof`/`rICC` means an ICC profile and
`auxC` names the alpha item, which is what `color_type`, `original_color_type`,
`has_icc_profile` and `format` are built from. It declines, and the file goes
back through `AvifDecoder`, on a `clap`, `irot` or `imir` transform it does not
apply, on two sizes or two coded records that disagree, and on a file whose brand
is not `avif`/`avis`, so a file it describes is one whose decode matches what it
promised. `decoder::decode` compares width, height and color type against the
probe all the same and fails the frame if they disagree.

### Native Library Linkage

libwebp is the only native library this crate links on its own; dav1d, libde265,
libheif and everything below them arrive through `libheif-sys`. `build.rs` links
libwebp from its archive wherever one exists, so a plugin built on unix does not
need the platform's `libwebp` at run time:

| build | what `build.rs` emits | what the linker is given |
| --- | --- | --- |
| windows | `vcpkg::find_package("libwebp")` | the archive from the `x64-windows-static-md` tree |
| gnu link editors | `rustc-link-lib=static=webp` | `-Bstatic -lwebp`, which selects the archive |
| apple | `rustc-link-lib=static:+whole-archive=webp` | `-force_load <path to libwebp.a>` |

the apple row is not a preference. `static=` is a hint, apple's `ld` reads no
`-Bstatic` equivalent, and it searches each directory of the search path for
`libwebp.dylib` before `libwebp.a`: homebrew installs both into one directory, so
the hint was silently ignored and a macos build came out needing
`/opt/homebrew/opt/webp/lib/libwebp.7.dylib` and `libsharpyuv.0.dylib`, the
latter being libwebp's own private requirement, which `pkg-config --static`
reports as one more library to link. `+whole-archive` is the form rustc resolves
to a path there, and it is emitted in the local native library position, ahead of
the libraries other crates name, so the archives supply their symbols first.
`-Wl,-dead_strip_dylibs` then drops a dylib that a link interface named but no
symbol came from, which is the shape libheif's generated `libheif.pc` has: its
`Requires.private` names libsharpyuv, and those flags belong to `libheif-sys`.
`.github/workflows/build.yml` asserts the result by reading `otool -L` on macos
and `readelf -d` on linux and rejecting either name.

### Decoder Abstraction

Keep all decoder-specific code behind a common internal interface:

```rust
struct ImageInfo {
    path: PathBuf,
    width: u32,
    height: u32,
    format: PixelFormat,
    color: ColorInfo,
}

struct DecodedImage {
    width: usize,
    height: usize,
    pixels: PixelData,
    color: ColorInfo,
}
```

The rest of the plugin should not care whether a frame originated from `image-rs`, `jxl-rs`, `libheif`, or OpenJPEG.

```text
file
 │
 ▼
format-specific decoder
 │
 ▼
DecodedImage
 │
 ▼
pixel/layout conversion
 │
 ▼
VapourSynth VideoFrame
```

This keeps format-specific dependencies isolated and makes it possible to replace individual codec backends later without changing the VapourSynth-facing implementation.

---

# Lazy Decoding

Do not decode every image when the source is created.

Creation should only probe image metadata.

```text
Read(files)
   │
   ├─ probe file 0
   ├─ probe file 1
   ├─ probe file 2
   └─ ...
         │
         ▼
     VSVideoInfo
```

Actual image decoding happens only when VapourSynth requests a frame:

```text
get_frame(342)
      │
      ▼
files[342]
      │
      ▼
lookahead worker: decode image -> create VSFrame(s) -> cache payload
      │
      ▼
return the finished frame
```

The frame is allocated and filled where the decode happens, not on the thread
that asked for it: a request only hands over a finished frame. `src/clip.rs`
owns that hand off, and [02](improvements/02-frame-write-path.md) has the
measurements.

This allows extremely large image sequences without loading all image data into memory.

---

# Debug Timings

`debug=True` keeps the normal output unchanged and emits information-level
messages through the VapourSynth log. The messages are opt-in because they
add one log entry per requested frame.

At creation time, report:

```text
image probing
format validation
video-format selection
total setup time
```

For each frame, report:

```text
fetch                     waiting for a lookahead worker, zero when the frame
                          was already finished
decode                    decoder open, metadata, buffer allocation, image read
frame format              the name of the format the frame was built in
allocate                  creating the frame in the core
convert                   writing the decoded pixels into the frame planes
properties                the frame property writes
total                     the whole request, on the requesting thread
```

These are wall-clock timings intended for diagnosing a script or decoder.
They are not a replacement for a controlled benchmark because VapourSynth's
cache and scheduler affect the result. Everything between `decode` and
`properties` is measured in the worker that read the file, so those fields
describe one frame's cost rather than the requesting thread's; on a busy pool
they add up to more than `total`. The `fetch` field is the part the requesting
thread waits for: a near-zero value means the frame was ready before it was
asked for.

---

# Parallelism

Each image must be independently decodable.

Do not maintain one persistent decoder that moves sequentially through the input list.

Bad:

```text
decoder
  frame 0
  frame 1
  frame 2
  ...
```

Preferred:

```text
frame 10 -> independently decode image 10
frame 3  -> independently decode image 3
frame 80 -> independently decode image 80
```

This allows VapourSynth to parallelize image decoding naturally.

Conceptually:

```text
VapourSynth scheduler

worker 0 -> frame 10 -> JPEG decoder
worker 1 -> frame 11 -> PNG decoder
worker 2 -> frame 12 -> JXL decoder
worker 3 -> frame 13 -> AVIF decoder
```

The persistent `Sequence` state should therefore be immutable apart from the
prefetch pool described below. Finished frames are cached as shared immutable
buffers, so the clips of one call never decode a file twice and a request never
writes pixels.

## Lookahead prefetching

VapourSynth only drives parallel decoding when its client already requests
several frames at once. Clients that walk a clip sequentially, including a
plain `for n in range(clip.num_frames): clip.get_frame(n)` loop, would leave
every core but one idle for expensive formats such as lossless WebP.

The plugin therefore keeps a small bounded lookahead pool inside the source
filter:

```text
fetch(n)
   │
   ├─ finished frame from the pool -> return immediately
   │
   ├─ otherwise decode n, build its frames and return on the calling thread
   │
   └─ queue n+1 .. n+window for the worker threads
```

The pool caches finished frames rather than decoded images: the worker that
reads a file also creates the frame of every clip of the call, writes the pixels
into it and attaches the source properties. A request therefore never touches
pixels, and its cost is the `fetch` field of the debug log, see
[02](improvements/02-frame-write-path.md). The pool is generic over what it
caches (`Payload` and `Prepare` in `src/prefetch.rs`), so `src/clip.rs` owns the
frame layout and the budget accounting stays in the pool.

The pool is intentionally conservative:

```text
sequential requests only   a seek clears the queue and invalidates old results
bounded worker count       prefetch=N, 0 disables it, default half the logical
                           cores capped at four, at most sixteen workers
small lookahead window     two frames beyond the worker count, at most sixteen,
                           further capped by the byte budget
byte budget                what is ready, queued and being decoded shares one
                           budget: max(192 MiB, window x largest frame) by
                           default, prefetch_memory=N to override it, entries
                           behind the request dropped first
shared between clips       a cached frame is handed out again, so `ReadAlpha`
                           decodes each file once for both clips
always forward progress    a consumer can decode its own frame if workers are busy
```

The worker count is configurable per filter instance:

```python
core.imgseqs.Read(files, prefetch=0)                  # decode synchronously
core.imgseqs.Read(files, prefetch=6)                  # six background decode workers
core.imgseqs.Read(files)                              # automatic worker count
core.imgseqs.Read(files, prefetch_memory=512)         # cap the lookahead at 512 MiB
```

The byte budget is what actually limits the window: a frame is only queued when
it fits, so on very large frames a wide window deepens nothing. The default
budget therefore follows the window (`max(192 MiB, window x largest frame)`),
and `prefetch_memory` replaces it with a fixed number of MiB for callers who
would rather bound memory than depth. `prefetch_memory=0` is rejected: disabling
lookahead is what `prefetch=0` means. `debug=True` prints the resolved budget on
the `create:` line.

Worker threads are joined when the filter instance is freed.

---

# Rayon and image-rs

`image-rs` has a `rayon` feature.

It is safe to use together with VapourSynth.

The feature primarily enables multithreading in some image decoder dependencies, such as expensive AVIF/EXR operations.

Therefore the initial recommendation is:

```text
VapourSynth frame-level parallelism: YES

image-rs rayon feature:           YES

custom Rayon inside get_frame():  NO

lookahead prefetch pool:          YES (bounded, sequential access only)
```

In other words, allow a decoder to internally use Rayon where useful, but do not manually parallelize every `get_frame()` operation using another Rayon job.

There may technically be nested parallelism:

```text
VapourSynth worker
      │
      ▼
decode AVIF
      │
      ▼
decoder internally uses Rayon
```

This is valid. The lookahead pool uses plain worker threads rather than
Rayon jobs, so a decoder's own internal parallelism is unaffected.

The only possible concern is CPU oversubscription and performance, not correctness.

Benchmark before disabling it.

For common PNG/JPEG workloads the `image-rs` Rayon feature is unlikely to materially interfere with VapourSynth scheduling.

---

# Probing

Use `ImageDecoder` rather than fully decoding through `image::open()` during initialization.

Probe:

```text
width
height
decoded color type
original color type
ICC profile
orientation
CICP/color information where available
```

Conceptually:

```rust
let reader = ImageReader::open(path)?
    .with_guessed_format()?;

let mut decoder = reader.into_decoder()?;

let (width, height) = decoder.dimensions();
let color = decoder.color_type();
let original = decoder.original_color_type();
```

This allows the plugin to construct the correct `VSVideoInfo` without decoding the entire image.

A container can also answer the whole probe, which is what `decoder::probe` asks
for first: `formats::heif::image_info` describes an avif from `ispe`, `av1C`,
`colr` and `auxC` and returns before a decoder is built. `image-rs`'s avif
decoder decodes the picture, and the alpha item beside it, inside
`AvifDecoder::new` and keeps them, because the size it reports is the decoded
picture's own, so building one to probe a file decodes that file for the second
time. Probing the 35 page 130 MB avif set of [BENCH.md](BENCH.md) took 6.4 s that
way, 183 ms per file, and takes 2 ms from the container. A file the module cannot
fully predict falls through to the decoder.

One step is then added after the color types above: `decoder::probe` passes them
to `format_override`, which is where a container can correct the format the
decoder reports — the correction the avif probe applies itself when it does
describe the file. `src/formats/webp.rs` uses it to hand back a lossy webp
without alpha as `YUV420P8` when the decoder reports rgb, and
`src/formats/heif.rs` uses it to hand back a monochrome avif as
`Gray8`/`Gray16` when `image-rs` reports `Rgba8` for it. Both read only the
leading boxes of the file, and both answer `None` for a file they do not need to
correct.

---

# VapourSynth Pixel Formats

The normalized image-rs representation maps cleanly to VapourSynth:

```text
image-rs          VapourSynth

L8                GRAY8
L16               GRAY16

RGB8              RGB24
RGB16             RGB48
RGB32F            RGBS

LA8               GRAY8 + alpha
LA16              GRAY16 + alpha

RGBA8             RGB24 + alpha
RGBA16            RGB48 + alpha
RGBA32F           RGBS + alpha
```

For the initial implementation, use:

```text
u8  -> 8-bit VS format
u16 -> 16-bit VS format
f32 -> 32-bit float VS format
```

Do not initially attempt to infer or preserve unusual nominal 10/12-bit representations stored inside `u16`.

That can be added later if a real decoder/use case requires it.

---

# RGB Layout Conversion

`image-rs` RGB buffers are interleaved:

```text
RGBRGBRGBRGBRGB...
```

VapourSynth RGB is planar:

```text
RRRRRRRR...
GGGGGGGG...
BBBBBBBB...
```

The plugin must therefore deinterleave RGB.

Example:

```rust
for (x, pixel) in src.chunks_exact(3).enumerate() {
    r[x] = pixel[0];
    g[x] = pixel[1];
    b[x] = pixel[2];
}
```

Do the equivalent operation for:

```text
RGB8
RGB16
RGB32F
```

This is an extra memory pass, but it is simple and highly SIMD-friendly.

Do not add custom Rayon parallelization for this initially.

Benchmark first.

---

# Constant and Variable Format Clips

During probing, determine whether all images have the same:

```text
width
height
VapourSynth pixel format
```

If everything matches:

```text
RGB24 1920x1080
RGB24 1920x1080
RGB24 1920x1080
```

return a normal constant-format clip.

If not:

```text
RGB24 1920x1080
RGB48 1920x1080
GRAY8 1920x1080
```

the plugin can support a variable-format clip.

Likewise different resolutions can be represented using variable dimensions.

Suggested behavior:

```python
core.imgseqs.Read(files)
```

requires compatible images.

And:

```python
core.imgseqs.Read(
    files,
    mismatch=True,
)
```

permits different formats and/or dimensions.

A mismatch error should be descriptive:

```text
imgseqs.Read: image format mismatch at frame 37

expected:
    RGB24 1920x1080

got:
    RGB48 1920x1080

file:
    /images/0037.png

Use mismatch=True to allow variable-format sequences.
```

---

# Do Not Automatically Split Into Multiple Clips

Do not group different formats into separate output clips by default.

For example:

```text
0 RGB24
1 RGB24
2 RGB48
3 RGB24
4 RGB48
```

automatically producing:

```text
clip A -> 0, 1, 3
clip B -> 2, 4
```

would destroy the original frame ordering.

Instead keep:

```text
frame 0 RGB24
frame 1 RGB24
frame 2 RGB48
frame 3 RGB24
frame 4 RGB48
```

as one variable-format `VideoNode`.

A separate grouping utility can be implemented later if needed.

---

# Color Metadata

Preserve color metadata where it can be represented correctly.

Useful VapourSynth frame properties include:

```text
_Primaries
_Transfer
_Matrix
_Range
_ChromaLocation
_FieldBased
```

For normalized RGB output:

```text
_Matrix = RGB / identity
_Range  = full
```

Set primaries and transfer characteristics when reliable metadata is available.

Do not invent sRGB/BT.709 metadata when the file only supplies an ICC profile that has not been interpreted.

ICC handling can initially be limited to preserving/detecting the profile rather than performing complete color management.

Full ICC conversion can be added later.

---

# Image-Specific Frame Properties

Useful custom properties:

```text
ImgSeqPath
ImgSeqIndex
```

For example:

```python
frame = clip.get_frame(100)

print(frame.props.ImgSeqPath)
print(frame.props.ImgSeqIndex)
```

This makes debugging and tooling much easier.

Avoid underscore-prefixed custom properties because underscore-prefixed names are generally reserved for VapourSynth-defined properties.

---

# Alpha Support

Ordinary VapourSynth RGB clips do not represent alpha as a fourth RGB plane.

Do not silently invent an RGBA video format.

`Read()` keeps its behavior: it returns the RGB or gray clip and ignores
alpha. Alpha is exposed separately:

```python
result = core.imgseqs.ReadAlpha(files=files)
clip, alpha = result["clip"], result["alpha"]
```

A plugin function with several outputs returns a dictionary in Python, keyed
by the names of its declared return type (`clip` and `alpha`).

Internally:

```text
RGBA8
 │
 ├─ RGB -> RGB24 clip
 │
 └─ A   -> GRAY8 alpha clip
```

The same pattern works for 16-bit and float images.

## Implemented design

`ReadAlpha` accepts exactly the same arguments as `Read` (`files`, `fpsnum`,
`fpsden`, `mismatch`, `debug`, `prefetch`, `prefetch_memory`) and declares
`clip:vnode;alpha:vnode;`.

```text
input      color clip   alpha clip
L8         GRAY8        GRAY8  (opaque)
L16        GRAY16       GRAY16 (opaque)
LA8        GRAY8        GRAY8
LA16       GRAY16       GRAY16
RGB8       RGB24        GRAY8  (opaque)
RGB16      RGB48        GRAY16 (opaque)
RGB32F     RGBS         GRAYS  (opaque)
RGBA8      RGB24        GRAY8
RGBA16     RGB48        GRAY16
RGBA32F    RGBS         GRAYS
```

Alpha keeps the sample depth of the source and is taken from the second
channel of `LA` input and the fourth channel of `RGBA` input. Sources without
an alpha channel produce an opaque plane (`255`, `65535`, or `1.0`), so every
frame of an alpha clip carries a meaningful value.

Both clips share frame count, dimensions, frame rate, and frame indexes, and
both are built from one `Sequence`. The prefetch pool caches the finished frames
of both clips, so a file is decoded once even when both clips ask
for the same frame. With `mismatch=True`, both clips use variable format
information and a frame whose file has no alpha still yields an opaque plane.

The alpha clip is marked with `ImgSeqAlpha=1`; the color clip is not, and the
alpha clip never gets `_Matrix` because it is not an RGB clip. Both clips keep
the usual `ImgSeq*` properties, and `Read` output is unchanged.

## Second output node

`vapoursynth4-rs` 0.5.1 can only create a node through
`Core::create_video_filter`, which stores it under the fixed `clip` key of the
output map, and its `VideoNode::new` helper has an inverted null check that
makes it unusable. `ReadAlpha` therefore:

1. creates the alpha node first and reads it back with `get_video_node("clip", 0)`;
2. deletes that key, which releases the reference the map held;
3. creates the color node, which takes over the primary `clip` key;
4. publishes the alpha node again with `consume_node("alpha", …)`.

The reference counts stay balanced and no dependency has to be patched or
vendored.

## Validation

`cargo test` covers the pixel plumbing, the prefetch pool (one decode shared by
two consumers, failed decodes retried, frames of a variable sequence kept
apart), the clip format mapping, and the container reader an avif is described
from (its size, bit depth, alpha item, ICC box, file type, the boxes that move
the picture, the header limit, and a fixture probed from its own container).
`tests/readalpha.vpy` covers the plugin itself against the fixtures written by
`tests/make-alpha-fixtures.py`: LA/RGBA 8/16-bit and float, opaque fills, mixed
alpha presence, variable depth, `ImgSeqAlpha`, `_Matrix`, prefetch variants,
the shared decode, and the monochrome containers — `mono-alpha.heic`,
`mono-alpha.avif` and the ten bit `mono-alpha-10.avif`, whose planes are the
exact samples of the 7x5 `mono-alpha.png` they are encoded from — beside the
colour containers (`alpha-rgba8.heic`, `alpha-rgba8.avif`) that must keep
`RGB24`.

---

# Error Handling

Use normal Rust errors internally.

Example:

```rust
#[derive(thiserror::Error, Debug)]
enum ImgSeqError {
    #[error("failed to open image {path}: {source}")]
    Open {
        path: PathBuf,
        source: image::ImageError,
    },

    #[error(
        "format mismatch at frame {frame}: expected {expected}, got {actual}"
    )]
    FormatMismatch {
        frame: usize,
        expected: String,
        actual: String,
    },

    #[error("unsupported image format: {0}")]
    UnsupportedFormat(String),
}
```

Convert errors into VapourSynth filter errors at the plugin boundary.

Errors should include the frame index and source path whenever possible.

---

# Suggested Project Layout

```text
vs-imgseqs/
├─ Cargo.toml
└─ src/
   ├─ lib.rs
   ├─ source.rs
   ├─ clip.rs
   ├─ prefetch.rs
   ├─ decoder.rs
   ├─ formats/
   ├─ pixel.rs
   ├─ color.rs
   └─ error.rs
```

Responsibilities:

```text
lib.rs
    plugin declaration
    function registration

source.rs
    Sequence
    Read / ReadAlpha filters
    Filter implementations
    VSVideoInfo handling

clip.rs
    the clips a sequence hands out
    building and caching their frames (the pool's Prepare and Payload)

prefetch.rs
    lookahead pool, window and byte budget
    worker threads

decoder.rs
    image-rs decoder creation
    JPEG XL registration
    metadata-only probing
    image decoding

formats/
    containers image-rs cannot express, one module each
    and the format corrections for the ones it can

pixel.rs
    image-rs -> VapourSynth mapping
    interleaved -> planar conversion

color.rs
    ICC/CICP/frame properties

error.rs
    plugin errors
```

---

# Initial Implementation Scope

The first version should remain deliberately small.

Implement:

1. `files:data[]`.
2. PNG/JPEG/WebP/TIFF/etc. through `image-rs`.
3. JPEG XL through the Rust JXL integration.
4. GRAY8/GRAY16.
5. RGB8/RGB16/RGB32F.
6. Lazy per-frame decoding.
7. Constant-format clips.
8. Optional variable format/resolution via `mismatch=True`.
9. Basic color metadata.
10. `ImgSeqPath` and `ImgSeqIndex`.
11. VapourSynth-managed frame-level concurrency.
12. Keep the `image-rs` Rayon feature enabled initially.
13. `ReadAlpha` for a separate alpha clip.

Defer:

```text
full ICC color management
GPU/Vulkan output
manual SIMD
custom Rayon inside get_frame()
native YUV preservation
filesystem globbing inside the native plugin
format-based clip grouping
special 10/12-bit preservation
```

---

# Final Architecture

```text
Python
  │
  │ explicit ordered file list
  ▼
core.imgseqs.Read() / core.imgseqs.ReadAlpha()
  │
  ▼
Sequence (shared by every clip of one call)
  │
  ├── immutable ImageInfo[]
  │
  ├── bounded lookahead prefetch pool
  │       └── cached finished frames, one per clip of the call
  │
  └── VSVideoInfo per clip
         │
         │ VapourSynth requests frame N
         ▼
      files[N]
         │
         ▼
     image-rs                    or a module of src/formats/
       │   │
       │   └── jxl-rs for JPEG XL
       │
       ▼
   normalized image
   RGB / Gray
       │
       ▼
interleaved -> planar
       │
       ▼
VapourSynth VideoFrame  <- built by the worker that decoded, not by the request
       │
       ├── pixel data
       ├── color properties
       ├── ImgSeqPath
       └── ImgSeqIndex
```

The central design principle is:

> Let VapourSynth schedule frames, let image-rs decode individual images,
> keep each frame completely independent, and only overlap decoding when a
> client walks the clip sequentially.

This gives the plugin simple random access, good multicore scaling, minimal shared state, and a mostly safe Rust implementation.
