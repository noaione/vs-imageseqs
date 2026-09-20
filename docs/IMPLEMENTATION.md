# Rust VapourSynth Image Sequence Reader

This is the design document. It was written before the plugin existed and is kept
in the order it was written: what the build does is described in these words
where it agrees, and annotated where it does not. Each change the doc asks for
is one plan in [docs/improvements/](improvements/README.md) — 06 for a jpeg 2000
backend, 07 for the two `image` features this doc's own configuration block
names, 08 for the colour properties, 09 for the exif orientation, 10 for the
nominal 10/12-bit depths, 12 for a heif's or an avif's own yuv planes, and 13
for embedded ICC export — and
each of those carries its status, so that index is the answer to "what is left":
its `plans` table for what is still a plan, and its `deferred` section for what
the landed ones left over.

The ICC work is [13](improvements/13-icc-color-management.md). It is
implemented: `icc_profile=False` keeps the current frame properties, while
`icc_profile=True` exposes the embedded bytes as the standard `ICCProfile`
binary frame property for a downstream color-management filter.

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
│     ├── TIFF
│     ├── EXR
│     └── other supported formats, a monochrome AVIF among them
│
├── libwebp
│     └── WebP, lossy files as planar yuv
│
├── dav1d
│     └── AVIF, the item's own planes, read by `src/formats/avif.rs`
│
├── libheif-rs
│     └── HEIF / HEIC pages, as planar yuv
│
└── jxl
      └── JPEG XL, decoded by `src/formats/jxl.rs`
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
├── TIFF
├── GIF
├── BMP
├── EXR
├── DDS
├── Farbfeld
├── PNM
├── QOI
├── TGA
├── ICO
├── HDR
└── a monochrome AVIF (the colour path is below)

JPEG XL
└── jxl
    └── decoded by `src/formats/jxl.rs`

AVIF
└── dav1d
    └── the primary item's own planes, described and fed by
        `src/formats/avif.rs`

HEIF / HEIC
└── libheif-rs
    └── libheif
        └── libde265 for HEVC/HEIC decoding

WebP
└── libwebp

JPEG 2000
└── jpeg2k
    └── OpenJPEG
```

Everything above is in the build. JPEG 2000 is implemented in
`src/formats/jp2.rs` through `jpeg2k` and its vendored OpenJPEG sources; the
probe walks the JP2/SIZ headers before decode. The `image` feature list below is
the other half of this picture, and `dds` and `ff` are enabled as described by
[07](improvements/07-dds-and-farbfeld.md).

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

`webp` stays in that list because `image` is still what identifies and probes a
webp file: the format is recognised from the file's magic and the size, the
colour type, the exif orientation and the icc profile all come from its decoder.
Only the pixels are decoded elsewhere, by `src/formats/webp.rs`, which
[04](improvements/04-webp-decoder.md) explains.

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
`image` hook of `libheif-rs` included. The modules
in `src/formats/` are picked by extension instead, because what they answer is
about the container and not about the samples: a monochrome avif is a file
`image` reads, but one whose format only its `av1C` box states. A module asked
about a file that is not its own answers `None`, and `decoder::probe` and
`decoder::decode` then leave the file to `image`.

### JPEG XL

JPEG XL goes through the `jxl` crate directly, in `src/formats/jxl.rs`, and not
through an `image` integration: `image` has no jpeg xl format of its own, and the
information a frame needs is the codestream's. The probe is a header read that
stops in the decoder's `WithImageInfo` state and records the size, the nominal bit
depth, the orientation and whether an icc profile is attached; the decode asks
for the sample layout the frame wants and draws one frame into the buffer it is
handed.

The orientation is the one field the decoder does not leave to the plugin: it
renders the picture the file's code describes, and there is no option in the
crate that turns that off. So `ImgSeqOrientation` reports the code the file
states, `apply_rotation=True` writes the rendered raster unchanged, and
`apply_rotation=False` writes it through the code *inverted*, which is the stored
picture. See [11](improvements/11-jxl-direct.md).

### HEIF / HEIC

Use `libheif-rs`.

HEIF is treated separately from AVIF:

```text
AVIF        -> dav1d, through `src/formats/avif.rs`
HEIF / HEIC -> libheif-rs
```

For Windows builds, `libheif` can be supplied through vcpkg, preferably using:

```text
x64-windows-static-md
```

HEIC decoding uses the HEVC decoder provided by the `libheif` dependency stack; HEVC encoding support is not required for this plugin.

Every HEIF/HEIC page is decoded through `libheif-rs` in `src/formats/heif.rs`.
Asking for `ColorSpace::Rgb` made libheif run its own yuv to rgb conversion into
an interleaved buffer, which the plugin then split into three planes again; the
module asks for `ColorSpace::YCbCr` with the chroma the file states instead, so
the planes libheif decodes are the ones the frame is filled from. A 4:2:0 page
costs half the bytes and its depth is carried by the format, and no conversion
happens in either direction — `docs/improvements/12-heif-avif-yuv-output.md` has
the measurement. The planes it reads are `planes.y`/`cb`/`cr`/`a`, each a
`Plane` with its own width, height and stride, which the module already walked
for the monochrome path, and its `color_profile_nclx()`,
`luma_bits_per_pixel()` and `has_alpha_channel()` answer the rest of the probe.

Two things did not move with it. libheif applies the container's `irot`/`imir`
itself, so a rotated heic is still handed out display-oriented with
`ImgSeqOrientation` reporting 1, the one promise of
[12](improvements/12-heif-avif-yuv-output.md) that is not kept: reporting the
code means reading the transform boxes, each of which also states a size the
module has to keep straight. And the alpha item is decoded whenever the probe
found one, for `Read` as well as `ReadAlpha`, because the colour type a probe
reports is decided before either clip is asked for a frame. libheif decodes the
whole handle at once, so asking for the alpha separately would open it twice.

Monochrome HEIF/HEIC asks for `ColorSpace::Monochrome` and writes the single
plane into a `Gray8`/`Gray10`/`Gray16` frame. `src/formats/` holds one module per
container that the `image` crate cannot express; `decoder::decode` asks each
module whether it handles the image before falling back to `image-rs`.

### AVIF

AVIF goes through the `dav1d` crate in `src/formats/avif.rs`, and not through
`image`'s avif decoder: that decoder converts to rgb in rust, always reports four
channels whatever the file holds, always decodes the alpha item, and left-aligns
the samples into 16 bit words, which is where a ten bit page used to lose its
depth into `RGB48`. `dav1d` is the same decoder `image` already linked for avif,
so this adds a direct dependency and no native code; the picture arrives as the
planes the bitstream holds, **right aligned**, which is what a VapourSynth ten
bit frame holds too — no shift, no conversion.

The module is in the shape of `formats/heif.rs` and `formats/jxl.rs`:

- **the probe** is a box walk that answers `image_info(path)` without decoding:
  the leading boxes give the brand, `meta` gives `iprp`, its `ipco` properties
  and its `ipma` associations (which index the property list from one), `iloc`
  the extents and `pitm` the primary item, and `iref`'s `auxl` reference with
  its `auxC` property names the alpha item beside it. `ispe` agrees on a size,
  `av1C` gives the depth and the subsampling, and `colr` of type `nclx` gives
  the matrix and the range when the file has one. **34 of the 35 files in
  `sandbox/avif` have none**, so the value comes from the AV1 sequence header's
  `color_config` at the head of the item's own bytes: a bit read of one OBU,
  where the sequence header's optional `timing_info` and `decoder_model_info`
  blocks have to be stepped over first.
- **the decode** feeds the primary item's payload to `dav1d::Decoder::send_data`,
  takes the `Picture`, and copies `picture.plane(Y/U/V)` into the planes the
  frame owns, using `picture.pixel_layout()` for the chroma geometry and the
  layout the frame has for the rest. A **grid** — a primary item that is a
  `dimg` reference to tiles — is refused with a clear error rather than half
  read.
- **a monochrome item is still `image`'s.** `avif::handles` takes a file whose
  probe answered a yuv format and whose extension is `avif`, so a monochrome
  avif keeps [05](improvements/05-monochrome-heif.md)'s corrected
  `Gray8`/`Gray16` and the `image` decode behind it, with the format narrowed to
  the depth `av1C` states by [10](improvements/10-nominal-bit-depth.md): the
  samples arrive left aligned in a sixteen bit word and the writer shifts them
  down, which is the sample the file holds. A monochrome item is a single plane
  dav1d hands over directly, which is the obvious next step for that plan rather
  than a gap here.
- **the alpha item** is decoded only when the colour type says the file has one,
  into the gray format of the same depth (`YUV420P10` gives a `Gray10` alpha),
  and its own bit depth is checked against that format before it is copied.

A frame can then have fewer planes than the buffer behind it, which is why
`pixel::write_planar` asks `planes_to_write` for the smaller of the two counts
instead of writing every plane the frame has: a `Gray8` frame with an `Rgba8`
buffer is one plane and three channels, and only the first channel is written.
The two counts disagree the other way only on a bug, which is what the error
reports.

### WebP

`image`'s `webp` feature is `image-webp`: pure rust, single threaded, and it
decodes into a canvas of its own which it then copies into the caller's buffer.
libwebp is the format's reference decoder, it is vectorised, and its entry
points write into a buffer and a stride the caller picks, so the canvas and that
copy both go. `src/formats/webp.rs` takes every webp file that way, picked by
extension and by the chunk headers a file starts with, and only the pixel decode
moved — see above for what `image` still answers. The size libwebp reads from the
bitstream is checked against the probe the way the `image` path checks its own
decoder.

Lossy webp stores yuv 4:2:0, and libwebp decodes it either way. A lossy file
without an alpha channel is decoded into its own planes and handed out as
`YUV420P8`: half the bytes per image, no yuv to rgb conversion that a graph
would only undo, and twice as many frames in the lookahead budget.
`formats::webp::output_format` is what tells the probe, and it is one of the two
format corrections `# Probing` describes. Everything else keeps the interleaved
layout the `image` path produced, because lossless webp is rgb by definition and
a file with an alpha channel needs the buffer its alpha plane is read from.

A yuv frame is tagged `_Matrix = 5` (`bt470bg`) and `_Range = 0` (limited): vp8
defines the bt.601 coefficients for the limited range, libwebp's own yuv to rgb
conversion uses that pair, and ffmpeg's webp decoder reports the same two values
for the same bitstreams. `src/color.rs` applies that pair to the yuv family and
leaves rgb and gray on `_Matrix = RGB` / `_Range = full`.

[BENCH.md](BENCH.md)'s per stage table has what both changes left behind: the
webp copy is the 5 ms of a frame whose planes arrive in the layout the frame
already wants, which is why [02](improvements/02-frame-write-path.md) could move
the rest of the write into the pool without the copy being the floor any more.

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

The rest of the plugin should not care whether a frame originated from `image-rs`, `jxl`, `libheif`, or OpenJPEG.

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

At creation time, one line reports what the call cost and what it decided:

```text
create: frames= clips= probe= validate= prefetch= prefetch_memory= total=
```

`probe` is every file's size, format and metadata, `validate` is the mismatch
check over them, `prefetch` and `prefetch_memory` are the pool the arguments
resolved to — the byte budget follows the window, see below — and `total` is the
whole call. There is no separate measurement for selecting the video format:
sizing a clip is a field assignment, not a pass over the files.

For each frame, one line names the frame and reports the fields the request was
made of:

```text
frame 342 ('…p342.png') (RGB24): fetch= decode= (open= metadata= buffer= read=)
                                  format= allocate= convert= properties= total=
```

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

Of that list, every field is read today. The size, both colour types and the
presence of an icc profile come from the decoder, the orientation from the exif
block and the picture is turned to it by default — [09](improvements/09-exif-orientation.md)
is implemented, and `apply_rotation=0` is what asks for the stored picture — and
the colour information comes from the container, which is
[08](improvements/08-color-metadata.md): an `nclx` box, a `cICP` chunk or a jxl
codestream header, read once per file and kept in `ImageInfo` as the h.273 code
points the file states. The last field is the nominal bit depth: the probe keeps
what the container states — `av1C`, libheif's handle or the jxl codestream
header — and the format is the one that names that depth rather than the word
the decoder reports, which is
[10](improvements/10-nominal-bit-depth.md). A container that states none, and
the `image` decode behind it, leaves the word alone.

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
for first: `formats::avif::image_info` describes an avif from `ispe`, `av1C`,
`iloc`, `ipma` and the AV1 sequence header, and returns before a decoder is
built, and `formats::heif::image_info` answers a heic from the libheif handle.
`image-rs`'s avif decoder decodes the picture, and the alpha item beside it,
inside `AvifDecoder::new` and keeps them, because the size it reports is the
decoded picture's own, so building one to probe a file decodes that file for the
second time. Probing the 35 page 130 MB avif set of [BENCH.md](BENCH.md) took
6.4 s that way, 183 ms per file, and takes 2 ms from the container. A file a
module cannot fully predict falls through to the decoder.

One step is then added after the color types above: `decoder::probe` passes them
to `format_override`, which is where a container can correct the format the
decoder reports. `src/formats/webp.rs` uses it to hand back a lossy webp
without alpha as `YUV420P8` when the decoder reports rgb, and
`src/formats/avif.rs` uses it for a monochrome avif, which `image-rs` reports as
`Rgba8` and whose `av1C` says is a `Gray8` to `Gray16` format at the depth the
box names. Both read only the leading
boxes of the file, and both answer `None` for a file they do not need to
correct. A colour avif or heic never reaches that step: `formats::avif::handles`
and the heif module answer the probe themselves, yuv and all, so there is nothing
left for `image-rs` to report.

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

planar yuv 4:2:0   YUV420P8
planar yuv 4:2:0   10 bit  YUV420P10
planar yuv 4:2:2   YUV422P8 / YUV422P10
planar yuv 4:4:4   YUV444P8 / YUV444P10 / YUV444P12 / YUV444P16

GRAY10            10-bit gray, for the alpha of a 10-bit yuv page
GRAY12            12-bit gray, for the alpha of a 12-bit yuv page
```

A colour type is a word rather than a depth. `L8`/`RGB8` are the eight bit
formats and `L16`/`RGB16` the sixteen bit ones, so the decoder's answer alone
gives

```text
u8  -> 8-bit VS format
u16 -> 16-bit VS format
f32 -> 32-bit float VS format
```

and a file whose nominal depth is 9 to 15 arrives in the 16-bit word with its
samples scaled onto the whole of it: a ten bit avif came back as `RGB48` with
`max=65472`, 0.4% short of the scale the format claimed, and with the wrong
answer for a graph that wanted `RGB30`. What closes that is the container's own
depth, which each probe reads while it is already walking the boxes or the
codestream header (`av1C` and the av1 sequence header for avif, libheif's
`luma_bits_per_pixel` for heif, the jxl image header): `PixelFormat::at_depth`
narrows the word to the format that names that depth, and the writer moves every
sample down by the difference, which recovers it exactly. VapourSynth names
every depth from eight to sixteen in both families, so the mapping is

```text
depth 9..=16, gray   GRAY9 .. GRAY16, the depth the file states
depth 9..=16, rgb    RGB27 .. RGB48,  three samples to the pixel
every other format   unchanged: a yuv format is named after the depth it holds,
                     and a depth outside 8..=16 leaves the word alone
```

[10](improvements/10-nominal-bit-depth.md) is the plan that landed this, and the
depths it does not have a fixture for are recorded there. What the plugin asks
no depth of — png, jpeg, tiff, webp, dds, and the r,g,b hand-outs of a heif page
— keeps the word it arrives in, which is what the sandbox's 16-bit png and tiff,
both full scale, already mean.

The yuv formats are the exception, and they are why
[12](improvements/12-heif-avif-yuv-output.md) needed plan
[08](improvements/08-color-metadata.md) first: a yuv page whose container states
a matrix the properties can name is handed out as the planes the file holds,
`YUV420P8` for the sandbox avif and heic sets and `YUV444P10` for a ten bit page,
with `_Matrix` and `_Range` saying what those planes are. `YUV420P8` was added
for lossy webp by [03](improvements/03-webp-yuv-output.md), and
[12](improvements/12-heif-avif-yuv-output.md) extended the table with the
depths and the chroma the two new readers can produce, together with the gray
depths an alpha plane of those pages needs. A file that states no usable matrix
— avif's code 2, or nothing at all — keeps the rgb hand-out it had, which is the
rule that keeps `sandbox/hitokage-sample` on its rgb path; the samples of such a
file are still narrowed to the depth its `av1C` states, which is what moved its
ten and twelve bit pages to `RGB30` and `RGB36`.

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

A yuv frame skips all of it: `src/formats/webp.rs` hands out planes that are
already planar and `pixel::write_decoded_planes` copies them plane by plane,
which is why the webp row is the cheapest copy in [BENCH.md](BENCH.md)'s per
stage table.

---

# Applying the exif orientation

A file's exif orientation is applied by default, and `apply_rotation=0` is the
argument that asks for the stored picture. Three facts make this more than a
write-time detail:

- orientations 5 to 8 swap width and height, so the size the clip is built from
  is the *transformed* one. `decoder::probe` reports both sizes and
  `ImageInfo`/`DecodedImage` hand out `output_width()`/`output_height()` beside
  the stored pair, which is what `source.rs` sizes the clip from,
  `clip.rs::expected_bytes` sizes the lookahead budget from, and `mismatch`
  compares. A folder that mixes a rotated page with an upright one therefore
  fails at creation, before a frame exists.
- the eight codes are the identity, a mirror per axis and a transpose, composed
  — `pixel::Transform` is those three flags and `source_of(x, y)` is the whole
  mapping. The mirrors are applied in the frame's own coordinates first and the
  transpose swaps what is left, which is what makes exif 5 the plain transpose
  and exif 6 a rotation; the reverse mapping is wrong and was written first.
- a transform that transposes reads *across* the decoder buffer while the frame
  is written *along* it, which the identity path never does. `pixel::for_each_block`
  walks the destination in square blocks of `TRANSFORM_BLOCK` samples so the
  reads of one block stay inside whole cache lines and the block's other rows
  reuse them; one destination sample per pass measured 9x the identity write on
  12 megapixel pages before blocking. The codes that do not transpose take the
  row-wise path instead, which is a row copy plus `reverse_samples` for a mirror,
  and a transpose of an interleaved buffer is one walk that fills every channel
  rather than one pass per plane. In the planar writer the sample size is a
  constant of the dispatch (`write_sample_planes::<T>`) and not a value the walk
  carries: a run-time sized move is a call into the copy routine per sample,
  which measured 3.3x worse on a transposed yuv plane than a constant one.
  [09](improvements/09-exif-orientation.md) has the numbers.

On an odd sized `4:2:0` page a subsampled plane is rounded up by the decoder and
down by the frame, so a transformed write there is approximate in the same way
the untransformed one already was; the fit test is made against the source turned
the way the frame was, which keeps the reads inside the decoder's buffer either
way. [BENCH.md](BENCH.md) and [09](improvements/09-exif-orientation.md) have the
numbers.

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

What is written today is six of those six. Every frame gets `_FieldBased=0`
and `_Range`; `_Matrix` is written for the RGB and YUV families only, because a
gray frame has no matrix to name:

```text
RGB / GRAY   _Matrix = RGB (identity)   _Range = full
yuv          _Matrix = bt470bg           _Range = limited
```

The rgb and gray pair is the meaning of the formats themselves. The yuv pair is
what [03](improvements/03-webp-yuv-output.md) established for a lossy webp,
whose vp8 defines those coefficients and for which ffmpeg reports the same two
values: it is the fallback for a yuv frame that states nothing of its own.

[08](improvements/08-color-metadata.md)
is implemented, so a file that states its own matrix and range is handed out with
what it states, while an rgb frame keeps the identity and the full range whatever
the file says, because the file is describing the yuv it codes and not the r,g,b
this plugin hands out.
[12](improvements/12-heif-avif-yuv-output.md) extended that to hand out the yuv
planes themselves, so a colour avif or heic page whose container states a matrix
the enum can name is tagged with the file's own values — matrix 6 and full range
for the sandbox sets — rather than the fallback pair. `_Primaries` and
`_Transfer` come from the same read and
are written for every family, since they describe a picture and not a plane
layout: a png that states sRGB primaries is describing the rgb it holds. The
containers read are an `nclx` colour box in avif and heif, a `cICP` chunk in png,
the AV1 sequence header (`color_config`) when an avif has no `colr` box at all,
and the codestream header in jxl, and the code points are the h.273 numbers the
VapourSynth enums already use, so the mapping is the identity where VapourSynth
names the code and the property stays unset where it does not. Code 2
(`unspecified`) is a statement rather than a description and leaves the property
unset, which is also what happens to a file with no box at all, and which is why
such a file keeps its rgb hand-out instead of gaining planes nothing labels.

`_ChromaLocation` is written only for a subsampled frame whose container names a
position: `chroma_sample_position` in an av1 sequence header, whose three values
map onto `_ChromaLocation`'s left, top-left and top-collocated. It is
`unknown` (0) in every sandbox file and absent from every other container the
plugin reads, so the property stays off a frame rather than guessing a position
that would misplace the chroma of the next resize — the same rule as code 2,
applied to a sample position. A graph that needs one passes it into its own
resize. The rule
this doc already had stands and is implemented: a file that supplies only an icc
profile is never turned into sRGB or BT.709 metadata. `ImgSeqHasICC` says it is
there, and `icc_profile=True` additionally exposes the exact raw bytes as
`ICCProfile`; full icc conversion stays in the defer list below.

An exif orientation is metadata of the same kind, and [09](improvements/09-exif-orientation.md)
is implemented: the code reaches every frame as `ImgSeqOrientation` either way,
and the picture is transformed by default. `apply_rotation=0` hands out the
stored picture, which is the one case where the property and the frame's size
say different things about the same file.

---

# Image-Specific Frame Properties

Useful custom properties — all seven are written, and the last one only on an
alpha clip:

```text
ImgSeqPath               the file this frame was read from
ImgSeqIndex              its position in the list
ImgSeqOriginalColorType  the decoder's own colour type, before any correction
ImgSeqHasICC             whether the file carries an icc profile
ICCProfile               raw icc bytes when icc_profile is enabled
ImgSeqOrientation        the exif orientation code, as the file stores it
ImgSeqAlpha              present on an alpha clip only, always 1
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
`fpsden`, `mismatch`, `apply_rotation`, `icc_profile`, `debug`, `prefetch`,
`prefetch_memory`) and declares
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

The rows are the color type a decoder reports, and a source whose container
states a depth above eight and below sixteen is one of the sixteen bit rows in
name only: [10](improvements/10-nominal-bit-depth.md) hands it out as the format
naming that depth — a ten bit page is `RGB30` with a `GRAY10` alpha, not `RGB48`
with a `GRAY16` one — and its samples are right aligned at that depth.

Alpha keeps the sample depth of the source and is taken from the second
channel of `LA` input and the fourth channel of `RGBA` input. Sources without
an alpha channel produce an opaque plane (`255`, `65535`, or `1.0` — or, on a
frame narrowed to its own depth, that depth's maximum, `1023` for a ten bit
one), so every frame of an alpha clip carries a meaningful value.

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

`cargo test` covers the pixel plumbing — including the format each nominal
depth maps to and the shift that recovers the sample from a wider word — the
prefetch pool (one decode shared by two consumers, failed decodes retried,
frames of a variable sequence kept apart), the clip format mapping, and the
container readers a file is described from: the avif walk (its size, bit depth,
alpha item, ICC box, file type, the properties an `ipma` table associates from
one, the boxes that move the picture, the header limit, the AV1 sequence header
with and without a colour description, and seven real fixtures probed or decoded
from their own containers), the heif handle and its own colour read, the png
chunk walk and the jxl header.

`tests/readalpha.vpy` covers the plugin itself against the fixtures written by
`tests/make-alpha-fixtures.py`: LA/RGBA 8/16-bit and float, opaque fills, mixed
alpha presence, variable depth, `ImgSeqAlpha`, `_Matrix`/`_Range`, prefetch
variants, the shared decode, the monochrome containers — `mono-alpha.heic`,
`mono-alpha.avif` and the ten bit `mono-alpha-10.avif`, whose planes are the
exact samples of the 7x5 `mono-alpha.png` they are encoded from — and the colour
containers: `alpha-rgba8.avif`, which states the identity matrix and stays
`RGB24`, beside `alpha-rgba8.heic`, which states bt.601 and is now `YUV420P8`.

[12](improvements/12-heif-avif-yuv-output.md) added a yuv section to it: the
three crop fixtures (`avif-yuv420p`, `avif-yuv422p`, `avif-yuv444p10`, the last
at `YUV444P10`) are read as one clip with `mismatch=True` and each frame is
checked for its format, its size, its chroma plane's own size, its `_Matrix` (6)
and `_Range` (1), and for `_ChromaLocation` staying off a file that states no
position; the 4x4 `alpha-yuv420p.avif` is checked plane by plane against the
`yuv-rgba8.png` it is encoded from, alpha included; the opaque fill is checked to
be the depth's own maximum (255 on the 8-bit page, 1023 on the ten bit one); and
a yuv page beside an rgb one is rejected without `mismatch`.

[10](improvements/10-nominal-bit-depth.md) added the depth section: the three
hand-made jxl fixtures — `jxl-gray10.jxl` and `jxl-gray12.jxl`, four by three
grayscale holding `1..12`, and `jxl-rgba10.jxl`, the same numbers in three planes
with `100..111` in its alpha — are read as `Gray10`, `Gray12` and `RGB30` with a
`Gray10` alpha plane, and the ten bit rows of the monochrome and yuv tables state
the depth's own samples, `round(value * 1023 / 255)`, which is what `avifenc`
stored, rather than the same numbers left aligned in the sixteen bit word those
rows used to be checked in. The fixture set now also has native-depth grayscale
PNG sources, 10- and 12-bit monochrome HEIF pages, and a 12-bit monochrome AVIF
with alpha; `tests/readalpha.vpy` checks their right-aligned color and alpha
samples, including the opaque fills.

---

# Error Handling

Use normal Rust errors internally, and convert them into a VapourSynth filter
error at the plugin boundary. Every message crosses one boundary and nothing
inside the plugin matches on a variant, so the error is a message and not an enum:

```rust
/// Error type passed back to VapourSynth.
pub struct ImgSeqError {
    message: CString,
}
```

`ImgSeqError::from_display` wraps anything that implements `Display`, which is
how a decoder's own error is carried through unchanged. Messages include the
frame index and the source path whenever the failure is about one file; the
mismatch error names the index, both paths, both sizes and both formats:

```text
frame 12 ('images/p012.png') has 1404x2000 RGB24, expected frame 0 ('images/p000.png') to be 1404x1998 RGB24
```

It does not tell the caller to pass `mismatch=True`, which is the argument that
would allow it. That hint is the one thing this section asks for that the code
does not do; the alternative is to leave the message as it is and let the
argument list be the documentation.

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
    image-rs decoder creation and the routing to the modules
    metadata-only probing
    image decoding

formats/
    containers image-rs cannot express, one module each
    and the format corrections for the ones it can
    plus the probes that answer before a decoder is built

pixel.rs
    image-rs -> VapourSynth mapping
    interleaved -> planar conversion

color.rs
    ICC detection and the frame properties
    the matrix/range tags of the yuv and rgb families
    the h.273 code points the container states, mapped onto those properties

error.rs
    plugin errors
```

---

# Initial Implementation Scope

The first version should remain deliberately small.

Implement:

1. `files:data[]`.
2. PNG/JPEG/TIFF/etc. through `image-rs`, and WebP's pixels through libwebp.
3. JPEG XL through the `jxl` crate.
4. GRAY8/GRAY16.
5. RGB8/RGB16/RGB32F.
6. Lazy per-frame decoding.
7. Constant-format clips.
8. Optional variable format/resolution via `mismatch=True`.
9. Basic color metadata.
10. The `ImgSeq*` frame properties and optional `ICCProfile` (see the section
    above).
11. VapourSynth-managed frame-level concurrency.
12. Keep the `image-rs` Rayon feature enabled initially.
13. `ReadAlpha` for a separate alpha clip.

Defer:

```text
GPU/Vulkan output
manual SIMD
custom Rayon inside get_frame()
filesystem globbing inside the native plugin
format-based clip grouping
special 10/12-bit preservation
```

The list has moved three times since it was written:

- **`native YUV preservation` is done**, for the one format that stores yuv and
  can be handed out that way: a lossy webp without alpha is a `YUV420P8` clip.
  Decoding it to rgb first cost a conversion nothing asked for and half the
  lookahead budget — the sandbox webp set reads in 3.39 s instead of 5.66 s at
  the default `prefetch` and 11.94 s instead of 19.97 s at `prefetch=0`, which
  [03](improvements/03-webp-yuv-output.md) measured. A colour heif, heic or avif
  page followed in [12](improvements/12-heif-avif-yuv-output.md), where the
  container's own planes and its own matrix decide the format.
- **`special 10/12-bit preservation` is done**: [10](improvements/10-nominal-bit-depth.md)
  reads the depth the container states and hands the file out as the format that
  names it, right aligned in that format's word, so a ten bit avif page is a
  `RGB30` frame instead of a `RGB48` one whose samples stop 0.4% short.
  [12](improvements/12-heif-avif-yuv-output.md) had taken the half of it a yuv
  page needs, because those planes already carry their depth in the format. What
  is left of the depth question is a decoder that does not fill the range of a
  sixteen bit file, and a fixture for the depths nobody has a file at.

The rest is policy and stays where it is: gpu output (waiting on
`vapoursynth4-rs`), SIMD, custom Rayon, and the two python-side helpers. ICC
conversion remains the responsibility of a downstream color-management
filter; plan [13](improvements/13-icc-color-management.md) covers exporting the
embedded profile bytes, which is now implemented.
`docs/improvements/README.md`'s `not planned` section has the reason for each,
and the plans this doc still asks for are 06 and 07 in that same folder.

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
   decoder::probe / decoder::decode
         │
         ├── image-rs           PNG, JPEG, TIFF, GIF, BMP, EXR, PNM, QOI,
         │   │                  TGA, ICO, HDR, and a monochrome avif, whose
         │   │                  samples it decodes as rgb whatever the file holds
         │   └── libheif-rs     colour heif/heic, which it decodes as its own
         │                      yuv planes rather than through the image hook
         │
         └── src/formats/       the files those paths describe wrongly
             ├── heif.rs        monochrome heif/heic and the heif handle's own
             │                  colour, bit depth and alpha
             ├── avif.rs        every colour avif, through `dav1d`, and the avif
             │                  probe that answers before any decode
             ├── jxl.rs         every jpeg xl, which `image` has no format for
             ├── png.rs         the `cICP` chunk of every png, which `image`
             │                  has no accessor for
             └── webp.rs        every webp's pixels, planar yuv for a lossy one
         │
         ▼
   normalized image
   RGB / Gray / YUV
       │
       ▼
interleaved or planar -> planar
       │
       ▼
VapourSynth VideoFrame  <- built by the worker that decoded, not by the request
       │
       ├── pixel data
       ├── color properties
       └── the ImgSeq* keys
```

The central design principle is:

> Let VapourSynth schedule frames, let the registered decoders read individual
> images, keep each frame completely independent, and only overlap decoding when
> a client walks the clip sequentially.

This gives the plugin simple random access, good multicore scaling, minimal shared state, and a mostly safe Rust implementation.
