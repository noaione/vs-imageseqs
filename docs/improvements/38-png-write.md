# PNGWrite: request-driven PNG export

Status: **research complete, proposed, not implemented**. This records the
requested `PNGWrite` function, the encoder capabilities checked during research,
and the recommended first scope. Defaults, repeat-save behavior and conversion
policy below are recommendations, not an approved or existing public API.
No plugin edits, builds, validator runs or benchmarks were performed for this
research. Adding this note does not change the plugin or wheel.

## objective and scope

Add `core.imgseqs.PNGWrite(clip, output_path, ...)`, returning a video node that
writes a PNG when its output frame is evaluated. Creating the node does not write
images. Python can trigger a write with `writer.get_frame(n)` or request all
frames to export the sequence. An unrequested frame is never written.

Keep the plugin identifier `xyz.n4o.imgseqs`, namespace `imgseqs`, existing
`Read` and `ReadAlpha` functions, and plugin-only wheel unchanged. No Python
helper package is needed. The function writes separate static PNGs, not APNG.

**Recommended first scope:** integer Gray and RGB at 8–16 bits, optionally with a
separate matching Gray alpha clip. Write 8/16-bit input directly and widen 9–15
bits to 16 without discarding precision. Require explicit upstream conversion
for YUV and float. Low-bit grayscale and indexed palette output are legal PNG
features but are not part of this first scope.

## evidence and difficulty

The project already depends on [`png 0.18.1`](../../Cargo.toml#L46), with
`default-features=false`. Encoding, streaming and numeric DEFLATE compression
are available in that configuration. No new native encoder dependency is needed.
The existing Rust VapourSynth bindings expose upstream frame requests, planar
pixel access, copy-on-write frame copies and node cache control.

Registration belongs beside the existing functions in
[`src/lib.rs`](../../src/lib.rs). The current source filter in
[`src/source.rs`](../../src/source.rs) demonstrates creation and error handling,
but a writer needs the normal two-stage upstream request callback rather than a
source's immediate frame return. The reader's decode/lookahead pool should not
become a disk-writing pool.

This is a **moderate feature**, not a codec implementation. The difficult work is
safe output publication, repeat-request semantics and color/conversion policy.
Planning estimates, not measured implementation results:

| scope | estimated effort |
| --- | --- |
| basic Gray/RGB 8/16-bit writer | 1–2 focused days |
| first release with alpha, intermediate-depth widening, metadata, safe publication, repeat handling and tests | 3–5 focused days |
| automatic YUV/float conversion with documented range, color and HDR policy | around 1–2 weeks total, depending on scope |

The existing [IMWRI writer documentation](https://raw.githubusercontent.com/vapoursynth/vs-imwri/master/docs/imwri.rst)
is a precedent for request-driven writing, numbered filenames and a separate
alpha clip. Its API is context, not a requirement to duplicate its behavior or
ImageMagick dependency.

## proposed arguments

The requested arguments are `clip`, `output_path`, `always_save` and
`compression`. Suggested additions are `alpha`, `overwrite` and `start_number`:

```python
# Proposed API; not implemented.
writer = core.imgseqs.PNGWrite(
    clip,
    output_path,
    always_save=False,
    compression=6,
    alpha=None,
    overwrite=False,
    start_number=0,
)
```

| argument | recommended contract |
| --- | --- |
| `clip` | required input video node |
| `output_path` | required UTF-8 filename or frame-number template |
| `always_save=False` | save each frame once successfully per writer instance |
| `compression=6` | integer 0–9, an encoding speed/size tradeoff, never image quality |
| `alpha=None` | optional Gray clip matching color dimensions, frame count, sample type and depth |
| `overwrite=False` | separate permission to replace existing destination files |
| `start_number=0` | nonnegative offset added to the writer node's frame index |

VapourSynth registers booleans as optional integer arguments. Validate values,
checked numbering arithmetic and path syntax before requesting pixels.

Recommended path rules:

- A literal filename is allowed for a single-frame clip. A multi-frame clip
  requires a numbered template, such as `frame%06d.png`.
- Parse a small explicit substitution grammar, including a way to escape a
  literal percent sign. Do not use arbitrary printf formatting.
- Number by the writer node's frame index, not `ImgSeqIndex` inherited from a
  source that may have been trimmed, reversed or spliced.
- Resolve relative paths against the working directory at filter creation,
  before later frame requests can observe a changed process directory.
- Reject empty paths, NULs and templates that let distinct frames target the
  same destination. Require an existing parent directory initially.
- With `overwrite=False`, an existing destination is a path-qualified error,
  not evidence that this writer successfully saved that frame. A future
  skip-existing policy would need a distinct reported result.

`always_save` must not implicitly authorize overwrite. With both options false,
a successfully committed frame is skipped on reevaluation. With
`always_save=True` and `overwrite=False`, a repeated attempt can fail because its
previous output already exists; document this deliberately rather than hiding
replacement permission inside `always_save`.

## frame requests, receipts and repeat handling

In `Initial`, request frame `n` from color and optional alpha dependencies. In
`AllFramesReady`, fetch them, validate actual dimensions and format, write the
image and return a copy-on-write copy of the original color frame with result
properties. The output pixels, format, dimensions, rate and length remain the
input's. Writing must finish successfully before the output request succeeds.
Do not synchronously call upstream `get_frame()` from the filter callback.

Suggested output properties, with names still to be decided:

- `ImgSeqPNGWritePath`: resolved output path.
- `ImgSeqPNGWriteSaved`: 1 after this instance committed the frame successfully.
- `ImgSeqPNGWritePerformed`: 1 when this particular evaluation performed the
  write, 0 when the successful per-instance record caused it to skip.

**FrameProps are receipts, not the authoritative save ledger.** Updating an
output copy does not mutate the upstream frame. Reevaluation may receive an
unmarked input, while a marker copied through other filters may refer to old
pixels or another destination. Do not use inherited save markers to skip work.
Use synchronized per-instance state keyed by frame index and its deterministic
destination, and record success only after publication. Keep the ledger separate
from pixel caches, so frame eviction does not forget successful writes. Bound its
memory, for example with a bitset over a finite clip rather than retained frames.

Recommended `always_save=True` meaning:

> Attempt a write each time the writer's frame callback executes, even if this
> instance successfully saved that frame before.

It cannot promise one write per Python `get_frame()` call. VapourSynth can return
cached output or coalesce simultaneous requests, and downstream caches can bypass
the writer entirely. Disabling the writer's own cache helps but does not defeat
all graph caches. See [cache controls](https://www.vapoursynth.com/doc/functions/video/setvideocache.html)
and the [C API's filter modes and request patterns](https://www.vapoursynth.com/doc/api/vapoursynth4.h.html).
Concurrent evaluations may complete out of order. A serialized completion mode
such as `ParallelRequests` is a simple first implementation; parallel encodes
require bounded workspace and destination coordination. Serialization within one
instance does not protect against another instance or process.

Failed writes raise a VapourSynth error containing the frame number and path.
They must not set success state, so a later evaluation can retry. FrameProps do
not guarantee that an output file still exists after another program deletes it,
and the per-instance ledger is not persistent across script runs.

## supported pixels and widening

The [PNG IHDR rules](https://www.w3.org/TR/png-3/#11IHDR) allow:

| stored PNG type | bits per sample |
| --- | --- |
| grayscale | 1, 2, 4, 8, 16 |
| RGB | 8, 16 |
| grayscale + alpha | 8, 16 |
| RGBA | 8, 16 |
| indexed palette | 1, 2, 4, 8 |

All stored channels have the same depth. PNG has no native YUV or floating-point
sample representation. The encoder needs interleaved Gray[Alpha] or RGB[Alpha]
rows, top to bottom. Honor each VapourSynth plane's own stride and exclude row
padding. Sixteen-bit samples must be serialized big-endian, not copied as native
`u16` bytes.

For integer depths `d` from 9 through 15, emit a 16-bit PNG using reversible bit
replication:

```text
y = (x << (16 - d)) | (x >> (2*d - 16))
original x = y >> (16 - d)
```

This preserves source precision in the high bits and maps maximum input to
65535. Merely writing 10-bit values into a 16-bit PNG would make the picture too
dark. Merely shifting and zero-filling would also leave maximum alpha short of
fully opaque. Emit an `sBIT` chunk describing the original per-channel precision.
See [PNG depth scaling](https://www.w3.org/TR/PNG-Encoders.html#E.Sample-depth-scaling)
and [sBIT](https://www.w3.org/TR/png-3/#11sBIT).

`sBIT` does not change IHDR's stored depth or require a reader to expose nominal
precision. The current project's PNG policy keeps its stored 16-bit word depth,
so exporting a 10-bit frame this way does not promise that `Read` returns
`Gray10` or `RGB30`. Changing reader interpretation is outside this proposal.

PNG alpha is straight/unassociated, linear and full-range. Preserve color samples
under fully transparent pixels. Do not silently premultiply, unpremultiply or
zero transparent RGB. Initially require matching color and alpha depth rather
than adding a separate alpha conversion policy.

Variable-size/format clips need validation on each actual frame. Supporting
variable dimensions is natural for separate files; accepting variable format
should only be claimed after every frame is checked against this same supported
format policy. Reject malformed plane layouts and checked-size overflows before
allocation or encoding.

## conversion policy

The first version should reject YUV and float with an actionable error directing
the caller to [VapourSynth's resize filters](https://www.vapoursynth.com/doc/functions/video/resize.html).
Those already handle chroma resampling, matrix/range conversion, integer depth,
dithering and transfer/primaries conversion without a new native dependency:

```python
# Supply the actual input matrix if frame metadata does not state one.
rgb = core.resize.Bicubic(clip, format=vs.RGB48, matrix_in_s="709")
```

This example is not a BT.709 default. Never infer a missing matrix as BT.709 or
mark unknown RGB as sRGB. Converting YUV to RGB does not itself tone-map HDR or
convert transfer characteristics for ordinary viewers.

Automatic conversion is a possible later extension, but requires decisions on
output depth, dithering, chroma resampling, missing color metadata, limited-range
Gray/RGB, float clipping, NaN/infinity handling and HDR/tone mapping. Float values
outside `[0, 1]` cannot be preserved as ordinary PNG integer samples. Keeping
these choices explicit is preferable to turning `PNGWrite` into a second color
conversion framework.

## compression and streaming

[`DeflateCompression`](https://docs.rs/png/0.18.1/png/enum.DeflateCompression.html)
exposes numeric levels 1–9. Map public `compression=0` to `NoCompression` and
explicitly set `Filter::NoFilter`; `set_deflate_compression` changes the compressor
but does not change the configured row filter. For levels 1–9, recommend adaptive
filtering, with level 6 as the default.

Every setting is lossless. Levels are zlib-like effort settings, not guarantees
of bit-identical zlib output, exact output size or stable implementation details.
The crate's `Fast`/`Fastest` presets use fdeflate rather than mapping directly to
the numeric levels. A separately named fast preset is optional future work, not
an extra numeric level. Cargo feature unification determines the actual flate2
backend; `png`'s disabled default features do not determine that alone.

Use [`Writer::stream_writer`](https://docs.rs/png/0.18.1/png/struct.Writer.html#method.stream_writer)
with a file destination. Pack one row at a time and call `write_all`, not a single
`write` that may stop at a row boundary. Do not supply PNG filter bytes yourself.
Explicitly finish the borrowed stream, then finish its parent writer and flush
the outer file buffer so compression, IEND and I/O errors are reported.

The checked implementation holds three internal row buffers, a default 4 KiB
chunk buffer and compressor state. With a caller-owned row of `R` bytes, extra
workspace is approximately `4R + 4096`, plus backend, metadata and sink overhead.
This is width-bounded, not a guaranteed exact total. `write_image_data` instead
requires the entire raw image and builds a compressed-frame buffer, so it is not
the proposed bounded-memory path. Concurrent encodes multiply workspace; measure
both encoder allocations and upstream frames retained during writing.

## metadata

The source can export raw binary `ICCProfile` bytes through
[`src/color.rs`](../../src/color.rs#L172-L179). A writer can embed a compatible
profile using `Encoder::with_info` and `Info.icc_profile`. That stores metadata,
not a color transform. The encoder compresses the ICC bytes independently of the
selected image compression level.

Only write metadata that describes the actual output pixels:

- Do not invent sRGB. In this encoder, setting sRGB suppresses ICC output.
- Do not copy stale ICC after color conversion. An RGB profile does not become a
  valid Gray profile just because the pixels are grayscale. Alpha is not part of
  the profile's color channels.
- Known compatible `_Primaries`, `_Transfer`, `_Matrix` and `_Range` can describe
  `cICP`; the PNG matrix field must be 0, and its full-range flag must describe
  the actual encoded values. Do not copy a YUV matrix into a PNG.
- Do not copy `ImgSeqOrientation` into an output EXIF orientation: the input
  pixels may already have had that transform applied.

**Encoder caveat verified in `png 0.18.1`:** `Info.sbit` and
`Info.coding_independent_code_points` exist, but `encode_header` does not emit
those chunks. Write `sBIT` and `cICP` explicitly with `Writer::write_chunk` before
IDAT. Both also precede PLTE; writing them after `write_header` is sufficient for
the proposed non-indexed scope, not for a future palette path whose header has
already emitted PLTE. See the
[encoder source](https://docs.rs/crate/png/0.18.1/source/src/encoder.rs).

PNG3 color precedence is `cICP`, then ICC, then sRGB, then gamma/chromaticities.
Define a policy for contradictory frame metadata instead of blindly exporting
all of it. See [PNG3 color spaces](https://www.w3.org/TR/png-3/#4Concepts.ColourSpaces).
Whether metadata export is automatic or controlled by an additional argument
remains an API decision.

## safe file publication

Do not encode directly over the final destination. Use a uniquely named temporary
file in the same directory, finish and flush it, close it as required by the
platform, then publish it with the chosen replacement/no-clobber semantics.
Only then record success and return the marked frame.

A check-then-create sequence is not sufficient for `overwrite=False`: another
writer can create the destination between those operations. Use a race-safe
publication primitive and verify its Windows and Unix behavior. Replacing an
existing file is not uniformly implemented by a naive `std::fs::rename` across
platforms. Choose a suitable abstraction rather than assuming it works.

Clean up only this writer's own temporary file on failure. Preserve an existing
destination if encoding or publication fails. Do not mistake an existing partial
or unrelated file for success. Atomic publication is not a promise of durability
through power loss; file/directory synchronization would be a separate guarantee.

## Python execution examples

```python
# Proposed API; request the writer node, not the original input.
writer = core.imgseqs.PNGWrite(
    rgb,
    output_path=r"exports\frame%06d.png",
    compression=6,
)

with writer.get_frame(42) as frame:
    print(frame.props["ImgSeqPNGWritePath"])

# Sequential requests make error reporting simple.
for n in range(writer.num_frames):
    with writer.get_frame(n):
        pass
```

A literal filename can be used after trimming to one frame, but the writer's
frame index then starts at zero. Requesting frames from the original clip does
not execute this branch. `set_output()` only makes a node available to a consumer;
it does not by itself export the sequence. Iterating `writer.frames()` is another
option, but may render frames concurrently. See the
[Python frame API](https://www.vapoursynth.com/doc/pythonreference.html#VideoNode.get_frame).

## required writer benchmark comparison

The implementation must compare these three routes, not only report plugin
encode timings:

1. `PNGWrite` from this plugin.
2. Python Pillow PNG saving at its default compression, serially on one thread.
3. Python Pillow PNG saving at the same defaults through an nmanga-style bounded
   write pool with **six worker threads**.

The reference inspected is the sibling project's
[`autolevel.py`](../../../nao-manga-rls/nmanga/cli/autolevel.py#L639-L703): the PNG
branches call `image.save(..., format="PNG")` without PNG compression overrides.
There is no function named `save_png` in this inspected file; `write_page` is the
save callback. The
[frame loop](../../../nao-manga-rls/nmanga/cli/autolevel.py#L885-L916)
pulls one frame at a time, converts it to an owned Pillow image, submits the write,
waits on the oldest returned future, and finally drains all pending futures.

[`BoundedWritePool`](../../../nao-manga-rls/nmanga/common.py#L1111-L1153) uses
`ThreadPoolExecutor(max_workers=workers)`. After submission brings its pending
list to `workers`, it pops and returns the oldest future; the caller immediately
waits on that future before producing another page. With six workers this gives
at most six outstanding write tasks, not an unbounded executor queue. Do not
imitate it by submitting the whole sequence before waiting. Its docstring notes
that Pillow releases the GIL during PNG encoding, allowing writes to overlap
while frame acquisition remains ordered on the main thread.

The reference's
[`vs_frame_to_image`](../../../nao-manga-rls/nmanga/vapour.py#L558-L567)
copies the NumPy view before releasing the VapourSynth frame. Preserve that
ownership rule in the benchmark: worker images must not alias a released frame.
That helper extracts one 8-bit plane; RGB/RGBA cohorts need a separately verified
packing path rather than treating plane zero as the whole color image.

The requested six-worker baseline is fixed across machines for comparability.
nmanga's actual
[thread default](../../../nao-manga-rls/nmanga/cli/options.py#L344-L348) is
`max(cpu_count() // 2, 1)`, used by its
[CLI option](../../../nao-manga-rls/nmanga/cli/options.py#L568-L577). Six is its
default on the 12-logical-CPU machine described in the benchmark notes, not a
universal hardcoded default.

The complete timing, fairness and reporting protocol is in
[the writer benchmark section](../BENCH.md#png-writer-comparison-planned). These
sibling links record inspected local reference code, not a new dependency or a
requirement to execute nmanga's full autolevel command. Record the reference
revision when running the comparison. No writer speedup has been measured yet.

## implementation route and acceptance

Before implementing, settle the proposed defaults, path grammar, repeat/overwrite
contract, metadata policy and intermediate-depth support. Then:

1. Add a separate writer module and register `PNGWrite` without changing existing
   identities or source behavior. Declare strict-spatial dependencies on color
   and optional alpha, and keep disk writes out of source prefetch workers.
2. Implement checked frame validation, planar row packing and streamed encoding.
   Keep sample widening independent of PNG chunk serialization so it can be
   tested across all values at each supported intermediate depth.
3. Implement synchronized success bookkeeping, output receipt properties and
   race-safe temporary-file publication. Decide completion concurrency explicitly.
4. Add unit tests, a VapourSynth integration validator and user documentation.
   A landed public function requires a changelog entry; this research note does
   not. Recheck notices only if dependencies or linkage change.

Acceptance must cover:

- Gray, RGB, GrayAlpha and RGBA at 8/16 bits, odd widths and padded strides, with
  exact decoded sample comparisons against an independent decoder.
- Every 9–15-bit widening formula, endpoints and alpha opacity, with exact high-bit
  recovery and raw `sBIT` chunk checks. Do not claim nominal-depth reader parity.
- Preservation of transparent color pixels, unchanged returned color pixels,
  and unchanged input properties after output receipt properties are attached.
- Missing/invalid alpha, frame-count and per-frame dimension mismatch, unsupported
  YUV/float and mixed variable-format frames with actionable errors.
- Compression 0 through 9, invalid levels, file completeness and decoding parity.
- Real `cICP`/`sBIT` chunk presence, compatible ICC byte preservation, truthful
  metadata and conflicts. Setting an encoder field alone is not a test.
- Lazy evaluation, out-of-order and repeated requests, writer cache eviction,
  `always_save` with both overwrite settings, and concurrent requests.
- Unicode/relative paths, changed working directory, numbering overflow, invalid
  templates, existing destinations, two instances racing for one path, and safe
  behavior after open/write/flush/publication failures. Failure injection is
  needed to prove old destinations survive and success is not recorded early.

Before and after implementation, follow the repository's release-build,
validator and benchmark baseline protocol. Run Cargo tests, clippy and fmt, the
existing reader validator and a new writer-specific validator. Measure encode
wall time, CPU, file size and peak memory at levels 0/1/6/9 on the same inputs,
including large RGB16/RGBA16 frames and bounded concurrent requests. Confirm
streaming does not allocate a second whole image and unrelated reader paths do
not regress. Packaging validation is needed if wheel contents, dependencies or
legal files change. No such measurements or validation results are claimed here.
