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
decode image
      │
      ▼
create VSFrame
      │
      ▼
return
```

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
decoder open
decoder metadata
decoded-buffer allocation
image read
frame-format lookup
frame allocation
planar conversion (directly into the VapourSynth planes)
frame-property writes
total frame time
```

These are wall-clock timings intended for diagnosing a script or decoder.
They are not a replacement for a controlled benchmark because VapourSynth's
cache and scheduler affect the result. The per-frame `decode` value includes
waiting for the lookahead pool, so a near-zero value means the frame was
already decoded in the background.

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

The persistent `ImageSequence` state should therefore be immutable apart
from the prefetch pool described below; frame data itself is never shared
between requests.

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
   ├─ ready frame from the pool -> return immediately
   │
   ├─ otherwise decode n on the calling thread
   │
   └─ queue n+1 .. n+window for the worker threads
```

The pool is intentionally conservative:

```text
sequential requests only   a seek clears the queue and invalidates old results
half the logical cores     at most four worker threads
small lookahead window     at most six frames ahead
decoded-byte budget        192 MiB of ready frames
always forward progress    a consumer can decode its own frame if workers are busy
```

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

For the initial `Read()` implementation, alpha may simply be ignored.

Later an API can expose alpha separately:

```python
clip, alpha = core.imgseqs.ReadAlpha(files)
```

Internally:

```text
RGBA8
 │
 ├─ RGB -> RGB24 clip
 │
 └─ A   -> GRAY8 alpha clip
```

The same pattern works for 16-bit and float images.

## RGBA implementation plan

Keep `Read()` compatible: it continues to return the RGB or gray clip and
ignores alpha. Do not represent alpha as a fourth VapourSynth RGB plane.

Add a separate `ReadAlpha()` entry point with the same file, frame-rate,
mismatch, and debug options. Its planned result is:

```text
RGB/GRAY clip + GRAY alpha clip
```

The RGB/gray clip and alpha clip must share frame count, dimensions, frame
rate, and frame index. Alpha should keep the source sample depth: 8-bit input
produces GRAY8, 16-bit input produces GRAY16, and float input produces GRAYS.
For LA input, the first channel is the image and the second is alpha; for
RGBA input, the fourth channel is alpha.

Implementation steps:

1. preserve the alpha-channel description in the internal decoded-image data;
2. split RGB/gray and alpha in one conversion pass where practical;
3. create the second clip through the plugin's multi-output function API;
4. apply the existing mismatch rules to both outputs;
5. copy the existing source properties to both clips and add an alpha marker;
6. add tests for LA/RGBA 8/16-bit and float data, including variable-format
   sequences.

This keeps the current `Read()` behavior stable while leaving room for a
proper alpha output instead of silently changing the meaning of RGB clips.

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
   ├─ decoder.rs
   ├─ probe.rs
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
    ImageSequence
    Filter implementation
    VSVideoInfo handling

decoder.rs
    image-rs decoder creation
    JPEG XL registration
    image decoding

probe.rs
    metadata-only probing

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

Defer:

```text
alpha output
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
core.imgseqs.Read()
  │
  ▼
ImageSequence
  │
  ├── immutable ImageInfo[]
  │
  ├── bounded lookahead prefetch pool
  │
  └── VSVideoInfo
         │
         │ VapourSynth requests frame N
         ▼
      files[N]
         │
         ▼
     image-rs
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
VapourSynth VideoFrame
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
