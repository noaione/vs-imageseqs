# wpd as the WebP decoder

Status: **implemented** — wpd decodes an ordinary still and libwebp keeps the
animated container's rectangle, canvas and blend. Investigated on 2026-10-04 and
implemented on 2026-10-06 against wpd revision
`45d5955260f6c44a1b6bea6954a4a34e818f3f8a`, which is what `Cargo.toml` pins;
the research below was measured on `9748cedeedbea4bb195cab18273a33b04531c455`.
This extends [34](34-input-routing-and-planar-decode.md).

## what landed

`src/formats/webp.rs` answers an ordinary still with a `decoder::RowStream`
that creates a wpd decoder inside its own `fill` and writes the borrowed rows
of the picture it decoded straight into the frames the call allocated. Nothing
about the container is read twice: `decode` reads the file once, asks
`animation::webp::first_picture` whether it holds a timeline, and hands the same
bytes to the stream. The decoder is created inside that call rather than stored
because wpd's decoder is not `Send`, which is the same restriction the research
recorded; a still is one picture, so nothing is lost by it.

A file whose `ImageInfo.transform` is not the identity is the one exception. It
is decoded into buffers and handed to the frame writer, because a transform is
`src/pixel.rs`'s walk and a row stream writes row for row. Its pixels are what
the orientation section of `tests/readalpha.vpy` checks, including the two yuv
orientation fixtures whose width and height swap.

The first-party probe, the content route, the animation source, the
one-presentation `first_picture` guard, the ICC policy and the property
handling are all unchanged. `docs/BENCH.md` carries the integrated figures; the
short version is 1.22x to 1.31x on the 35 page webp set at the three prefetch
depths, 1.53x on the serial per-frame stage split (196.84 → 128.46 ms a frame),
and byte identical pixels on 76 of 76 webp parity lines and all 941 validator
checks.

### what is left over

- **wpd for the animation rectangle.** The optional experiment at the end of
  this page was not implemented: `animation/webp.rs` still composes with
  libwebp's own rectangle decoder and its integer blend. Nothing in the still
  work blocks it, and the raw-input adaptation it needs was already proven (27
  of 27 extracted rectangles matched libwebp). It is the one piece of the plan
  left, and it is deliberately not done, so libwebp stays linked either way.
- **native ARGB output for lossless files.** Recorded below as an option and
  not measured, because the user accepted the memory increase before this was
  implemented; the packed RGB/RGBA path is what shipped.
- **`Options.frame_size_limit` is set to the pixel count the container
  states.** That is a bound against a header asking for an allocation before the
  size check runs, not a policy; a file that legitimately decoded to a
  different size than its container declared would now be refused twice over.
- **assembly tooling.** An x86 or x86-64 build needs `nasm` on `PATH`, and the
  archiver `nasm-rs` calls is MSVC's `lib.exe` on Windows. `rust-tests.yml` and
  `tools/setup-linux-build.sh` install it, and the Windows wheel job already
  did; an ARM build assembles with the C compiler and needs neither.

Only this report and its index entry are intended tracked changes. Experiments
and cloned upstream sources are under ignored `target/`.

## decision

The selected approach is **wpd as the default still WebP pixel decoder, with
libwebp retained for animation**. This is feasible with the current source
architecture. The two libraries already link and run together in the standalone
research executable. Keeping libwebp here does not restore an image-rs dependency.

Use wpd's Rust API for ordinary still containers, keep opaque lossy stills as
YUV420P8, and keep alpha and lossless stills as planar RGB with separate alpha
where requested. Keep our first-party probe, orientation and ICC handling.
For animated containers, keep the existing timeline, canvas and libwebp RGBA
subframe decode, including the one-presentation container case. Start wpd with
one internal decoder thread per prefetch worker.

The user accepts the measured RGB/RGBA memory increase as a library tradeoff.
Reducing it to libwebp's footprint is **not an adoption requirement**. Continue
reporting memory and respecting existing prefetch semantics; correctness,
platform builds and integrated still speed are the remaining acceptance checks.
wpd's `Send` restriction and compositor differences do not block this split,
because wpd is created and dropped inside each still decode worker.

The previous wpd-per-subframe proposal remains an optional future replacement
for the animation codec. It is not required for this implementation scope, and
there is no requirement to remove libwebp later.

This recommendation concerns the measured upstream revision, not a promise
that a future release or every image will be faster.

## versions and evidence

- Baseline repository HEAD: `0fc0e192572d401055e36e1c1f50d9cf4ae3788b`, with
  existing user edits in the working tree. The local release DLL was preserved
  independently of subsequent working-tree changes. SHA256:
  `E1D3A17EDECE7DE75D1E61888BC21B94243C660D6E36099ADC14080545DC8ADE`.
- [wpd](https://github.com/halidecx/wpd/tree/9748cedeedbea4bb195cab18273a33b04531c455):
  revision `9748cedeedbea4bb195cab18273a33b04531c455`, Cargo version 0.2.0,
  Rust minimum 1.82, BSD-2-Clause. No upstream source modifications were needed.
- [Upstream fixtures](https://github.com/halidecx/wpd-test-data/tree/f8c31341db3ab4400f048a96e7b3736fed303b34):
  revision `f8c31341db3ab4400f048a96e7b3736fed303b34`, 48 WebP files.
- Native comparison: local libwebp 1.6.0. Windows x86-64, Intel i5-11400H
  (6 cores, 12 logical processors), release builds with wpd's assembly enabled.
  The scratch harness uses the plugin's release shape:
  optimization 3, thin LTO, one codegen unit, abort on panic.

The repository was being edited concurrently. The baseline DLL, upstream
revision and scratch executable are separate artifacts; performance results
below do not depend on later edits to TIFF, PNM or EXR.

## what currently costs work

`src/formats/webp.rs` reads the complete compressed file, validates dimensions
with `WebPGetInfo`, then calls libwebp. Opaque lossy images use
`WebPDecodeYUVInto` into three owned planes, which are copied into VapourSynth.
Other images use packed RGB/RGBA and then the planar writer. At the initial
benchmark snapshot, still metadata also passed through the `image-webp` adapter
in `src/still.rs`. A follow-up inspection confirms that the reworked tree now
routes WebP probing to `formats::webp::image_info`, including dimensions, alpha,
EXIF orientation and ICC bytes. Cargo no longer enables the image-rs WebP
decoder. A hidden-name animation's first picture also uses the first-party timeline
path. Keep these improvements: wpd can replace the pixel engine without taking
over metadata or reintroducing image-rs. This new tree needs a fresh performance
baseline before implementing a replacement; the table below remains historical.

`src/animation/webp.rs` already owns timeline discovery and composition. A
presentation request reads the complete file again, extracts a frame payload,
constructs a standalone WebP where necessary, calls libwebp for RGBA, draws
the rectangle and clones the canvas. These repeated compressed reads and
wrappers can be removed independently of the decoder choice. Fixing them is
part of a fair complete implementation, rather than attributing every resulting
gain to wpd. The current extraction helper returns a copied chunk sequence for
VP8/VP8L and adds RIFF/VP8X only for a frame beginning with ALPH. wpd's accepted
raw input differs; the follow-up experiment below identifies the needed slice.

## fit of the Rust API

The [Rust API](https://github.com/halidecx/wpd/blob/9748cedeedbea4bb195cab18273a33b04531c455/src/api.rs)
can be used as an ordinary Cargo library. Meson and the C wrapper are not
needed for this plugin.

| requirement | upstream capability | integration choice |
| --- | --- | --- |
| Opaque lossy still | `Format::Yuv420p`, borrowed plane rows | Copy each active row directly into the VS Y/U/V planes. |
| RGB or alpha still | RGB, RGBA, BGRA, native ARGB and other packed outputs | Scatter rows into planar R/G/B and, for `ReadAlpha`, gray alpha. Do not use premultiplied output. |
| Animated WebP | wpd can decode raw subframes or a complete animation | Deferred for wpd. Keep our parser, `SegmentTable`, canvas and existing libwebp rectangle decoder. Ignore loop count as before. |
| Orientation / ICC | EXIF and ICCP payload access | Parse orientation with the existing `src/exif.rs` helper; keep pixel rotation and property handling in imageseqs. |
| Compressed ownership | Borrowed, copied, transferred and incremental inputs | Choose ownership explicitly rather than hiding another complete input copy. |

`Picture` borrows decoder-owned storage. Consume its rows into a VS frame or
the existing immutable presentation cache before calling the decoder again.
Validate plane count and dimensions before indexing: public row access asserts
on invalid indices, which would abort this plugin's release process.

Subsampled plane dimensions use ceiling division for odd sizes. That does not
remove VapourSynth's own restrictions on a subsampled frame's dimensions.
YUVA support also does not justify changing alpha WebP's existing RGB output.
Keep source labels, 8-bit depth, color properties, opaque-alpha fallback and
orientation behavior consistent between probe and decode.

## selected split and its routing boundary

| input | selected pixel backend | output behavior |
| --- | --- | --- |
| Ordinary opaque lossy WebP | wpd | Existing YUV420P8 and its color properties. |
| Ordinary lossless WebP | wpd | Existing planar RGB8, with gray alpha when present and requested. |
| Ordinary lossy WebP with ALPH | wpd | Existing planar RGB8 and separate requested alpha. |
| Multi-presentation animation container | libwebp through our animation source | Existing composed canvases, sampled timeline, alpha, orientation and metadata. |
| Animation container holding one presentation | libwebp through `first_picture` | One output frame, using the existing animation canvas behavior. |

This is a container-based selection, not a retry chain or extension-based
choice. A still decode error from wpd should remain a path-qualified error;
the initial design does not silently retry every still with libwebp.

The existing call boundaries make the split concrete:

- `decoder::probe_segment` sends a WebP with several presentations to
  `animation::webp::segment_info` and its existing `Source`.
- `Source::presentation` calls `formats::webp::decode_rgba` for each extracted
  rectangle. Keep this helper's libwebp implementation for animation.
- `formats::webp::decode` already checks `animation::webp::first_picture`
  before its ordinary still pixel path. Preserve that guard, then use wpd for
  the ordinary YUV/RGB/RGBA still branches.
- `formats::webp::image_info` remains the shared first-party probe. It does not
  need to be replaced by wpd's complete-file scanner.

**Do not select the backend from `num_frames == 1`.** Our
`Animation::is_animated` means more than one presentation, so `segment_info`
declines a one-presentation animated container. `first_picture` still recognizes
that container and renders it through the existing source. Sending it to wpd's
full compositor would reintroduce the pixel differences recorded below.
Use the validated animation container state for backend selection, including
any required first-picture path; malformed animation must report an error.

Content routing already uses `identify::route`, so a renamed animation needs
the same libwebp owner. The saved decoder plan proposed in 34 can record the
selection and avoid repeating it; a temporary filename or output frame count
is not its authority. A mixed file list may contain both backends; it follows
the existing `mismatch` rules when still YUV and animated RGB formats differ.

Follow-up research checks for this split:

- The existing research executable statically links libwebp and includes wpd
  as a Rust dependency. Both codecs are called in the same process. Rechecked
  one large YUV still and five RGB/RGBA/odd-size cases: all active plane bytes
  and hashes matched. This verifies local coexistence, not a hybrid VS plugin.
- An isolated RIFF classification experiment passed six cases: ordinary alpha
  still, ICC lossless still, renamed still, ordinary animation, renamed
  animation, and a generated one-presentation animation container. Both renamed
  animation and one-presentation animation selected libwebp.
- Source inspection confirms the one-presentation guard and animation rectangle
  helper already exist. No threading redesign is required to retain them.

The classification experiment uses real fixture bytes but is a research model,
not a test of new production routing. A plugin implementation must still test
the saved backend decision, actual frames and metadata through VapourSynth.

### one input and inexpensive probing

`open(&[u8])` copies compressed input. `open_borrowed(&[u8])` avoids that copy,
but requires its owner to outlive the decoder. A still worker can read once,
borrow that local allocation, decode and drop both in order. A persistent
animation source cannot casually store a buffer and a decoder borrowing it in
the same ordinary Rust struct. Prefer ownership transfer using `open_stream`,
`update(Vec<u8>)` and `end_of_stream`, or a deliberately designed source owner;
do not use forged `'static` borrows. Updated input retains its full prefix and
can rewind; appended input cannot. Confirm this distinction in the adapter.
Measure padding/capacity growth as well as logical input size.

Complete-file `open` scans the container and builds a frame index without
rendering pixels, but still needs the complete bytes. `api::info` accepts a
partial header and is useful for early dimensions; it is not proof that the
file is complete or that the animation's final frame count has been discovered.
The public container scanner consumes byte slices, not a general `Read + Seek`
reader. Incremental `append` is available, but copies bytes and is not a
seekable header-only adapter.

Follow plan 34: identify content once, record the selected backend, and use
one seekable input for each probe/decode operation. Retain a small RIFF walker
for header, metadata and timeline probing, seeking past pixel payloads. Do not
read every image whole at clip creation merely to call wpd's complete-file API.
At demand time, read compressed data once into the decode operation. A source
can keep a reader opened on demand or retain compressed input within an explicit
budget, avoiding repeated opens and standalone wrappers. Bound the number of
retained handles or inputs across files rather than retaining them for every
animation in a long clip indefinitely.

Keeping every file open or every compressed image resident for the clip's
lifetime is not necessary to avoid repeated opens *within* an operation.
For large still lists, lazy decode still opens the requested file later.

Metadata flags in VP8X are not equivalent to a successfully read ICCP chunk.
wpd's high-level metadata getter also collapses retrieval errors into `None`.
Derive `ImgSeqHasICC` from the actual profile, keep bytes only for
`icc_profile=True`, and preserve path-qualified malformed-container errors.
wpd does not apply EXIF orientation or an ICC color transform for us.

### threading and the concrete Send blocker

The plugin's `AnimationDecoder` requires `Send`; `RowStream` requires
`Send + Sync`. At this revision, `wpd::api::Decoder<'static>` fails a compile
check for `Send`: its driver contains an optional
`Box<dyn handout::RowSink>` without a `Send` bound. The failure occurs even
when no sink is installed. A worker-local still decode can use it, but the
existing shared animation source cannot store it directly.

### optional future animation replacement: retain our state, decode locally

The previous conclusion was too broad if read as blocking animation altogether.
`Send` applies to the state transferred between threads, not every temporary
created inside a synchronous method. Our current WebP `Source` already stores
the path, parsed frame entries, transform, output format, canvas and next index;
it does not store a libwebp decoder. The same arrangement works with wpd.
See Rust's [Send contract](https://doc.rust-lang.org/std/marker/trait.Send.html).

For every independently coded ANMF rectangle needed by `presentation(index)`:

1. Obtain its compressed bytes using the existing checked frame ranges.
2. Construct a local `wpd::api::Decoder`, set one thread and straight RGBA,
   and borrow just this rectangle's input.
3. Decode, check the actual rectangle size, and consume rows into an owned
   rectangle buffer or the existing canvas before advancing or dropping wpd.
4. Drop the local decoder within the same call. Keep only ordinary owned input,
   pixels, canvas and replay state in `Source`.

There is no need for `Animation::Subframe` on an isolated raw frame: it is a
still from wpd's perspective. That option belongs to the alternative where
wpd opens the complete animated container. Preserve the current blend decision,
clipping, disposal policy, timeline and presentation cache in either design.

The compressed input conversion is important:

| ANMF payload | input borrowed by the temporary wpd decoder |
| --- | --- |
| `VP8 ` chunk | Codec bytes after its eight-byte chunk header, limited to its declared length and excluding padding. |
| `VP8L` chunk | Same bounded slicing rule. |
| `ALPH` followed by `VP8 ` | Both chunk headers and payloads, with their padding, as the original checked chunk sequence. wpd explicitly accepts this raw form. |

Passing the current helper's VP8/VP8L chunk sequence unchanged was reproduced
as `not a WebP file`; libwebp accepts that shape. Applying the slicing rules
above fixed this. No RIFF allocation is needed for these tested raw forms.
The relevant parser is [raw_headers](https://github.com/halidecx/wpd/blob/9748cedeedbea4bb195cab18273a33b04531c455/src/container.rs).

Two new scratch experiments validate the alternative without modifying the plugin:

- A `Send` source containing `Arc<[u8]>`, a canvas and an index was moved through
  two spawned workers. Each constructed and dropped its own wpd decoder and
  returned identical owned RGBA. The release example compiled and ran without
  unsafe code or an upstream patch.
- All 27 extracted rectangles from the project animation, both upstream blend
  examples and upstream `anim_yuva.webp` / `anim_yuv.webp` matched reference
  libwebp RGBA exactly. This includes VP8, VP8L and ALPH/VP8 inputs. It validates raw input adaptation,
  not a complete plugin integration or its performance.

This was the previous first implementation proposal; animation is now explicitly
kept on libwebp. If wpd animation is pursued later, this option retains the
existing threading architecture and avoids replaying the animated container inside wpd.
Creating a decoder for the **whole animation** on each request would decode
earlier frames again; isolating the requested rectangles avoids that extra replay.
Backward reads still replay according to our existing canvas/cache policy.

The tradeoff is decoder setup and allocation per rectangle. The historical
still benchmark already includes constructing a decoder per image, but it
does not measure small-frame animation setup or workspace reuse. Benchmark
short/tiny subframes as well as large rectangles before claiming this path is
faster. A persistent full-file decoder remains an optional later optimization.

| alternative | fit for this plugin |
| --- | --- |
| Keep libwebp for animation, wpd for stills | Selected initial scope; no retained wpd animation decoder or compositor changes. |
| Local wpd decoder per independent rectangle | Optional later experiment; no `Send` patch, actor thread or unsafe wrapper needed. |
| Audited upstream change or pinned fork adding appropriate sink bounds | Enables retaining a decoder; pursue if measured setup/reuse gains justify it. Compile and test the patched type rather than assuming the sink is the only obstacle. |
| Decoder kept on a dedicated thread with bounded requests | Possible, but adds scheduling, shutdown and memory accounting; reserve for a measured persistent-context benefit. |
| Thread-local decoder cache on arbitrary prefetch workers | No guaranteed worker affinity for a source; risks duplicate decoder state and replay. Not the first choice. |

### optional persistent decoder and thread budget

For the persistent design, an upstream API change should make the decoder
transferable between
threads, separating a thread-affine sink if necessary, or requiring appropriate
sink bounds after auditing the implementation. Do not paper over this with an
unconditional unsafe `Send` implementation or a raw C pointer wrapper.
A permanently thread-owned decoder with message passing is possible, but adds
queues, scheduling and memory accounting that this plugin already has.

Set `Options.n_threads=1` explicitly at first. Applying the default options
with `n_threads=0` requests automatic internal threading. Combining that with
multiple prefetch workers can multiply the runnable threads. Animated ahead
decoding also has its own cache, capped at 16 slots and 96 MiB at this revision;
that cap excludes other decoder buffers and is not the plugin's global budget.
Benchmark internal 2/4-thread decoding separately, especially with `prefetch=0`.
The one exploratory four-thread YUV run did not establish a gain.
See [task scheduling](https://github.com/halidecx/wpd/blob/9748cedeedbea4bb195cab18273a33b04531c455/src/task.rs)
and [animation implementation](https://github.com/halidecx/wpd/blob/9748cedeedbea4bb195cab18273a33b04531c455/src/driver/anim.rs).

### rows and memory

For stills, the first adapter should create the decoder *inside* a row-stream
write operation and copy borrowed decoded rows directly to all frames allocated
for that call. This removes our intermediate `Pixels` buffers while retaining
the decoder's own picture. Rotation can still require a buffered path.
wpd has no planar RGB output, so packed RGB needs one deinterleave pass.

The lower-level driver exposes a boxed external `RowSink`, but the high-level
API does not expose its setter. That sink has a `'static` ownership shape and
the threading limitation above; it is not immediately interchangeable with
the plugin's borrowed frame writer. It exports decoded rows from internal
storage, rather than eliminating codec workspace. Start with the borrowed-row
API before depending on this lower-level facility.

Native ARGB is worth a separate experiment for lossless files: upstream can
hand out its existing `u32` image without converting it into another packed
format. On little-endian systems those words expose BGRA bytes, so the writer
must map channels explicitly and handle other byte orders. This may remove a
full packed conversion buffer; that saving was **not measured** in this study.
The [export code](https://github.com/halidecx/wpd/blob/9748cedeedbea4bb195cab18273a33b04531c455/src/driver/export.rs)
is the relevant implementation, not an assumption that all RGB output is free.

## animation parity is an explicit choice

The current WebP canvas deliberately ignores disposal and uses a particular
integer blend, including a 24-bit reciprocal. It also treats VP8L payloads as
alpha-capable for deciding whether to blend. wpd's full compositor honors
disposal and can take different opaque/keyframe paths.

Measured against the preserved plugin through `ReadAlpha` at 1000 fps:

- `tests/fixtures/animation.webp`: all four displayed canvases match.
- Upstream `dispose_bg_blend.webp`: all three full-compositor canvases differ.
- Upstream `dispose_none_blend.webp`: all three differ even without disposal.
  On the first frame, a channel is 255 in wpd and 254 in the current plugin;
  alpha matches. This is a composition difference, not a still decode failure.
- Decoding **subframes** with wpd and applying the current plugin's canvas
  arithmetic and inert-disposal policy reproduces all ten presentations across
  these three files exactly, including both alpha and color.

Therefore retain existing frame payload classification, clipping, composition,
zero-delay handling and CFR mapping. The selected split keeps libwebp's current
rectangle decoder. If wpd animation is pursued later, use independently decoded
raw rectangles or `Animation::Subframe` for a complete animated input.
Do not silently adopt upstream's full compositor. Changing these existing
pixels or disposal semantics needs its own user-facing decision and fixtures.

`rewind()` supports replay for complete input, not appended streams. There is
no high-level arbitrary-frame seek operation. Keep the existing bounded
presentation cache and replay strategy; do not assume an index alone provides
cheap reverse access. Random/seeking behavior needs its own measurements.

## local results

The scratch executable loads compressed inputs before timing, constructs a
fresh decoder per image, decodes, allocates output and transfers rows into
simulated VS planar buffers. Both sides produce the same requested format;
wpd uses borrowed compressed input and one internal thread. Three pairs
alternate libwebp/wpd order. Numbers are median elapsed seconds and median
**paired** speedup. They exclude disk I/O, plugin scheduling, properties and
actual VS allocation, so they are not plugin throughput results.
This was a warm local experiment without controlled background machine load.

| case | work per pass | libwebp seconds | wpd seconds | paired speedup | peak process MiB, libwebp → wpd |
| --- | --- | ---: | ---: | ---: | ---: |
| Sandbox opaque lossy, YUV420 | 35 pages | 12.674 | 9.002 | 1.408× | about 206 → 207 |
| Generated lossless RGB | 12 decodes, 3312×4717 | 3.803 | 2.311 | 1.646× | 118 → 163 |
| Generated lossless RGBA | 12 decodes, 3312×4717 | 4.316 | 2.909 | 1.484× | 134 → 193 |
| Generated lossy RGBA | 12 decodes, 3312×4717 | 3.911 | 2.488 | 1.572× | 139 → 163 |

Peak working set is the OS process peak, including preloaded compressed input
and allocator behavior. It is not a portable per-decoder allocation bound.
An extra 24–60 MiB on one large alpha image can multiply across prefetch workers,
but the user accepts this measured increase. Report the integrated footprint;
do not require equal or lower memory to select wpd. Existing `prefetch_memory`
accounts for lookahead payloads rather than all codec workspace, and must not
be presented as a hard bound on process memory. The current harness includes our intermediate
buffers on the libwebp side and upstream conversion buffers on the wpd side;
direct VS transfer and native ARGB still require an integrated comparison.

Exploratory, unpaired runs: copying compressed input with `open` took 9.998 s
on the YUV corpus; borrowed input with four threads took 8.932 s. Neither is
enough evidence to choose a new thread policy or quantify a copy penalty.

Correctness checks performed:

- All 35 sandbox files: libwebp 1.6.0 and wpd Y/U/V active bytes and SHA256
  hashes match exactly.
- Five generated cases: large lossless RGB/RGBA, large lossy RGBA and two odd
  17×13 alpha stills match packed RGBA exactly.
- Independent Python comparison: 21 upstream stills plus those five generated
  files match Pillow/libwebp's packed RGBA bytes exactly. This separately
  verifies the Rust harness's still-parity conclusion.
- Upstream release tests: 215 library tests and 9 integration tests pass
  after fetching the separately maintained fixture repository.
- `Decoder<'static>: Send` check fails as described above.
- Initial and release plugin validator: 609 checks pass with no captured
  warning/critical/fatal messages. This checks the **baseline**, not a wpd plugin.

The baseline plugin benchmark used the preserved DLL, all 35 sandbox WebPs,
three alternating passes, and default / 0 / 16 prefetch. Best frame-read times
were 3.591 / 12.340 / 2.291 s, with clip creation 0.083 / 0.135 / 0.100 s.
Timing spread is visible in the logs; no candidate plugin exists to compare.

After writing the report, `cargo build --release --locked` succeeded and the
current validator again passed all 609 checks with no captured warnings.
Repeating the plugin benchmark with the **same preserved baseline DLL** gave
best frame-read times of 3.896 / 12.944 / 2.198 s and clip creation of
0.094 / 0.092 / 0.116 s. This records local timing variation; it does not
measure an implementation regression or establish candidate plugin speed.

## build and packaging for the selected split

wpd's default features include assembly and threads. Assembly provides runtime
dispatch through x86 AVX2 and ARM implementations; there is no AVX512 decoder
dispatch at this revision. Keep the plugin's CPU variants and host-safe Cargo
target configuration. A no-assembly build exists, but its performance must be
measured independently. See [Cargo configuration](https://github.com/halidecx/wpd/blob/9748cedeedbea4bb195cab18273a33b04531c455/Cargo.toml)
and [build script](https://github.com/halidecx/wpd/blob/9748cedeedbea4bb195cab18273a33b04531c455/build.rs).

The first Windows build reported that NASM could not spawn a program. NASM
was installed; the missing executable was MSVC's `lib.exe`, used by
`nasm-rs` to archive objects. Adding the existing MSVC tools directory to PATH
and using an owned writable TEMP directory fixed the build without source
patches. CI must provide the assembler and archiver, or upstream must improve
tool discovery. ARM, Linux glibc/musl, macOS and all wheel variants were not
built here. Upstream tests passing on Windows does not certify those targets.

Pin a reproducible release or exact revision and lockfile; ensure source
distribution/offline builds include or can obtain the required Rust and
assembly sources. Add wpd to the existing plugin build rather than building
its C ABI as a second dynamic library. Keep the plugin identity and wheel layout.

**Retain libwebp's native dependency, build scripts and license notices.**
`build.rs`, `vcpkg.json`, the Linux native builds and macOS packaging/link checks
still serve a production animation decoder. The split will ship both libraries;
it does not provide a dependency-count or guaranteed binary-size reduction.
wpd's Cargo/assembly build must be validated for each wheel platform and CPU
variant. Add its applicable license text and notices when implementation lands.
Keep libheif's dependency policy unchanged.

The libwebp FFI is localized. Keep `WebPGetInfo` and `WebPDecodeRGBAInto` for
the animation rectangle helper. Ordinary still pixel calls to
`WebPDecodeRGBInto` / `WebPDecodeYUVInto` can leave production dispatch, while
remaining as test-only references if useful. Existing libwebp fixture encoders
and comparison tests can stay because the native library remains linked.

The follow-up tree has removed image-rs from Cargo and deleted `src/still.rs`.
Both selected WebP paths are independent of it. If all-WebP wpd adoption is
pursued later, native dependency removal becomes a separate scope requiring
another packaging and legal audit; it is not part of this selected split.

## implementation order and acceptance

1. Capture the baseline of the reworked image-rs-free tree. Pin wpd, use its
   Rust API and make internal thread use explicit. Keep native libwebp linked.
2. Keep the new first-party WebP probe and the content-selected plan from 34
   for metadata, dimensions, orientation and timeline. Preserve header-only
   clip creation and ICC policy. Select wpd only for ordinary still containers;
   keep the animation source and the one-presentation first-picture guard.
3. Add a worker-local still decoder and direct row transfer. Measure native
   ARGB versus packed RGB/RGBA if useful. The user accepts the measured memory
   increase, so that optimization is optional rather than a prerequisite.
4. Keep animation rectangle decoding on libwebp. Verify multi-presentation,
   one-presentation and renamed animated inputs, as well as mixed backend file
   lists. No new persistent wpd animation context or thread pool is required.
5. Compare complete candidate and baseline plugin runs.
   Require exact active-plane bytes, format, alpha, dimensions, orientation,
   ICC and color-property parity for stills and the sampled animation timeline.
6. Measure default, 0 and 16 prefetch with repeated alternating passes: clip
   creation, first frame, whole sequence, CPU time and peak memory. Include
   lossless, alpha, animation, random/backward reads and unrelated-format controls.
   Require measured integrated still improvement and investigate significant
   speed regressions; retain memory measurements without imposing parity with
   libwebp's footprint. Confirm normal bounded prefetch/cache behavior.
7. Reproduce builds for shipping platforms, update notices to include wpd while
   keeping libwebp, and validate wheel/native linkage and legal contents. Run
   the validator and benchmark protocol from `AGENTS.md` again.

Additional qualification remains: malformed/truncated/oversized files,
compressed ALPH variants, mixed-codec animation, all disposal/blend combinations,
oversized rectangles, zero delays, reverse reads, metadata errors and concurrent
workers. The upstream fixture set is useful but is not a complete security or
platform qualification. Full checkasm, fuzzing and Miri were not run.

Local artifacts: `target/research-wpd-{release,release-final,validator-initial,
validator-release,plugin-baseline,microbench,parity-sandbox,parity-generated,
independent-parity,sendcheck,upstream-tests-with-data}.log`. Scratch location
files are `target/research-wpd-location.txt` and
`target/research-wpd-upstream-location.txt`. These are local evidence, not
tracked fixtures or implementation files.
The final validator and repeated plugin benchmark are in
`target/research-wpd-validator-final.log` and `target/research-wpd-plugin-final.log`.
Follow-up evidence is `target/research-wpd-local-decoder.log`,
`target/research-wpd-subframe-inputs.log` (the input-shape rejection) and
`target/research-wpd-raw-subframe-parity.log` (27 adapted inputs passing).
The selected-split follow-up is recorded in
`target/research-wpd-coexistence-yuv.log`,
`target/research-wpd-coexistence-rgba.log` and
`target/research-wpd-hybrid-routing.log`. The latter is an isolated classification
model, not an implemented hybrid plugin.
