# 26 - remove image-rs after every format has a replacement

status: proposed, research only. no decoder, dependency, public argument or
wheel has changed. researched on 2026-10-03 against commit
`18183fd3b5cc251cdcbe738e7bdb763a5a5bb7c5`, `image 0.25.10` and the versions in
`Cargo.lock`. [27](27-direct-still-decoders.md) selected every still format's
replacement on 2026-10-04 and [28](28-animation-container-decoders.md) selected
every animated format's and both container fallbacks' the same day; no
replacement has been implemented. The two selections together leave the
removal with no undecided format, which is what this plan's gate waits for;
what is left is the implementation and the measurement.

## goal

Make probing, decoding and writing frames belong to the plugin's own format
adapters, then remove the `image` crate **after all currently supported formats
and fallback cases have replacements**. Reuse a codec library where one already
exists. Reimplementing the integration does not require rewriting its compression
algorithm or replacing every crate maintained by the image-rs organization.

The motivation is a read path suited to VapourSynth: metadata without rendering
a picture, demand-aware alpha decoding, bounded animation state, and rows or
planes written with as few intermediate allocations and copies as the backend
permits. Removal alone is not evidence of a speed improvement. We need to bench
against an image-rs baseline to ensure the proposed paths really improve
performance, including clip creation and memory use.

The supporting plans are:

| plan | responsibility |
| --- | --- |
| [27 direct still decoders](27-direct-still-decoders.md) | all still formats, direct dependencies, internal image-rs codecs and candidate alternatives |
| [28 animation and container fallbacks](28-animation-container-decoders.md) | GIF/WebP timeline discovery and composition, AVIF/HEIF fallback coverage |
| [29 decoder types](29-decoder-types-without-image.md) | replace image-rs enums, reader dispatch and frame wrappers with existing project types or small local definitions |

## what the current code establishes

`src/decoder.rs::describe` uses container probes for eligible HEIF/AVIF, JPEG XL
and JPEG 2000. Everything else constructs an `ImageReader` decoder, asks for
dimensions, decoded and original color types, ICC bytes and orientation, then
drops that decoder. `decode` first tries direct format modules and PNG streaming,
then opens another image-rs decoder, allocates a zeroed interleaved picture,
calls `read_image`, and leaves the planar write to `src/pixel.rs`.

These are different costs and should be counted separately:

| path | evidence | what to investigate |
| --- | --- | --- |
| GIF and animated WebP | `src/animation/frames.rs::durations` walks `into_frames`, decoding and composing every presentation, then `StreamSource` opens another pass for playback | confirmed pixel decoding during timeline discovery followed by playback decoding, including presentations sampling may never display |
| generic JPEG | image-rs `JpegDecoder::new` reads the compressed file and calls `decode_headers`; `icc_profile` and `orientation` each create another header decoder; `read_image` constructs the decoder that produces pixels | repeated header parsing and compressed-file reads, not two full JPEG pixel decodes |
| generic still fallback | `decode` makes `vec![0; total_bytes]`, then `read_image`, then the frame writer | initialization and interleaved-to-planar work, not automatically a second decompression |
| TIFF and EXR | image-rs adapters create a backend-owned result and copy it into the caller's buffer | an additional whole-picture buffer and copy before the plugin's planar write |
| AVIF fallback | image-rs `AvifDecoder::new` renders the primary item and linked alpha before dimensions can be queried | constructor-time decode remains on routes that bypass the project's container probe or still use its image-rs decode fallback |
| PNG streaming | `formats/png.rs::walkable` parses headers, and `Rows::fill` opens its reader for pixels | header reopening remains, but the existing row path already removes the generic whole-picture intermediate |

Do not attribute the old lookahead re-decode problem to image-rs. [01](01-lookahead-scheduling.md)
already addressed that scheduling problem. The native AVIF probe also already
removed the old full decode on clip creation for the inputs it handles. Count
remaining work per actual route rather than carrying those historical claims
over to every file.

## status of the order of work

**Steps 1 to 4 are done.** Nothing animated is replayed through the `image`
crate any more -- `src/animation/frames.rs`, the last adapter that did, is
deleted -- and the avif and heif fallbacks belong to `libheif` rather than to
it. What is left of the crate is one adapter (`src/still.rs`) and the still
formats of step 5, which the next goal takes up.

The commits, in order: `398cf4e` the shared types, `d2287ea` the route audit,
`f42def2` jpeg, `2b0d2e8` the shared exif reader, `049463d` the png probe,
`21c1e14` the gif compositor, `fa808f4` the webp container walk, `9549430` the
webp canvas and blending, `380589b` the webp replay, `2de10de` the avif extent
join, and `e08b621` the libheif fallback.

### heif and avif without `image`, in progress

A second goal takes the last two containers off the crate. The route is now:
`avif.rs` decides which library owns an avif from the same walk the decode
needs for the pixels, and everything that is not the yuv its own walk reads --
an r,g,b container, a monochrome one, one the walk refuses -- goes to
`heif.rs`, which reads it through libheif. The probe asks the same question the
same way, so the two cannot disagree.

- `61c6dba` gave `HeifHeader::format()` the `Rgb(C444)` arm it was missing.
- `c4d17b8` taught `heif::decode` the r,g,b arrangement -- libheif fills the
  red, green and blue channels for one and leaves the luma channel empty, which
  is what made it fail with `has no luma plane` -- and routed those containers
  to it. One property moved: a file with no alpha channel now reports
  `ImgSeqOriginalColorType=rgb8` where the `image` decoder named `rgba8`.
- `4555b4d` routed the monochrome avifs the same way, which removed the last
  `image` call from the avif still path.

What made the second step worth measuring rather than assuming: the plan
expected `color_type` to move from `Rgba8` to `La8` for `mono-alpha.avif` and
called that user-visible. It is not. `ImgSeqOriginalColorType` was already
`La8` -- only the internal decode layout changed -- and a before/after check of
the colour clip, the alpha clip and every property found **nothing** moved for
any monochrome avif, so no changelog entry was written for it. The r,g,b step
did move a property, and that one is in `CHANGELOG.md`.

- `fdc9b86` removed the last of it: the `libheif_rs::integration::image` hooks
  that taught `image` to read a heif (and `libheif-rs`'s `image` feature), and
  the stale `Format::Avif`/`Format::Heif` arms in `src/still.rs`, whose own doc
  already said `None` was the answer for a container an adapter owns.

Step 3 changed nothing observable, and that is the finding rather than a
disappointment: the colour clip, the alpha clip and every probe fact are
byte-identical across all 8 heic fixtures and all 23 avif ones. Every container
was already being read by the module that owns it, so the hooks were dead
weight. It is also *provable* that they are gone -- `image` can no longer read
a `.heic` at all now, which is why the r,g,b equivalence test no longer names
`animation.heic`. `image` reads an avif itself through `avif-native`, so that
half of the comparison still works and still passes.

Step 2 is done, in `398cf4e`. The shared representations exist
(`src/layout.rs`), the identification does too (`src/format.rs`), and every
remaining `image` call in production code is behind one adapter
(`src/still.rs`). Probe, samples, properties and the public source labels are
unchanged: 186 unit tests pass, the release validator reports `all checks
passed`, and the one error message a container's refusal carries is
byte-identical to the old build's.

Step 1's route audit is recorded below, and the fixtures it asked for are the
ones [28](28-animation-container-decoders.md) already committed.

Step 3 is done: `f42def2` moved jpeg onto `zune-jpeg`, `2b0d2e8` shared the
exif reader out of it, and `049463d` moved the png probe onto the `png` crate.
Between them they own the facts `image` supplied: the ICC profile, the exif
orientation (through `src/exif.rs`, which reads both byte orders), and for a png
the `cICP` chunk the crate never exposed at all.

The jpeg measurements, interleaved against a build of `5b10d67`:

| jpeg, 35 pages | before | after |
| --- | ---: | ---: |
| clip creation (`open`) | 104–121 ms | **3 ms** |
| frames | 2.15–2.61 s | 2.16–2.20 s |

That probe win is the point of the migration: `image`'s jpeg reader reads the
whole compressed file to identify it and again for each accessor, so creating a
clip over the 213 MB corpus read all of it several times. This reads the header
bytes and stops. The png probe is parity rather than a win — `image`'s png
reader was already reading headers only — and it is what lets step 6 remove the
crate from that format at all.

What was checked, both before and after, for each half:

- `frame-parity.py`: 245 of 245 lines byte-identical, over the six sandbox sets
  and `tests/fixtures`.
- A probe-facts diff over every png fixture, the 35 page png set and the 35 file
  mixed set: identical, including all eight exif orientation fixtures and the
  four `mono-*`/`alpha-*` depth cases.
- Six hand-made edge cases the corpus has none of — a palette page, a palette
  page with `tRNS`, one and four bit grey, four bit grey with `tRNS`, and an
  interlaced page: identical probe facts *and* identical pixel hashes.
- The release validator reports `all checks passed`.

Step 4's gif third is done, in `21c1e14`. `src/animation/gif.rs` reads the
timeline with `skip_frame_decoding(true)` and composes the canvas itself, which
is the port [28](28-animation-container-decoders.md) called for. `frames.rs` is
now WebP alone and says so.

Two things about that port are worth keeping in view, because both are places
where the obvious implementation is wrong:

- The displayed picture must be built from the sub-rectangle *and* the
  undisposed canvas. Keeping the drawn canvas instead — the first thing I
  wrote — makes every frame after the first read content the last one had
  already replaced, and it is invisible on a file whose frames cover the whole
  canvas.
- `Previous` restores the last *undisposed* picture, not the previous frame's
  displayed picture. A `Previous`-disposed frame's drawing therefore does not
  become the next frame's base, which is what makes it usable for a one-frame
  flourish.

Verified three ways: `frame-parity.py` 245 of 245 lines byte-identical, every
fixture's pixel hashes identical to the compositor it replaced, and the
independent oracle **14 of 14 frames match Pillow**. Seven unit tests pin the
compositor's own behaviour, including the two deliberate departures from the
specification.

Step 4's webp third is half done, in `fa808f4`: `src/animation/webp.rs` walks
the RIFF container and reads the canvas, the loop count, the alpha/ICC/exif
flags and every frame's rectangle, duration, blend and disposal, with the
payload range that a decode will be handed. It is a walk and not a decode, so a
timeline costs no pixels. Eight unit tests pass, asserting the fixture's real
fields against the values `webpinfo.exe` prints for it -- an independent
cross-check, and the one that caught that my own expectation for frames 2 and 3
was wrong, not the walk.
Nothing consumes the walk yet, so the plugin is unchanged: `frame-parity.py`
is 245 of 245 and the validator passes.

Two things the compositor needs, both found while reading what it replaces:

- `image-webp` never sets a background colour. It keeps the `ANIM` background
  as a `hint` and leaves the colour a disposal would clear to as `None`
  unless a caller sets it, and `image` does not. A disposal is therefore
  **inert** in the path being replaced. Reproducing that is what keeps the
  pixels identical, and [`Animation::dispose_is_inert`] records it.
- Alpha blending is not a plain multiply: the path uses libwebp's integer
  routine, whose `div_by_255` rounds to nearest rather than down. It is in
  `image-webp`'s `alpha_blending.rs` and has to be ported exactly.

Step 4's webp third is done, in `380589b`. `src/animation/webp.rs` replays the
timeline the walk reads, builds the smallest container libwebp will decode one
frame out of, and composes the canvas with libwebp's own integer alpha
blending. **`src/animation/frames.rs` is deleted**: no animated format is
replayed through the `image` crate any more.

Verified four ways:

- `frame-parity.py`: 245 of 245 lines byte-identical to the baseline.
- Every fixture's pixel hashes identical to the path it replaced, including
  `animation.webp` and `lossy.webp`.
- **14 of 14 frames match libwebp's own `anim_dump`**, which is the oracle that
  settles whether the compositor is right rather than merely unchanged.
- The release validator reports `all checks passed`.

Two things the port turned up, both worth keeping:

- The blending routine is **lossy even for an opaque source**: a sample of 10
  over anything comes back as 9, including over a transparent canvas, because
  the renormalisation's 24-bit reciprocal truncates. Three of my own
  from-first-principles expectations for it were wrong and the port was right,
  so it is pinned with exact values rather than tolerances.
- `webpinfo` shows every frame of the fixture uses *do not blend*, so the
  fixture never exercises blending at all. It had to be verified by
  construction, and the fixture's pixels could only ever settle the copy path.

The first attempt broke `lossy.webp` outright. Claiming every `.webp` means a
still webp reaches the walk, and a plain lossy webp has no `VP8X` header at
all, so the walk errored where it had to *decline*: `parse` now answers
`Ok(None)` for a webp that states no animation, which is what keeps every still
webp on the libwebp path. A test pins both directions.

### step 4's remaining piece

**The extent join is done, in `2de10de`.** `data_range` became `data_ranges` and
hands out every extent of an item, `primary_data`/`alpha_data` became
`primary_ranges`/`alpha_ranges`, and the three read sites join them through a
new `read_ranges`. `limit` now bounds the *joined* payload rather than each
extent, so a prefix request does not read the same length out of every extent.
`native_eligible` stopped refusing several extents and still refuses a grid and
an unknown construction method, which is what it should do.

What changed for a user: `avif-split-extents.avif` is now `YUV420P8`, the yuv
its container states, where it used to be the `RGB24` the `image` decoder
built. The validator's extents section no longer asserts a format and no longer
needs `mismatch`; it checks the joined file's planes against the whole file's
**plane for plane**, which is a stronger check than the sample-by-sample one it
replaced and the one that would catch a wrong extent order.

Verified: every plane of `avif-split-extents.avif` is byte-identical to
`avif-yuv420p.avif`'s, 209 unit tests pass, `clippy -D warnings` and `fmt
--check` are clean, the validator reports `all checks passed`, and a fixture
pixel diff shows the split-extent file as the **only** changed fixture in the
tree.

**Step 4 is done**, in `e08b621`. A container this tree's own avif walk refuses
-- a grid of tiles, a construction method the walker does not follow -- is now
described and decoded by `libheif`, which plan
[28](28-animation-container-decoders.md) decides as "one library for every
refused container". `avif::image_info` delegates and `avif::handles` reads the
same walk, so the probe and the decode cannot disagree about who owns a file.

What made this worth doing rather than just re-routing is what the run showed:
**`libheif` opens the grid and `image` could not**. The `image` decoder has no
monochrome avif, and a `avifenc -g 2x2` grid's cells are monochrome, so it
answered `Invalid argument` for a container libheif reads back at 256x256. The
validator's grid section was written to fail the moment this happened and
named the four cell values to check, so it became a real sample check: the
four quadrants read 8, 10, 15 and 17, taken at each cell's middle so a wrong
cell order cannot pass.

A fixture pixel diff shows the grid as the only fixture this commit changes --
from an error to `Gray8` 256x256 -- beside the split-extent file from the
previous one. Nothing else in the tree moved.

Everything below is the scoping written before either piece landed, kept
because it is the record of what was decided and why.


Both of these change what the plugin hands out rather than only where it comes
from, which is why they were not a quiet continuation of the gif and webp work.
Each needs a `CHANGELOG.md` entry and an updated validator, and neither can be
checked by "the pixels are unchanged".

**1. Join an item's extents in the container walker.**
`avif-split-extents.avif` is the coded item of `avif-yuv420p.avif` cut in half
and located as two extents: the same picture, written a legal way this reader
declines. `Meta::native_eligible` refuses it and the probe therefore describes
it as the format the `image` decoder produces, so the file currently comes out
`RGB24` where the file it was cut from comes out `YUV420P8`.

What has to change, all inside `src/formats/avif.rs`:

- `data_range` (line ~1070) returns one `Range<usize>` and refuses more than
  one extent. It becomes a multi-range accessor: every extent of method 0 is
  relative to the file and every extent of method 1 to `idat`, and each range
  still has to be checked against its container before a caller allocates it.
  `idat` may hold only one extent, which the specification already says and
  which is worth keeping as an explicit refusal.
- `primary_data` and `alpha_data` (~1040 and ~1048) hand out those ranges, and
  the five production call sites (~107, ~267, ~272, ~1028) read the bytes. The
  three read sites concatenate, which is the actual join.
- `native_eligible` (~1027) stops refusing several extents. It must keep
  refusing a grid and an unknown construction method.
- Eight unit tests assert the old refusal, including two that assert
  `primary_data` equals a single range; they become join assertions.
- `tests/readalpha.vpy`'s extents section turns from "the fallback's format"
  into a sample check against `avif-yuv420p.avif`, which is the point: the two
  files become the same picture through the same decoder. The `mismatch`
  assertion at the end of that section goes away with the format difference.

**2. Send a refused avif or heif to libheif rather than to `image`.**
Plan [28](28-animation-container-decoders.md) decides "one library for every
refused container". Today `src/formats/avif.rs` (~91 and ~235) asks
`native_eligible` and leaves a `false` answer to the `image` hooks, whose error
string is what the validator's grid section pins today
(`Format error decoding Avif: Invalid argument`).

So this one moves two validator sections at once: the grid's refusal becomes
whatever libheif does with a grid, and any container libheif also refuses needs
its new error string written down. `formats/heif.rs` already has the entry
point for a colour page, which is what plan 28's "HEIF RGB page" row names, so
the work is the routing and the expectations rather than a decoder.

### step 5, the remaining still formats

Started, in `f329f79` for qoi and `3aedaee` for farbfeld. The order and the
per-format decisions are [27](27-direct-still-decoders.md)'s; this is what has
landed so far.

- **qoi** — done. `qoi 0.4.1` was already in the lock behind `image`, so the
  crate was promoted and [`src/formats/qoi.rs`](../../src/formats/qoi.rs) reads
  the fourteen byte header for the probe and runs the decoder into the frame's
  buffer. `image`'s own `qoi` feature is now off, which is what proves the crate
  no longer reads one. There were **no committed qoi fixtures at all**, so
  [`tests/make-qoi-fixtures.py`](../../tests/make-qoi-fixtures.py) wrote three:
  three channels, four channels with varying alpha, and one whose colours flag is
  set. That third one exists only to pin the parity rule -- the flag is
  informative and must never become a `_Transfer` property -- and the validator
  checks that its planes, alpha, source label and colour properties all equal the
  plain file's.

- **farbfeld** — done. [`src/formats/farbfeld.rs`](../../src/formats/farbfeld.rs): a
  magic and two numbers, then one big-endian `u16` per channel per pixel.
  `image`'s `ff` feature is off. The format has one spelling, so
  `alpha-rgba16.ff` was already the whole surface and no new fixture was
  needed.

Parity was exact for both: probe facts, colour planes and both `ReadAlpha`
clips are byte-identical to the build before each, across the whole fixture
set, and no changelog entry was written because nothing a user can see moved.

What is left, in plan 27's order, with the fixtures each still needs:
- **bmp and ico** — **done**. Fixtures in `2bd0edc`, the ports in `6603a6d` (bmp) and
  `6a7a246` (ico), and `image`'s `bmp` and `ico` features are both off. Parity is
  exact: probe facts, colour planes and both `ReadAlpha` clips are byte-identical
  across all thirteen `bmp-*` and all three `ico-*` fixtures. The fixture work
  found three things worth recording:
  Sixteen files from [`tests/make-bmp-ico-fixtures.py`](../../tests/make-bmp-ico-fixtures.py).

  - **`magick` cannot write the BMP fixtures.** Its BMP encoder ignores
    `-depth` and `-compress`: `-depth 1`, `-depth 4` and `-depth 8` each wrote an
    eight bit `BI_RLE8` bitmap, `-depth 16` wrote a thirty-two bit one, and a
    `-flip` turned the `-depth 24` recipe into a bitfields file. The first pass
    silently produced four copies of one file under four names. Every BMP header
    is written out field by field here instead, and the script's own docstring
    says so.
  - **The research corpus has no PNG-payload icon.** Its `png-payload.ico`
    holds a bare DIB -- the directory entry points at DIB bytes, not at a PNG
    signature -- so the PNG arm of the ICO payload sniffer was never exercised.
    `ico-png.ico` is written by hand and is the file that exercises it.
  - **The baseline confirms every parity rule empirically.** Read against the
    build before the port, the four thirty-two bit cases split exactly as the
    plan says: `bmp-rgb32` (BI_RGB, V3) and `bmp-rgb32-v5` (BI_RGB, V5) both
    report `Rgb8` with an opaque alpha clip, so the fourth byte is dropped in
    both; `bmp-bitfields32-noalpha` (alpha mask zero) is `Rgb8` too, while
    `bmp-bitfields32` (mask `0xFF000000`) is `Rgba8` with real alpha. The two
    no-alpha files share one identical alpha hash, which is what makes the rule
    a measurement rather than an assertion.
- **tga** — **done**. Fixtures in `490117f`, the port in `fa9ec1d`, and `image`'s
  `tga` feature is off. Parity is exact: probe facts, colour planes and both clips
  byte-identical across all twelve fixtures. Twelve files from
  [`tests/make-tga-fixtures.py`](../../tests/make-tga-fixtures.py) covering all six image
  types the format uses in practice (the three raw ones and their three
  run-length forms), the four depths that change the outcome, and the two
  descriptor directions. Written out field by field rather than by ImageMagick,
  for the reason the BMP fixtures are: `magick` does not honour the options that
  select these subtypes, so a recipe asking for sixteen bits can quietly write
  thirty-two. The writer had a bug on its first run worth recording, because it
  is the kind that produces *plausible* files: the colour map entry size is a
  single byte at offset seven, and writing it as a word shifted the origin,
  width, height, depth and descriptor each by one, so all twelve files were
  refused. The header is checked back against the plugin now, and the survey
  prints every field.
  The rule the plan names is confirmed rather than assumed: `tga-rgb32-attr0.tga`
  is a thirty-two bit image whose descriptor states **zero** attribute bits, and
  it is handed out as `Rgba8` -- the opposite of what the same fourth byte does
  in a BMP. The other baselines are `Rgb5x1` for the sixteen bit file, `La8` for
  the grayscale-with-alpha one, `L8` for plain grayscale, and `Rgb8` for the
  rest.
- **dds** — **done**. Fixtures in `c783877`, the port in `d9392ab`, and `image`'s
  `dds` feature is off. Parity is exact: probe facts, colour planes and both
  `ReadAlpha` clips are byte-identical across all seven fixtures. Six new files
  from [`tests/make-dds-fixtures.py`](../../tests/make-dds-fixtures.py) beside the
  `alpha-dds.dds` the validator already had: the three variants by four character
  code, the DX10 spelling of two of them, and one that states a mipmap count and
  sets the cube map and volume bits. Written here rather than by ImageMagick, for
  the reason the BMP and TGA fixtures are. The baseline pins the rule in two
  ways worth keeping: `dds-dxt1` and `dds-dx10-bc1` share one alpha hash and
  `dds-dxt5`, `dds-dx10-bc3` and `dds-dxt5-ignored` share another, so the DX10
  spelling reaching the same variant as the code, and the ignored bits changing
  nothing, are measurements rather than assertions. DXT1 and its BC1 equivalent
  are `Rgb8` with no alpha; the other four are `Rgba8`.

  The port's own trap, recorded because it is the sort that reads as plausible:
  this format widens a five or six bit channel by **truncating** division
  (`v * 255 / max`), where the bitmap and targa readers round to nearest. Five
  bits of three are 24 here and 25 there. The plan's phrase "image-rs's exact
  5-to-8 bit expansion" is that difference, and reusing `bmp::expand` would have
  been wrong -- as the author of this note found out by writing the
  round-to-nearest table into the test that exists to tell the two apart.
- **pnm** — **done**. Fixtures in `1bb56df`, the port in `6cf86fc`, and `image`'s
  `pnm` feature is off. Parity is exact everywhere except the wording of one
  error: 15 of the 16 fixtures are byte-identical in probe facts, colour planes
  and both clips, and the sixteenth is `pnm-ascii-comment.pgm`, which both
  readers *refuse* -- only the message differs, and a diagnostic is not a frame.
  Sixteen files from
  [`tests/make-pnm-fixtures.py`](../../tests/make-pnm-fixtures.py) beside the two the
  validator already had, and the set covers the rule's three parts rather than
  the format as a whole:

  - **`P1` to `P7`**, which is seven subtypes and not one. The three ASCII
    rasters, the three binary ones, and the tagged container in four spellings
    (`RGB`, `RGB_ALPHA`, `GRAYSCALE`, `GRAYSCALE_ALPHA`), so the tuple type is
    what varies between the last four.
  - **The comment asymmetry, which is the part worth having.** A comment between
    two header fields is legal and `pnm-comment.pgm` carries two; a comment
    inside the raster of an *ASCII* file is not, and `pnm-ascii-comment.pgm` is
    refused with "Non-ASCII-digit character when parsing number in sample". The
    same bytes in a binary subtype are data rather than a comment, so the two
    files together are what hold the distinction.
  - **The `f32` rescale.** `pnm-p7-maxval31.pam` states a `MAXVAL` of 31, which
    is not one less than a power of two, so its samples are not a shift of the
    frame's word and the reader rescales them instead of widening them.

  The baseline is otherwise unremarkable and worth stating because three
  formats in a row have been anything but: `P1` and `P4` are labelled `L1` and
  handed out as `Gray8`; eight bit files keep their word; sixteen bit ones are
  `Gray16` or `RGB48`; and the tagged container's alpha spellings are `La8` and
  `Rgba8`.

  Reading the source turned three things up before the port itself:

  - **`autobreak.rs` is not a decoder concern.** The plan lists it as one of the
    three parts to port, and it is an *encoder* helper: it inserts line breaks
    when writing a PNM so a token is never split. This plugin never writes one,
    so it has nothing to port. The reader is the parser and the header, and the
    header is smaller than the plan's 366 lines suggest because a good part of
    it is the same encoder's field formatting.
  - **The `f32` rescale, in full.** After a raster is read,
    `target_sample_max` is 255 for a one byte sample and 65535 for a two byte
    one, `current_sample_max` is the file's `MAXVAL`, and when they differ the
    samples are multiplied by `target / current` as **`f32`** and rounded. Two
    details are easy to miss: the multiplication is floating point rather than
    an integer ratio, and which of the two sizes a file uses is decided by
    `MAXVAL` itself -- `<= 0xFF` is one byte, `<= 0xFFFF` two -- so a file whose
    `MAXVAL` is 1023 reads two byte samples and is still rescaled.
  - **A cross-fixture diagnostic worth keeping.** `pnm-p6-16.ppm` states 65535
    and `pnm-p7-rgb16.pam` states 1023, and both hold the same picture. Their
    red and green planes hash *differently*, because one file's samples are
    exact and the other's have been through the rescale's rounding, but their
    **blue planes hash identically** -- blue is only ever 0 or 255 there, so it
    saturates to the same word either way. A port that drops the rescale, or
    does it in integer arithmetic, will therefore show a partial match: planes
    nought and one wrong, plane two right. That signature points at the rescale
    rather than at the raster reader.
  - **The rescale saturates, and that is load-bearing.** A bitmap's
    `maximal_sample()` is `1`, so `factor` is `255.0` and the one bit subtypes
    go through it too: `PbmBit::from_ascii` writes `0` or `255` for a `0` or `1`
    digit, and `255.0 * 255.0` is `65025.0`. Rust's float to integer `as` cast
    **saturates** -- it is not the wrapping truncation a C cast would give --
    so `65025.0 as u8` is `255` and the file reads correctly. A port that wrote
    the rounding any other way, or that reused a wrapping conversion, would turn
    every white pixel of a `P1` into `1`: a nearly black picture that still has
    the right shape. Measured against the baseline, `pnm-p1.pbm` and
    `pnm-p4.pbm` hold the same pattern and both decode to exactly `0` and `255`,
    which is what pins this rather than the arithmetic looking right.
  - **`P4` unpacks in place and backwards**, one bit a sample, most significant
    bit first, with each row padded to `width.div_ceil(8)` bytes, and writes
    `1 - bit` so that a set bit (black in the format) becomes `0`. Working
    backwards is what lets the packed bytes be expanded into the same buffer
    without a second allocation.
- **hdr** — **done**. Fixtures in `09684f9`, the port in the commit that follows, and
  `image`'s `hdr` feature is off.

  The decision recorded above was taken the way the plan asks: **every** resolution
  spelling reads, and the three that used to be refused now decode. The change is in
  `CHANGELOG.md`. The evidence that the signs are applied rather than ignored is
  that five files storing one picture five different ways -- `hdr-rle.hdr` and the
  four `hdr-orient-*` -- all report the identical plane hash `45fd135aa555e6b2`.
  The six files that already read are byte-identical to the `pnm` build. Ten files from
  [`tests/make-hdr-fixtures.py`](../../tests/make-hdr-fixtures.py): the three scanline
  encodings (flat, the new per-component run-length form, and the old
  repeat-marker form), an exponent sweep, a header with an unknown field, and the
  four resolution signs. The baseline is `RGBS` with `original=Rgb32F` for every
  file that reads at all, which is the rule's first half confirmed.

  **A scope decision has to be made before the port, and it is not a detail.** The
  plan says hdr is "written here ... covering both RLE schemes and the sixteen
  orientations the candidate refuses", which reads as though the orientations are
  the reason to write it rather than port it. But **the reader in this tree today
  refuses them too**: `image`'s hdr decoder accepts the pair `("-Y", "+X")` and
  returns `Unsupported` for every other spelling, with a comment acknowledging
  that the others exist. Measured against the current build, three of the four
  sign combinations fail:

  | resolution | today |
  | --- | --- |
  | `-Y h +X w` | reads |
  | `+Y h +X w` | `does not support the format features Orientation +Y +X` |
  | `-Y h -X w` | `... Orientation -Y -X` |
  | `+Y h -X w` | `... Orientation +Y -X` |

  So the two readings are: **refuse the same three**, which keeps every file's
  behaviour byte-identical and makes the three failing fixtures the spec, or
  **implement all of them**, which is more capable than the tree is today and is
  therefore a change a user can see -- it needs a `CHANGELOG.md` entry and a
  deliberate decision rather than a default. Deciding this after writing the
  reader would mean discovering it from a failing fixture, so it is recorded
  here first.
- **tiff** — **done**, module in `725b97a`, and `image`'s `tiff` feature is off.
  Twelve of the thirteen fixtures are byte-identical in probe facts, colour planes and
  both clips, including the **planar** one, which is the file three attempts went
  into. The thirteenth is `tiff-palette.tiff`, which both readers refuse -- only
  the wording differs (`failed to identify` where the old one said `failed to
  create decoder`), and both refuse it before a frame is promised. The five bugs
  the attempts found are listed below and each is a real trap rather than a typo.

- **exr** — **done**, module in `bf8e172`, and `image`'s `exr` feature is off.
  All eight fixtures are byte-identical in probe facts, colour planes and both
  clips, including the two alpha hashes the baseline pinned: `b748099f3030ff76`
  for the two files with an alpha channel and `883aa43aebbbe3be` for the six
  without, so the channel selection by name agrees with the one by position that
  it replaced. `read_first_flat_layer_from_file` and `MetaData::read_from_file`
  are the two calls, as the research above said. Fixtures from `2205e87`. The crate's
  API is researched, so the next attempt starts from a shape rather than a
  search:

  - **`read_first_rgba_layer_from_file` is the wrong door.** It takes two
    closures and returns a `PixelImage<Pixels, RgbaChannels>`, which is for a
    caller that wants the crate to drive a pixel store. What this module wants is
    the samples, and
    **`exr::prelude::read_first_flat_layer_from_file(path)`** hands them over
    directly as `Image<Layer<AnyChannels<FlatSamples>>>`.
  - **The data is already one flat vector per channel**, which is the shape a
    planar decode wants: `AnyChannels::list` is a list of `Channel<FlatSamples>`,
    each with a `name: Text` and a `FlatSamples` that is
    `F16(Vec<f16>) | F32(Vec<f32>) | U32(Vec<u32>)`. So the channels are selected
    by name -- `R`, `G`, `B`, and `A` when the file has one -- rather than by
    position, and a file with the channels in another order still reads.
  - **A half sample is `half::f16`** (`half 2.7.1` is already in the lock behind
    `exr`), and `FlatSamples::values_as_f32` walks either width as `f32` without
    allocating. The frame is `Rgb32F` whatever the file held, which is the
    baseline: every EXR fixture reports `RGBS` with `Rgb32F` or `Rgba32F`.
  - **`Pixels::Interleaved`, not `Pixels::Planar`.** The plan says exr writes
    planar, and that is true of how the *crate* gives the data, but
    `Pixels::Planar { planes, alpha }` is the yuv shape `webp.rs` builds and its
    planes are byte planes of a subsampled frame. An EXR frame is interleaved
    float `Rgb32F` and is exactly the shape `hdr.rs` already hands over as
    `Pixels::Interleaved`, so that arm is both the proven one and the correct
    one. Interleaving three float channels is a copy per sample, not a transpose.
  - **The probe reads the header alone.** The `image` hook uses
    `read_first_flat_layer_from_file` for both jobs, which decodes the picture
    just to describe it. `exr::meta::MetaData::read_from_file(path, pedantic)`
    reads only what a probe needs -- `MetaData { requirements, headers }` -- so
    the two jobs can ask different questions of the same file without
    disagreeing. Still to look up: the field names on `Header` for its size and
    its channel list, which are not in `meta/mod.rs`.
  - **The intermediate shapes are settled.** `Image { attributes, layer_data }`,
    `Layer { channel_data, size: Vec2<usize>, encoding }`,
    `AnyChannels { list }` and `AnyChannel { name: Text, sample_data }`, with
    `FlatSamples` as `F16(Vec<f16>) | F32(Vec<f32>) | U32(Vec<u32>)`. `list` is
    a `SmallVec`, which is iterated rather than named, so the module needs no
    dependency on it.
  Eight EXR files and thirteen TIFF files from
  [`tests/make-tiff-exr-fixtures.py`](../../tests/make-tiff-exr-fixtures.py).
  Every EXR reads (`RGBS` with `original=Rgb32F` or `Rgba32F`) across all five
  compressions, and twelve of the thirteen TIFFs read. Four things the run
  settled, and the first two are why the script prints what ImageMagick actually
  wrote:

  - **`image` refuses palette TIFF**, and that is the one TIFF that does not
    read: `does not support the format features Photometric interpretation
    RGBPalette`. Whether the `tiff` crate reads one is therefore a **decision**,
    the same shape as the HDR orientations: taking it is more capable than the
    tree is today and needs a `CHANGELOG.md` entry, and refusing it keeps every
    file's behaviour identical.
  - **ImageMagick writes a palette page for a picture with few enough colours,
    whatever was asked for.** The first run produced *eight* files that were all
    palette, so they were all testing one refusal rather than the six subtypes
    they were named for. `-type TrueColor` is what forces the photometric, and it
    has to be per case: applying it to the grey sources turned them into three
    channel ones, which the dump caught on the next run.
  - **`PackBits` is not an ImageMagick compression name**; the TIFF codec it
    means is spelled `RLE`. And **`-depth 16` alone does not make the writer emit
    sixteen bit samples** -- it wrote eight from an eight bit source, and only a
    sixteen bit source produced `16 srgb`. Both were caught by the dump rather
    than by the file being wrong later.
  - **Four bugs the first `tiff` module hit, recorded so the next attempt starts
    from them rather than from the crate's front page.** All four were found by
    the fixture parity diff, and none by the unit tests, which is the point of
    running it:

    1. **`default-features = false` drops the decompressors.** `tiff`'s default
       features are what bring in LZW and Deflate, so `tiff-lzw.tiff`,
       `tiff-deflate.tiff` and `tiff-tiled.tiff` all answered `unsupported
       error`. The crate has to be taken with its defaults, or with the
       compression features named explicitly.
    2. **A four channel file must map to the format of its own depth.** The
       first attempt sent every `RGBA` to `PixelFormat::Rgb32F`, and
       `tiff-rgba8.tiff` then failed with "the frame holds 4 byte samples, but
       the decoder returned 1 byte samples". `Rgba8` is `Rgb8`, `Rgba16` is
       `Rgb16` and only `Rgba32F` is `Rgb32F`.
    3. **`read_image` reads one plane, and `read_image_to_buffer` fixes the
       count and not the layout.** The crate documents that `read_image` "will
       currently only read the first sample's plane" and calls
       `result_extent_for_planes(0..1)` to do it: `tiff-planar.tiff` came back as
       851 samples where 2553 belong. The plane aware call is necessary and **not
       sufficient** -- it returned all 2553 and then wrote them out as if they
       were interleaved, and the raster is three planes laid end to end, so the
       picture had the right size and the wrong colours. It returns a
       `BufferLayoutPreference`, which is the instruction the second attempt
       ignored: a third should read the file, check that preference, and
       reorder the planes itself when it says `Planar`.
    4. **The sample count guard is worth keeping even after that**, because
       `read_image_to_buffer` falls back to one plane when the file's own size
       exceeds the decoder's buffer limit.
    5. **A palette page is refused at *identify*, not at decode**, which keeps
       the probe from promising a frame the decode would refuse. That part
       worked.

  - **The four channel sources land as three channel colour plus an alpha clip**:
    `tiff-rgba8.tiff` is `RGB24 original=Rgba8` and every EXR RGBA is `RGBS
    original=Rgba32F`, with the alpha in `GrayS` on its own clip.

### the benchmark check this step still owes

Every slice above is a parity migration, so none of them claims a speed
improvement and **fewer `image` APIs is not evidence of one**. What they owe is
the opposite check: that nine formats moved off `image` without making the read
path slower. That has not been run yet, and it is the one item on this list that
is not a format.

It is a paired A/B against the pre-step-5 build, which is
`target/bench/vs_imageseqs-nohooks.dll`. `docs/BENCH.md` names the harness and
the shape to copy:

- `target/bench/ab-sets.py` is the tool the page uses for exactly this -- two
  builds, alternating inside one batch, per set -- and its own section says a
  run-to-run spread of up to 4% makes a single pair meaningless, so the number
  to read is the ratio over several rounds rather than any one pass.
- **The corpus is built and verified.** `target/bench/make-step5-corpus.py`
  writes 80 files, 202 MB, from one `sandbox/png` page in each of the ten moved
  formats, eight copies each so a run takes seconds. Every one of the ten reads;
  the formats and what they hand out are `bmp`, `dds`, `tga`, `qoi`, `tiff`,
  `ppm` as `RGB24`, `hdr` and `exr` as `RGBS`, `ff` as `RGB48` (its samples are
  words), and `ico` at 256x192 as `RGB24`. Its own writer was wrong first --
  farbfeld is **always four channels**, so an r,g,b buffer has to be widened
  with an opaque alpha, and the plugin refused all eight files with "holds
  6480000 bytes of pixels where the header states 8640000" until it was.
- The sets that matter here are the ones whose formats moved: `sandbox/png`
  holds no step-5 format and is the control, while the fixtures tree holds
  every one of them. A set of the moved formats at a size worth timing does not
  exist yet and would have to be built, because the committed fixtures are
  37x23.
- `frame-parity.py` is the pixel half and has been run: 0 of 245 differ, which
  is recorded per slice above. The speed half is what is missing.

**It has been run, and it found four regressions.** `target/bench/step5-ab.py`,
five rounds, alternating the two builds inside each round, `prefetch=0`, against
`vs_imageseqs-nohooks.dll`. The per-round ratio is what decides, because the
best-of column alone hides drift:

| format | per-round after/before | reading |
| --- | --- | --- |
| `bmp` | 0.61 0.55 0.66 0.55 0.71 | **faster**, every round |
| `ff` | 0.44 0.33 0.44 0.41 0.38 | **faster**, every round |
| `ico` | 0.54 0.41 0.46 0.44 0.43 | **faster**, every round |
| `qoi` | 0.70 0.70 0.63 0.74 0.97 | **faster**, four of five |
| `tiff` | 1.09 1.04 0.88 1.01 0.93 | unchanged |
| `exr` | 0.94 0.99 1.34 0.98 1.31 | unchanged, wide |
| `dds` | 1.57 1.36 1.20 1.18 1.14 | **slower**, every round |
| `hdr` | 1.41 1.71 1.29 1.35 1.47 | **slower**, every round |
| `ppm` | 1.00 1.23 1.46 1.84 1.10 | **slower**, four of five |
| `tga` | 2.04 1.28 1.14 1.46 1.11 | **slower**, every round |

Four of the nine ports are about **1.2x to 1.3x slower** than the reader they
replaced, and the goal's own rule is that a regression is investigated and
fixed rather than noted. The four are `dds`, `hdr`, `ppm` and `tga`; the other
five are faster or unchanged, so this is not one systemic mistake but something
the four have in common.

**It is the probe, not the decode, and the first diagnosis was wrong.** Splitting
the two stages for six formats, one measurement each, milliseconds:

| format | probe before | probe after | decode before | decode after |
| --- | --- | --- | --- | --- |
| `hdr` | 2.5 | **23.3** | 318.4 | 330.6 |
| `tga` | 1.5 | **24.4** | 53.1 | 72.7 |
| `ppm` | 1.5 | **22.1** | 47.8 | 53.2 |
| `dds` | 5.0 | **11.3** | 101.7 | 92.8 |
| `bmp` | 2.6 | **19.8** | 104.1 | 60.5 |
| `qoi` | 1.4 | 7.8 | 121.8 | 81.7 |

**Every module here reads the whole file to parse a header, and `image` reads only
the bytes the header occupies.** The probe went from 1.5 to 5 ms to 8 to 24 ms, a
10x to 16x increase, and that is the regression: `hdr`'s decode is 318 ms before
against 331 after, and its probe is 2.5 against 23.3. The decode got *faster* on
four of the six, which is why `bmp`, `ff`, `ico` and `qoi` came out ahead anyway
-- their decode wins were larger than the probe they had added.

So the fix is one change in all of them, not four: **probe from the header alone.**
`image_info` should read the first few kilobytes rather than the file, which is
what the `image` decoder did and what `png.rs` in this tree already does. Every
module listed above has its own `std::fs::read(path)` in `image_info` and each one
is the whole file.

**The probe fix is in, and it did not close the gap.** `image_head` reads at most
64 KB and every one of these modules now probes through it, which is the right
shape and is parity neutral -- no pixel and no probe fact moved, and the
validator still passes. But re-measuring after it, `dds`, `hdr`, `ppm` and `tga`
are still 1.17x to 1.28x, so the probe was **not** the whole of it either:

| format | per-round after/before, after the probe fix |
| --- | --- |
| `dds` | 1.16 1.25 1.33 1.24 1.18 |
| `hdr` | 1.15 1.17 1.16 1.15 1.18 |
| `ppm` | 1.25 1.16 1.20 1.23 1.30 |
| `tga` | 1.24 1.29 1.29 1.21 1.27 |

Two of the four now have a *stable* ratio across every round (`hdr` at 1.16 and
`dds` at 1.2), which the earlier runs did not -- those are the two to look at
first, because a stable 1.16 is a cost and not drift. Three things were tried
against this and two were wrong: per-row allocation (flattened `hdr.rs`, no
effect) and the whole-file probe (fixed, no effect on these four). The next step
is a profile of `hdr` and `dds` specifically rather than another guess, since two
guesses have now been measured and both were wrong.

The probe reading is still worth keeping regardless: it is a 10x to 16x cut in
what a probe reads, it is why `bmp` went from 0.61 to 0.56 and `ff` from 0.41 to
0.38, and a probe that reads megabytes to look at a few hundred bytes is wrong on
its own terms.

**`hdr` is fixed; the cause was the full-picture buffers, not the per-row ones.**
The rewrite keeps one allocation -- the frame -- and one reusable scanline, where
the reader built three whole-picture buffers: a 4 byte scanline buffer, a 12 byte
float buffer and a 12 byte byte buffer. On 1200x900 that is 4, 13 and 13 MB of
zeroed memory traffic to produce 13 MB of output.

| format | before | after the rewrite |
| --- | --- | --- |
| `hdr` | 1.15 1.17 1.16 1.15 1.18 | **0.85 0.95 0.92 0.99 0.98 0.97** |

Every round is now at or below 1.0, so `hdr` is faster than the reader it
replaced rather than 1.16x slower. This also explains why the earlier per-row
flattening did nothing: it removed the *per-row* allocations and left all three
whole-picture ones, which were the cost. Two guesses were wrong for the same
reason -- each fixed something real that was not the thing being measured.

**`dds` improved but is not conclusively fixed.** Its variant match was inside
the block loops, where it cannot change between blocks; hoisting it so each arm's
loop is monomorphic took the best-of ratio from 1.302 to 1.135. Pixels are
unchanged and 266 tests pass. But the per-round ratios are still 0.92 1.33 1.36
1.07 0.99 1.23, which is two below 1.0 and four above: `dds`'s group takes about
40 ms for eight files, so this set is at the edge of what the harness can
resolve, and a wider corpus of that format is what a conclusion needs. Recording
that as unresolved rather than calling 1.135 a fix.

**The harness was wrong before the code was: `ppm` is not a regression and `tga`
is.** The stage timing inside `pnm` settles where its decode time goes, on a
3.24 MB file, twenty rounds: `fs::read` **1.33 ms**, header **0.001 ms**, raster
**0.57 ms**. The whole module is 1.9 ms a file and seven tenths of it is the
file read, so there is almost nothing in `raster` for a regression to hide in.

Seven paired repetitions of the decode stage say the same thing, and they say
something the earlier numbers did not:

| format | before, 7 reps | after, 7 reps |
| --- | --- | --- |
| `ppm` | 33.9 39.1 42.2 47.5 55.1 65.1 101.4, **median 47.5** | 41.0 42.0 42.4 42.7 42.9 43.1 55.2, **median 42.7** |
| `tga` | 41.3 41.5 42.8 43.4 45.0 45.6 53.8, **median 43.4** | 55.3 55.7 55.7 58.4 58.6 59.7 61.0, **median 55.7** |

`ppm`'s before row ranges from 33.9 to 101.4 and its after row does not: the
*baseline* is the noisy build, and `step5-ab.py`'s best-of statistic rewarded it
for that. By median `ppm` is **faster** after the port, 42.7 against 47.5, so it
was never a regression and the 1.19x was an artifact of the statistic. `tga` is
the opposite and is real: the two ranges do not overlap at all, 43.4 against
55.7, a stable 1.28x.

**`tga`'s profile, the same way, on a 3.24 MB file over twenty rounds:**

    read 1.95 ms   header 0.001   stored_pixels 0.70   orient+expand 0.65   reverse 0.64

The module is 3.95 ms a file, and the interesting line is the last one:
`reverse_encoding` is a **whole extra pass over the picture** doing a byte swap of
channels nought and two, and `expand` is already reading every stored pixel on
its way to the wider one. Fusing them is one loop instead of two and is worth the
0.64 ms, which is 16% of the module and about 5 ms of the eight file run the
harness measures. That is the identified next step for `tga` and it is a
measurement rather than a reading: the pass is named and timed, not inferred.

**But the fusion is not obviously a win and this note corrects the claim above
before it was acted on.** Reading the two functions: `reverse_encoding` is
`chunks_exact_mut(channels)` and a `swap(0, 2)` per pixel, which is 0.64 ms for
3.24 MB or about 5 GB/s -- already at memory bandwidth. And `expand`'s raw branch
is a `copy_from_slice`, a memcpy. Fusing them means replacing that memcpy with a
per-pixel three byte swap loop, which is slower per byte than a memcpy plus a
separate pass that the compiler can vectorise. The saving would be one traversal
and the cost would be a worse inner loop, and which wins is not something reading
the code can say.

**It was written and measured, and it did not pay.** The single pass was built:
`expand` exchanged blue and red as it wrote, in all three branches, and the
separate `reverse_encoding` walk was deleted. Pixels byte-identical, 266 tests,
clippy clean. Then seven paired repetitions of the decode stage, median:

| build | median | all seven |
| --- | --- | --- |
| pre-step-5 (the reader replaced) | **43.6** | 40.7 43.2 43.2 43.6 48.0 48.3 50.5 |
| before the fusion | 58.9 | 54.8 55.5 58.8 58.9 60.6 61.1 62.3 |
| after the fusion | 58.3 | 54.2 56.4 56.9 58.3 60.9 61.6 73.8 |

0.6 ms of 59, about one percent, and inside the spread. So the fusion was
**reverted**: more code for no measurable gain, and a change that adds a branch
to every branch to save nothing is worse than the two passes it replaced.

That is the prediction above confirmed rather than assumed, and it is the first
of these five attempts where the reasoning held up under measurement -- even
though the answer was "no". **`tga` therefore has a measured profile and no
fix.**

The parity check also caught one real bug on the way: the first fused version
left the fifteen bit branch exchanging red and blue, and `tga-rgb16.tga` came
back with planes nought and two swapped. The pixel diff found it immediately;
the unit tests did not.
So `tga` is three passes over the picture where one might do, and whether
collapsing them helps is itself unmeasured. The honest state is that `tga` has a
**measured profile** and **no measured fix**, and the next step is to write the
single pass and measure it, reverting it if it is not faster -- not to assume the
fusion pays because the pass it removes is named. That is the same mistake as the
three before it, one level up: a cost that is real and a saving that is assumed.

The read is 1.95 of the 3.95, as it was for `pnm`, so the ceiling on what any of
this can win is bounded by the file read -- which is also why `ppm` turned out not
to be a regression at all.

So one format is left, not two, and the numbers that said otherwise were
measuring the harness. **Read the median of paired repetitions, not the best of
them** -- a build that occasionally stalls makes best-of look better than it is,
and that is what happened here for three rounds.
**`ppm` and `tga` are decode, and three guesses have now missed them.** Splitting
the stages again with the probe fixed, one measurement each, milliseconds:

| format | probe before -> after | decode before -> after |
| --- | --- | --- |
| `ppm` | 1.2 -> 1.2 | 32.2 -> **38.4** (+19%) |
| `tga` | 1.2 -> 1.4 | 43.0 -> **56.6** (+32%) |
| `bmp` | 1.5 -> 1.1 | 92.4 -> **54.9** (-41%) |
| `hdr` | 1.3 -> 1.2 | 149.4 -> 143.9 |

So the probe is behind every one of them now, and `ppm` and `tga` are slower in
the **decode** by a fifth to a third. `pnm`'s decode was handing `vec![0u8; size]`
to a path that overwrites every byte of it, so the buffer is now allocated by the
arm that needs one and the binary raster takes the payload directly. That is a
real saving and the pixels are identical -- but **it did not move the ratio**
(1.184 against 1.19 before it), so it is not the cause either.

Three attempts at these two formats have now been measured and three were wrong:
per-row allocation, the whole-file probe, and the zeroed output buffer. The
common thread is that each was found by reading the code and reasoning about what
*should* cost something, and each time the thing that does cost something is
somewhere else. The next step for `ppm` and `tga` is a real profile -- a sampling
profiler or instrumented stage timings inside the module -- and not a fourth
reading of the source. Every one of these changes is kept because each is right
on its own terms and leaves the pixels identical, but none of them is a fix and
none should be counted as one.

`tga` is next, and it is the same shape as `hdr` was: one output allocation and
a reusable row, `dds` being the one with the stable ratio (1.30 this round, 1.2
before, so it needs the profile this method just supplied for `hdr`).

The first attempt at this section guessed per-row allocation and flattened
`hdr.rs`'s `Vec<Vec<[u8; 4]>>` into one buffer. That is kept -- it is 900 fewer
allocations on a 900 row picture, the pixels are unchanged, and it cost nothing --
but it did not move the ratio (hdr was 1.33 before it and 1.29 after), which is
what ruled the guess out. A guess that is not measured is how the wrong thing gets
written twice.

What they have in common, from their own code, was the guess above and it is
block where the reader it replaces writes into a buffer it was handed, and
`hdr` and `dds` additionally build an intermediate `Vec` per scanline or per
block. The fix is to write the picture into one buffer allocated once and to
keep the per-row work in that buffer, which is the shape `png.rs` already uses.
That is the next action, and this corpus and this harness are what will say
whether it worked.

### the three documents, checked against the tree

The goal names three files to keep current as slices land, and two of them were
checked by reading them. The third, `AGENTS.md`, was checked by asking it about
the tree instead: every module in `src/formats/` is named in its source layout.

That found one that was not -- `jpeg.rs`, which is step 3 of the *previous* goal
rather than a step-5 slice, and which that goal left out of the layout. It is
named now, with what it does and why: one `zune-jpeg` header pass answers every
fact a probe asks, so creating a clip over a 35 page corpus reads headers instead
of 213 MB of pictures. Left out, a reader of `AGENTS.md` would not know the module
exists, and the layout is the document that says which file owns which format.

The other two were checked the same way and needed nothing beyond what earlier
rounds fixed: `27-direct-still-decoders.md`'s two status tables both name all ten
as landed, and `26-remove-image-rs.md`'s sections above are the record of the
verification itself.

### every frame property, not just the four a survey reads

The step-5 constraint names "every pixel, every frame property and both
`ReadAlpha` clips". The pixels and both clips were checked exhaustively and
`probe-facts.py` reads four properties, but nothing had ever asked a frame for its
*whole* property set -- so a property no survey tool names could have moved
unnoticed. `target/bench/all-props.py` prints every key and value of the colour
frame and of the alpha frame for every fixture; two runs diffed are the check.

**Zero of the 160 files both builds read differ in any property**, colour clip or
alpha clip. The properties that exist at all are `_FieldBased`, `_Matrix`,
`_Primaries`, `_Range`, `_Transfer`, `ImgSeqAlpha`, `ImgSeqHasICC`,
`ImgSeqIndex`, `ImgSeqOrientation`, `ImgSeqOriginalColorType` and `ImgSeqPath` --
and for these formats `_Primaries` and `_Transfer` are unset throughout, because
no step-5 still states a colour, which is worth knowing rather than assuming.

Four files are newly readable and that is the intended widening: the three HDR
orientations the plan asks for (in `CHANGELOG.md`) and `tiff-zstd.tiff`, which is
a new fixture rather than a behaviour change. Their properties are new lines, not
changed ones.

### the licence obligation, checked against the modules

The constraint is that "a ported decoder keeps image-rs's `LICENSE-*` text and a
`THIRD_PARTY_NOTICES` entry naming the files it came from". Asserted when the
notices were written, and checked here by asking the modules rather than the
notices:

| module | what its head says it came from |
| --- | --- |
| `bmp.rs`, `ico.rs`, `tga.rs`, `dds.rs`, `pnm.rs` | **ported**, from `image` |
| `hdr.rs` | written here |
| `tiff.rs`, `exr.rs`, `qoi.rs`, `farbfeld.rs`, `jp2.rs`, `jpeg.rs`, `jxl.rs`, `heif.rs`, `webp.rs` | a crate, called directly |
| `png.rs` | written here -- the `cICP` chunk and a row walk over the `png` crate |
| `avif.rs` | written here -- its own box walker over dav1d |

**Five modules say ported and the notices name those same five**, so the list is
complete rather than a guess: a sixth port would show up as a module whose head
says so and a notice that does not mention it. `LICENSES/image-LICENSE-MIT.txt`
and `image-LICENSE-APACHE.txt` are both present, which is what makes the
`MIT OR Apache-2.0` choice coverable from either.

### the probe never promises what a decode refuses

Also checked directly for the first time, rather than read off each slice by hand.
`target/bench/probe-agreement.py` asks every fixture and the whole step-5 corpus
the way a caller does -- `Read` probes while the clip is built, `get_frame`
decodes what the probe accepted -- and counts the files that get a clip and then
fail on their frame. Those are the violation; a file the probe refuses outright
is not.

| build | decoded | refused up front | promised then broken |
| --- | --- | --- | --- |
| pre-step-5 | 161 | 6 | 2 |
| this tree | **165** | **2** | 2 |

**The same two files promise and then refuse in both builds**, so step 5 did not
introduce one: `avif-no-picture.avif` (the item holds no picture) and
`pnm-ascii-comment.pgm` (a comment in an ASCII raster). Both are **content-level**
defects -- a legal header over a body that is not -- and a probe cannot find them
without decoding, which is what probing is for. The rule is about a probe and a
decoder disagreeing over a file they both own, and for that the count is zero in
both builds.

The other half of the table is the pleasant part: step 5 took the files this tree
refuses from 6 to 2, because three HDR orientations and one other now read where
the crate would not take them.

### the request-order half of the verification

This page asks that baseline and candidate agree "including both `ReadAlpha` clips
**and different request orders**", and for all of step 5 only the first half had
been checked. `target/bench/request-order.py` closes it: over the 80 file step-5
corpus, with `ReadAlpha`, it asks for every frame four ways -- ascending,
descending, a fixed shuffle, and each frame twice -- hashes both the colour and
the alpha clip of every one, and requires every hash to be identical. It runs that
at `prefetch=0` and at the default, because the lookahead window is the part that
makes a different order a different code path.

All four orders agree on all 80 frames, for both clips, at both prefetch settings,
on **three builds**: this tree, the pre-step-5 `nohooks` build, and the build the
whole fixture set was baselined against. So the comparison the page asks for holds
under both conditions rather than only the one that had been run.

The script lives under `target/` with the rest of the benchmark tooling, so it is
not committed; this paragraph is the record of it.

### what step 5 leaves behind

**The migration itself is complete.** All ten formats of the plan are read by
this tree, every one of their `image` features is disabled, and `image`'s
remaining feature list is `jpeg`, `avif-native`, `gif`, `png` and `webp` -- none
of them a step-5 format. What is left on the crate is the animated paths and the
two fallbacks, and removing it is step 6.

Per slice the evidence is the same shape in each: probe facts, colour planes and
both `ReadAlpha` clips byte-identical to the build before it, the fixture corpus
unchanged, 266 tests, clippy and fmt clean, the validator at `all checks passed`
and `frame-parity.py` at 0 of 245.

**`tga` is 1.13x now, down from 1.34x, and it is the second fix that came from
the profile.** The stage timing showed `stored_pixels` copying the whole picture
(0.70 ms) into a buffer that `expand` then copied again (0.65 ms). An
**uncompressed** targa's bytes are already what `expand` reads, so the first copy
is one the picture does not need; the decode now hands the file's own slice to
`expand` whenever nothing has to be moved first. Seven paired repetitions,
median:

| build | median | all seven |
| --- | --- | --- |
| pre-step-5 | **46.0** | 42.5 44.3 44.9 46.0 50.6 52.7 60.4 |
| before | 57.4 | 52.9 55.1 56.6 57.4 57.5 59.0 61.7 |
| after | **51.9** | 46.0 47.0 51.3 51.9 52.2 52.2 59.2 |

5.5 ms, about a tenth, which is what 0.70 ms a file over eight files predicts.

**That contrast is the finding worth keeping.** The fusion removed a *loop* and
paid for it by replacing a memcpy with per-pixel work: no gain, reverted. This
removed a *copy* and left every loop alone: a tenth, kept. Two passes over a
picture only cost something when the pass is a whole extra traversal of memory
that another pass could have done instead -- not when two traversals can be
fused into one slower one.

**`dds`: profiled, two attempts, still open.** The stage timing puts everything in
one place -- on a 1.08 MB file over twenty rounds, `read` 0.41 ms, `header`
0.001 ms, **`blocks` 3.94 ms** -- so unlike `tga` there is no copy to remove and
the block loop is the whole cost.

The inner loop did `index / 4` and `index % 4` for each of the sixteen pixels of
every block, which is 1.08M divisions, so it was rewritten as nested loops that
step the target instead. Seven paired repetitions, median:

| build | median | all seven |
| --- | --- | --- |
| pre-step-5 | **51.5** | 44.4 49.2 49.5 51.5 56.6 57.5 61.7 |
| before | 57.5 | 56.5 56.6 56.8 57.5 58.5 60.5 66.6 |
| nested loops | **66.1** | 63.1 64.3 64.5 66.1 67.4 71.0 73.0 |

It is **worse by 8.6 ms** and was reverted. The division was not the cost, and
the nested form defeated something the compiler was already doing with the flat
one -- an enumerate over a fixed sixteen element array is unrolled well, and
telling it how to walk the output did not help. Pixels were identical either way,
so only the measurement could say this.

That is the fourth attempt at these two formats to be measured and the third to be
wrong, and the split is clean: `hdr` and `tga` were fixed by removing whole
traversals of memory, and every attempt to make an *existing* traversal cheaper
has failed. `dds` has no such traversal to remove -- `blocks` is the picture being
written once, which is the minimum -- so the next attempt should look for work
that is not happening at all rather than for work that could be arranged better.

**Why the method stops at `dds`, from the source rather than another guess.**
`blocks` at 3.94 ms over 67,500 blocks is **58 ns, about 175 cycles, per block**,
and reading it says that is the block *math*, not memory: the only writes are the
4.3 MB of output (0.4 ms at 10 GB/s) and a 64 byte array on the stack, which is
1.08M L1 accesses and rounds to nothing. `dxt5_block` calls
`alpha_levels(source[0], source[1])` and `colour_block(&source[8..16], false)`
once per block, which is required -- the endpoints are in the block, not in the
file -- and there is no per-pixel work that a per-block result would serve
instead. So there is no traversal to remove here, which is exactly why `hdr` and
`tga` were fixable and this is not: **their gap was memory and this one is
arithmetic.**

**The comparison was made, and the answer is not the arithmetic.** `image`'s DXT
decoder is `src/codecs/dxt.rs`, and its `read_image` is

```rust
for chunk in buf.chunks_mut(self.scanline_bytes().max(1) as usize) {
    self.read_scanline(chunk)?;
}
```

-- **it decodes straight into the buffer the caller hands it.** This tree's `dds.rs`
returns a `Vec<u8>` of the whole picture from `blocks()` and hands it back as
`Pixels::Interleaved`, which means the plugin then copies those 4.3 MB into the
frame. That is a whole extra traversal of the picture, which is the same shape as
the two fixes that worked: `hdr`'s three buffers and `tga`'s redundant copy.

So `blocks` at 3.94 ms is not proof that the block arithmetic is the cost -- it is
the cost of *building a buffer*, which the other decoder does not do.

**That the extra copy is real was checked rather than assumed.** `src/still.rs` is
the `image` path, and its reader is

```rust
pub fn read(self, buffer: &mut [u8]) -> Result<(), OpenError>
```

-- **the caller hands it the buffer and it fills that.** So the two paths differ
by exactly one traversal of the picture: `image` writes the frame, and this
tree's `dds.rs` writes a 4.3 MB `Vec` that the plugin then copies into the frame,
which is 4.3 MB of extra write and 4.3 MB of extra read, about 0.86 ms a file at
10 GB/s against a measured gap of 1.15.

It also explains the formats that came out *ahead* despite doing the same thing.
`bmp`, `hdr`, `tga` and `qoi` all build an intermediate too, and all four are
faster than the reader they replaced -- their decode wins more than the copy
costs. `dds`'s decode is comparable to `image`'s, so for this one format the copy
is the whole difference and it shows as a loss. One architecture, five formats,
and the sign of the result depends on how much the decode itself gained.

**The model to copy is `png.rs`.** It is the only module that already answers with
a stream (`src/formats/png.rs:650`), and its `Rows` is: hold the path rather than
the picture, and in `fill` re-open the file, read the header again, check it
against what the probe saw, then loop `reader.next_row()` handing each row to a
`Placer` that writes it into the `RowSink`. Its own comment says why: "There is no
buffer, so the buffer stage is nothing; the read is the decode and the placement
together, because they are one pass."

For `dds` the same shape is: `struct Rows { path, header }`, `has_alpha()` is
`matches!(variant, Dxt3 | Dxt5)` (a DXT1 surface has no alpha plane, which is the
rule this module already keeps), `duplicate()` re-opens, and `fill` walks **block
rows** -- `across` blocks at a time -- decoding each block straight into the four
pixel rows of the sink it belongs to. The block loop is already written that way
in `blocks()`; what changes is where the sixteen pixels go, from a `Vec` to four
row slices. The 4.3 MB intermediate then does not exist and the traversal that
this section is about is gone.

**It was written, measured, and reverted.** The stream was built exactly as
described: `Rows { path, alpha }`, `has_alpha` from the variant, `duplicate`
re-opening, and `fill` walking block rows and decoding each block into a band of
four pixel rows before writing them to the sink's planes. Pixels byte-identical,
266 tests, clippy clean. Then seven paired repetitions of the decode stage,
median:

| build | median | all seven |
| --- | --- | --- |
| pre-step-5 | **52.1** | 49.9 49.9 51.4 52.1 52.6 53.6 62.0 |
| buffered (before) | 63.9 | 59.0 59.4 60.3 63.9 64.4 66.9 69.1 |
| streamed | **67.1** | 64.7 65.3 65.5 67.1 70.7 71.1 76.2 |

**Worse by 3.2 ms**, so it was reverted. The 4.3 MB copy it removed cost less than
the scatter it added: the sink is one entry per plane, so writing an interleaved
band to it is three or four passes over every row, and `png.rs` says as much in
its own `place` -- "walking the row once per plane reads it three times and
multiplies for every byte it writes". The lesson it took a specialised `place` to
learn is the one this attempt ignored.

**The correction is one pass over the row, not three.** The implementation did

```rust
for (plane, channel) in [(0, 0), (1, 1), (2, 2)] {
    let line = target.row(y);
    for (x, pixel) in source.chunks_exact(channels).enumerate() {
        line[x] = pixel[channel];
    }
}
```

-- which reads the band once per plane, three or four times over. `png.rs` does
the opposite and says so: it walks the row **once** and writes all three planes
inside that walk, so every byte of the source is read once and the three plane
rows are held together. Its comment is about the gather it is avoiding; the same
sentence is the instruction this attempt needed and read past.

Holding the three plane rows at once is not the obstacle it looks like: they are
three separate `PlaneRows` in `sink.colour`, so disjoint mutable borrows come from
splitting the `Vec`, and within one plane the four rows of a block row are
disjoint slices of one `bytes` buffer. That is the shape a third attempt should
have, and it is a different claim from the one that failed -- the previous one was
correct that the 4.3 MB copy is worth removing and wrong about what removing it
would cost.

**It was written a second time with that correction, and it still does not pay.**
The one-pass version holds the three plane rows together and reads the band once;
parity is byte-identical and the three-pass cost is gone (67.1 ms down to 62.5).
But seven paired repetitions put it at 62.5 against the buffered 60.7, with the
ranges overlapping -- before 56.5 58.9 60.6 60.7 64.3 64.9 77.9 and after 60.3
61.2 61.5 62.5 62.7 64.1 64.5. That is no better, and it is more code, so it was
reverted too.

**That closes the question.** The extra copy is real, and removing it by streaming
into the frame does not recover the 1.12x: two implementations were built to the
right shape, measured, and neither beat the buffer. `dds` is left as it is, at
1.12x, with the cause understood and the remedy tried rather than assumed.

So the stream is the right *shape* and this was the wrong *implementation* of it:
the transpose has to happen inside the block decode -- each block's four pixels
writing their four bytes per plane as they are decoded, rather than into a band
that is then scattered -- and that is the next attempt rather than this one.

The mechanism to remove it is already in this tree and documented for exactly
this: [`decoder::RowStream`], "a decode that hands each row to the frame it
belongs in and therefore has no buffer of its own". `png.rs` uses it. A dds
`RowStream` decodes a row of blocks per call and writes that row, and the 4.3 MB
intermediate disappears. That is the next step for this format, and it is a
change of the kind that has worked twice rather than a fifth attempt at arranging
the same traversal.

Closing it by making the block arithmetic faster would mean comparing this
`dxt5_block` against the one in `image` line by line -- `image`'s is still in the
vendored source and is the thing being 1.12x faster. That is a real task rather
than another edit, and it is the right next step rather than a fifth guess at the
same shape.

**The third attempt: copy whole lines instead of pixels. Also worse.** `image`'s
`dxt.rs` writes a decoded block with four copies of a whole line
(`dest[offset..offset + 16].copy_from_slice(&decoded_block[line * 16..])`) where
this tree did sixteen copies of four bytes, and that was the one structural
difference left once the arithmetic had been compared line by line -- `widen`,
`from_565`, `colour_block` and `alpha_levels` are already the same algorithm. The
block decoders were made to return flat arrays (48 bytes and 64) so a line is
contiguous, and `blocks` copies twelve or sixteen bytes at a time. Pixels
byte-identical, and **57.7 against 55.3** -- worse again, so it was reverted.
Four bytes at a time was already what the compiler wanted.

**The measurement itself was the thing to fix, and doing so changes the answer.**
Every figure above comes from seven or so repetitions compared as *medians*,
which is not the same as comparing the two builds on the same run: this
machine's throughput drifts enough that the pre-step-5 build measured 51.5,
52.1, 55.4, 60.7 and 52.5 across these rounds while this tree measured 57.5,
63.9, 60.7, 55.3 and 60.7. Interleaving the two builds inside each repetition
and comparing the *paired* ratio is the measurement that settles it:

| | median | min | max |
| --- | --- | --- | --- |
| pre-step-5 | 52.5 | 49.8 | 68.4 |
| this tree | 60.7 | 54.9 | 66.5 |

Fifteen interleaved pairs give a **median ratio of 1.124**, and this tree is
faster in only 3 of the 15. So the regression is real and about 1.12x -- not the
artifact `ppm` turned out to be, and not the 1.23x one unpaired round suggested
either. The interleaved paired design is what the harness should have used from
the start, and its absence is why the same question was answered three different
ways across these rounds.

That is the dds budget spent: four attempts, all measured, all reverted, and the
block arithmetic shown to be equivalent to the reference. Whatever the 12% is, it
is not in the code this page has looked at.
## plan 34, phase 1: routing reproduced

`docs/improvements/34-input-routing-and-planar-decode.md` calls for one content
identification and one saved decoder plan. Before changing dispatch, its problem
was reproduced in this tree from committed fixtures.

**The cause is that `owns()` is extension-only.** Every format module's is the
same shape:

```rust
pub fn owns(path: &Path) -> bool {
    path.extension()...is_some_and(|e| EXTENSIONS.iter().any(|k| e.eq_ignore_ascii_case(k)))
}
```

`format_decoder` checks `handles(info)` for avif, heif, webp, jxl and jp2 -- those
read content -- and then `owns(&info.path)` for everything else. So ten of the
fifteen routers pick a backend from the filename alone, and a correct file under a
wrong name is handed to a decoder that cannot read it.

**The plan's own research set cannot be used to show this.**
`target/research-routing-p72melmd/` holds 86 files and every one of them is 85
bytes of bare signature -- the renamed copies were kept and the files they were
made from were not, so there is nothing to compare a copy against.

`target/bench/routing.py` makes its own copies from `tests/fixtures/` instead, one
source per distinct extension, each copied under `.bmp`, `.jpg`, `.dat` and its
own extension uppercased. A copy must decode to the same bytes, the same alpha and
the same properties as its source. `ImgSeqPath` is excluded, because a renamed
file's path differs by construction and is not a routing fact -- including it made
the first run report 51 phantom property failures.

### the EXR probe and decode pick different layers, and why

Item (2) is diagnosed rather than fixed, because the repair is a design step and
guessing at it would be the mistake this whole exercise keeps repeating.

**They ask different questions of the same file.**

```rust
// the probe, exr.rs:178
let Some(layer) = metadata.headers.iter().find(|header| flat_rgb(header)) else { ... };

// the decode
read_first_flat_layer_from_file(path)
```

`read_first_flat_layer_from_file` is `read().no_deep_data().largest_resolution_level()
.all_channels().first_valid_layer().all_attributes().from_file(path)`, so it takes
the first layer that is merely *flat*. The probe takes the first that is flat **and
carries R, G and B**. A file whose first part is Z-only and whose second is r,g,b
therefore has the probe describe the second and the decode read the first, and the
decode then fails on the missing `R` channel -- which is exactly the plan's
reproduction, now explained rather than only observed. It is the same shape as the
`jxl`/`jp2` bug: two halves of one reader answering one question two ways.

**What the fix needs is not more identification.** Both halves agree on the
backend; they disagree about which layer *inside* it. The plan calls for a saved
decoder plan, and this is the case that shows why: the probe's choice has to be
recorded and reused rather than made a second time. The crate offers the prelude
reader and a `read()` builder ending in `.first_valid_layer()`, and no
layer-selecting accessor turned up in a search of its public functions, so the
shape of the repair is not yet settled -- whether the probe should instead report
the first flat layer and let the frame be grey, or the decode should walk the
builder's layers and take the same one the probe took. That is the decision to
make before writing code, and the plan's `## architecture` section is where to
settle it.

Reproducing it needs a two-part EXR, which nothing in `tests/fixtures` is: every
EXR there has one part. Writing one is the first step of the slice.
### the tiff adapter was throwing its own metadata away

Plan 34 records a TIFF stating orientation 6 and carrying an ICC profile coming out
as "orientation 1, no ICC", and calls the two independently repairable. They were
one cause: `image_info` built its `ImageInfo` with both hardcoded away.

```rust
has_icc_profile: false,
icc_profile: None,
orientation: Orientation::NoTransforms,
transform: Transform::IDENTITY,
```

Both tags are in the directory the crate already reads for the dimensions, so
neither costs a pass. The orientation goes through `Orientation::from_exif`, which
is the same 1-to-8 code an exif tag uses, and the profile comes back from tag
34675 -- as a `List` of single bytes, because the crate's `Value::Byte` holds one
byte and a byte array is a list of them.

**A third bug was sitting in the same function.** The gate below it accepted only
the classic version word:

```rust
let little = signature == [0x49, 0x49, 0x2a, 0x00];
let big = signature == [0x4d, 0x4d, 0x00, 0x2a];
```

So a BigTIFF was declined here however `identify` had routed it -- the plan's "local
signature gate blocks it" line, and reachable only because the crate does decode
BigTIFF. It now asks `identify` instead. That is the third time in this work that a
module keeping its own copy of a table produced a disagreement, and the fix is the
same each time: one table.

`tests/fixtures/tiff-orient6-icc.tiff` is the reproduction -- a 2x3 picture that
states orientation 6 and embeds the 588 byte profile the ICC fixtures already use
-- and `the_orientation_and_the_profile_are_read_from_the_directory` pins it. The
test asserts the *stored* size and a non-identity transform rather than the 3x2 a
frame comes out as, because the probe reports what the file stores and the writer
is what applies the turn; getting that backwards is what the first version of the
test did.

Verified: no existing frame differs, the validator is at `all checks passed`, the
routing check is still 0 of 76, and all four request orders agree. In
`CHANGELOG.md` because orientation and ICC are both visible.
### the silent one: a word-wide sample with no tuple type

Plan 34 calls this the silent case, and it earned the name. A `P7` that states a
`MAXVAL` above a byte but names no `TUPLTYPE` was read one byte a sample, because
`arbitrary_tuple`'s empty-tuple arm chose from the depth alone -- its comment said
so, "and only at a byte". The specification says a sample occupies one or two bytes
according to `MAXVAL` whether or not the tuple is named.

Nothing failed. A raster of `[1023, 512]` under a `MAXVAL` of 1023 came back as
`[1, 64]`: the right number of samples, the wrong picture, no error. `[3, 255]`
read as two bytes and rescaled as eight gives exactly those two numbers, which is
how the cause was pinned rather than guessed.

The fix is the arm reading `(depth, wide)` instead of `depth` alone. The same
raster now comes back `Gray16` with `[65535, 32800]`, which is `1023` and `512`
against a `MAXVAL` of 1023 at the sixteen bit container the plugin hands out.
`tests/fixtures/pnm-p7-notuple16.pam` is the reproduction and
`a_word_wide_sample_is_a_word_even_without_a_tuple_type` pins it; it is in
`CHANGELOG.md` because it is a picture a user can see change.

Verified: no existing frame differs, the validator is at `all checks passed`, the
routing check is still 0 of 76, and all four request orders agree.
### the routing half of phase 1 is green

`heif` was the last module answering from its name, and it was at least honest
about it: `image_info` and `handles` both called `has_heif_extension`, so the probe
and the decode agreed with each other and both ignored the container. The brand in
the `ftyp` box separates a heif from an avif and `identify` already read it, so
`heif` gained an `owns` and the two call sites moved to it.

**Renamed copies read wrong: 0 of 76.** `tests/routing.py` is green for the first
time, and the whole run is unchanged where it must be: no frame differs from the
recorded baseline, the validator is at `all checks passed`, all four request orders
agree, 272 tests pass, clippy and fmt are clean.

So the dispatch half of phase 1 is done. What phase 1 still lists and this does not
cover is the *other* half of "one saved decoder plan": two files can route
correctly to the right backend and still have the probe and the decode pick
different things inside it -- an EXR part, a TIFF directory, a PAM sample type.
Those are disagreements about content rather than about ownership, they are
invisible to a check built on extensions, and they are where the next slice goes.
The PAM case is the worst of them, because it is silent: `MAXVAL` 1023 with depth 1
and no `TUPLTYPE` is accepted as gray8 and returns the wrong samples rather than
refusing.
### probe and decode were answering from different places

The 12 that survived the content rule were three formats content cannot name --
`jp2`/`j2k`, `heic` and `jxl` -- and the first two turned out to be a bug of their
own rather than a missing signature.

**`jxl` and `jp2` disagreed with themselves.** `describe` routed them through
`owns`, which had just been made content-first, but `format_decoder` routed them
through `handles`, and `handles` answered from its own extension helper:

```rust
pub fn handles(info: &ImageInfo) -> bool {
    has_jxl_extension(&info.path)
}
```

So a renamed jxl **passed the probe and fell through to `image` at decode, which
refused it on the extension** -- exactly what the plan forbids, "a probe must never
promise a frame a decode would refuse to produce", live in the tree and invisible
while both halves agreed. The two now call `owns`, and the extension helpers and
their constants are gone; the tests that named them ask `owns` instead, because
that is what ships.

**A separate constant is a separate answer**, which is why the halves could drift
at all: each module kept its own extension list. `identify` is now the only one,
so the drift is unrepresentable rather than merely repaired.

**Renamed copies read wrong: 3 of 76, all `heic`.** Everything else in the tree
routes by its bytes. No frame differs from the recorded baseline, the validator is
at `all checks passed`, and all four request orders agree.
### the routing fix: `owns` becomes content-first

The content check was already inside every module's `image_info`, and its own
comment said so -- "the extension is only a hint". `owns` was what stood in front
of it and returned early. So the fix is one function per module, delegating to a
new `identify::owns(format, path, head)` that applies the plan's rule in one
place: a signature decides, and a format with none of its own falls back on the
name. No caller and no signature moved, and the unit tests inside each module pass
paths that do not exist, so they read as no content and answer from the extension
exactly as before.

**Renamed copies read wrong: 59 of 84, now 15 of 80.**

Two formats had to be taken back out of the content rule, and both are real
findings rather than exceptions granted for convenience.

**Targa has no leading signature**, so it was never eligible -- the plan says its
extension "is not a hint but the whole answer". But the first pass left it in the
content rule and broke it in the other direction: a Targa type 2 header begins
`00 00 02 00`, which is CUR's magic, so identifying content handed every such
Targa to the icon reader and 4 tests plus every Targa fixture failed with "the
icon holds no entries". The pixel diff caught it, not the routing check.

**The icon family is the same collision from the other side.** `\0\0\1\0` and
`\0\0\2\0` are an icon or cursor *and* a Targa type 1 or 2 header, and those four
bytes are the whole of the leading signature there is. So `Format::has_signature`
is false for `Tga` and `Ico`, `identify` declines to claim either, and both decide
by name. The plan asks to "extend the table for BigTIFF, CUR and RGBE": BigTIFF and
RGBE are extended and working, and CUR is the one that cannot be, for the reason
above.

The 15 that remain are three formats content cannot currently name -- `jp2`/`j2k`,
`heic` and `jxl`, each renamed to `.bmp`, `.jpg` and `.dat` -- and they are the
next slice. Everything else is routed by its bytes.

Verified unchanged: no frame differs from the recorded baseline, the validator is
at `all checks passed`, and all four request orders agree.
### the routing check, committed

`tests/routing.py` is plan 34 phase 1's acceptance check, moved out of the
scratch tooling so that the criterion is a committed artifact rather than a script
under `target/`. It makes its own copies from `tests/fixtures/` -- one source per
distinct extension, each under `.bmp`, `.jpg`, `.dat` and its own uppercased
extension -- and requires every copy to decode to the same bytes, the same alpha
and the same properties as its source. `ImgSeqPath` is excluded, because a renamed
file's path differs by construction and is not a routing fact.

It reports **59 of 84 wrong** and is expected to reach 0 when `describe` and
`format_decoder` consult `src/formats/identify.rs` instead of the extension. It
exits non-zero on a failure and aborts if the plugin cannot be loaded, so it
cannot pass by measuring nothing -- which is the mistake its first version made
when it was pointed at the plan's research set and skipped every group whose
original was missing.

The wiring is wider than it looks and is the next slice. `describe` and
`format_decoder` both gate through each module's `owns(path)`, and that gate is
called *inside* `image_info` and `decode`, so the identification has to reach them
through either a new parameter on fifteen functions or a read inside `owns`
itself. The plan's phase 3 asks for one open per operation, so the parameter is
the shape that will survive; doing it as a read inside `owns` would be correct but
would open the file once per format check.
### the identification table, landed before its wiring

`src/formats/identify.rs` is the strong half of the plan's rule. It reads sixteen
bytes and answers which format they are, or `None` when they say nothing --
`identify` -- beside `from_extension` as the weak half, kept separate so a caller
can tell which answered. The plan's table is extended where it asked: BigTIFF
(`II+\0` and `MM\0+`), CUR (`\0\0\2\0`), and both Radiance spellings, so the
`#?RGBE` form the plan flags as compared by the wrong prefix length is named.

Its test is the one that matters: **every fixture whose format has a signature is
identified as that format from its bytes alone**, asserted rather than compared,
across more than a hundred files. The one contradiction that would fail it is
exactly the class of bug phase 1 is about. `Tga` and `Bmp` are the two it cannot
answer for, and the test records that rather than skipping them silently: Targa
has no leading magic, and a bare DIB is the case the plan gives "a distinct
extension-assisted structural probe".

Two layering choices worth reviewing, because both are where a mistake would
hide. `P6` with nothing after it **is** identified as netpbm -- the magic is
there, and refusing a file that stops there belongs to the header check the plan
asks for, not to identification. And an `ftyp` brand neither avif nor heif claims
answers `None` rather than being guessed at.

The module carries a module-level `#![allow(dead_code)]` with a comment saying
why: it lands one slice ahead of the routing change that reads it, which is what
"do not combine all phases into an unreviewable backend rewrite" asks for. The
allow is to be removed in that slice. This is the one deliberate piece of unused
code in the tree and it should not outlive the next commit.
**The baseline is 59 of 84 copies wrong**, and they separate by suffix:

| suffix | failures | what happens |
| --- | --- | --- |
| `.jpg` | 21 | the jpeg adapter is chosen by extension and refuses the bytes |
| `.bmp` | 20 | the bitmap adapter is chosen by extension and refuses them |
| `.dat` | 17 | nothing claims it, so `image` guesses from the extension and refuses |

Every uppercase case passes, and no property differs, so the check is measuring
routing and nothing else. The fix is the plan's: identify content once, save the
route, and let both the probe and the decode read the saved one.
### bmp, and the reversal that is not a transpose

A bitmap's rows are the frame's rows, only possibly in the other order: stored
top-down they are already right, and stored bottom-up they are the same rows
reversed, which is a choice of target row rather than a transpose. The stream
covers every uncompressed form without alpha and hands each row to
`RowSink::place_rgb8` -- but unlike `tga` it does not reimplement the row, it
reuses `unpack_row` into one scratch row that is reused for every row, so the
palette lookup, the bit expansion and the blue-first exchange all still happen in
the one place that already did them. That is what keeps it a saving rather than a
second decoder.

Fifteen interleaved pairs:

| | median | min | max |
| --- | --- | --- | --- |
| buffered | 51.7 | 49.4 | 66.5 |
| streamed | **43.8** | 42.3 | 64.2 |

**A paired ratio median of 0.859 -- 14% off -- faster in 14 of 15.** Against the
pre-step-5 build it is **0.514**: this reader is now about twice as fast as the one
it replaced, where the buffered version was already well ahead. The run-length and
four channel forms keep the buffered path; a run's packets cross rows, and a four
channel row needs an alpha placer this does not have.

Every bmp and ico fixture is byte-identical -- ico reads its DIB payloads through
this same module -- the validator is at `all checks passed`, no file that both
builds read differs in any property, and all four request orders agree.
### tga, and the regression it was carrying

The same shape works far better here, because `tga`'s stored row is the frame's
row with two bytes exchanged and nothing else. The stream takes an uncompressed,
top-to-bottom, left-to-right, three-bytes-a-pixel image -- the corpus file -- and
hands each of the file's own rows to `RowSink::place_bgr8`, a placer that is
`place_rgb8` with `pixel[2]` written first. Targa stores blue first, so the swap
is required; folding it into the placement walk is what keeps this a saving rather
than a trade.

Fifteen interleaved pairs:

| | median | min | max |
| --- | --- | --- | --- |
| buffered | 51.0 | 45.6 | 77.7 |
| streamed | **30.7** | 28.4 | 35.3 |

**A paired ratio median of 0.602 -- 40% off -- faster in 15 of 15.** Against the
pre-step-5 build it is **0.761**, so this format's 1.13x regression is not closed
but inverted: it is now 24% faster than the reader it replaced. That was the last
open regression from step 5.

Everything else unchanged: every tga fixture byte-identical, `all checks passed`,
the same frame properties, all four request orders agreeing.
### the stream that paid: ppm

The row placer gets its first result, and it is the result the dds attempts were
looking for. `pnm.rs` now answers `Pixels::Stream` for the one netpbm shape that
needs nothing between the file and the frame -- a packed `P6` at `MAXVAL` 255,
whose rows are `width * 3` bytes of the file and nothing else, so each is a slice
handed to `RowSink::place_rgb8`. Every other form (ASCII, a word a sample, a
`MAXVAL` that rescales, `P4`, `P5`, `P7`) keeps the buffered path, which is the
rule that a stream is only answered with when it can fill every frame of a call.

Fifteen interleaved pairs on the `.ppm` corpus, against the buffered build:

| | median | min | max |
| --- | --- | --- | --- |
| buffered | 38.2 | 36.9 | 43.6 |
| streamed | **30.8** | 29.4 | 62.8 |

**A paired ratio median of 0.789 -- 21% off -- faster in 14 of the 15.** Against
the pre-step-5 build it is **0.921**: the reader this replaced is now itself
beaten by 8%, where the buffered version only matched it.

Everything else is unchanged: every pnm fixture byte-identical, `all checks
passed`, 0 of the 160 files that both builds read differ in any frame property,
and all four request orders agree at both prefetch settings.

**Why this one worked and the dds streams did not** is the whole finding, and it
is two things rather than one. `place_rgb8` walks the planes with a `zip`, so
there is no index and no bound check per byte -- the dds streams indexed a plane
per pixel with `get_mut`. And a `P6` row is already the layout the frame wants,
where a dds block is sixteen pixels that must be transposed out of a block. So
streaming pays when the decoder's own output order matches the frame's, and the
saving is a whole traversal; it loses when the stream has to transpose, because
then it trades a bulk copy for per-pixel work. That is the same split the earlier
attempts found from the other side, now with a case that lands on the winning
side.

The next formats to try are the ones whose rows are already the frame's: `tga`'s
uncompressed rows and `bmp`'s bottom-up rows. `qoi` decodes to a buffer by
construction and `hdr` transposes for three of its eight orientations, so both
would need their own argument.
### the row placer, lifted out of png.rs

The dds attempts failed for a reason that reading `png.rs` explains. Its
`place_rgb` takes the three plane rows apart with `split_first_mut`, holds them
together, and walks them with

```rust
let planes = red.row(row).iter_mut()
    .zip(green.row(row).iter_mut())
    .zip(blue.row(row).iter_mut());
for (pixel, ((red_byte, green_byte), blue_byte)) in
    data.as_chunks::<3>().0.iter().zip(planes)
```

-- a `zip` over the source pixels and the three rows at once, so there is no index
and no bound check per byte. The dds stream indexed a plane per pixel with
`get_mut`, which is the same walk carrying a check per byte, and that is what made
it lose to the copy it removed.

That technique is now `RowSink::place_rgb8` in `src/decoder.rs`, and `png.rs`
calls it rather than keeping its own copy. Both the extraction and the delegate
are behaviour-preserving by construction -- the body did not change, it moved --
so this is the one change here that did not need a measurement to justify: every
frame is byte-identical across the fixture set and the validator is at `all checks
passed`.

**What is not established is a measured win from it.** `target/bench/step5-corpus`
holds `bmp`, `dds`, `exr`, `ff`, `hdr`, `ico`, `ppm`, `qoi`, `tga` and `tiff` and
no png, so the extraction cannot be timed where it came from, and the half of the
work that would give it something new to speed up -- answering `Pixels::Stream`
from a format that currently buffers the whole picture -- is where the budget ran
out. The first target is `ppm`, whose binary `P6` rows are exactly `width * 3`
bytes in the file and therefore exactly what `place_rgb8` takes, and `tga`'s
uncompressed rows are the same shape. Until one of those is streamed and measured,
the honest statement is that the facility exists and the win does not.
**`dds` remains the one open regression**, at 1.12x where `tga` was 1.34x and is
now 1.13x.

| format | before | after | state |
| --- | --- | --- | --- |
| `tga` | 43.6 ms | 58.3 ms | profiled; five attempts; no fix found |
| `dds` | 39.1 ms | 50.9 ms | improved to 1.135 by hoisting the variant match; needs a wider corpus |

`ppm`, which read as a third, was cleared: its 1.19x was this harness comparing
best-of runs between a high-variance baseline and a low-variance build, and by
median it is faster.

So step 5's objective is met and the performance constraint that goes with it is
**not** fully met. That distinction is the point of this section rather than a
footnote: the format work is done and verified, and two formats are slower than
the readers they replaced with a profile and no remedy. Step 6 should wait.
Step 6, removing the crate, still waits for all of these plus plan 28's
fallback cases.

## the order of work

1. Capture the current format/subtype coverage and baseline behavior, including
   error paths and accepted extension aliases. Record which files use the direct,
   row-stream, animation or image-rs fallback route. Add missing fixtures in a
   future implementation task, before taking their old decoder away.
2. Introduce the small shared representations in [29](29-decoder-types-without-image.md).
   Keep temporary image-rs conversions at its remaining adapter boundary. This
   stage should preserve samples, properties and performance.
3. Move JPEG and the remaining PNG probe/fallback work onto the direct libraries
   in [27](27-direct-still-decoders.md). Retain the working PNG row path from
   [22](22-png-decode-path.md). Benchmark one migration at a time.
4. Replace GIF/WebP discovery and replay, and AVIF/HEIF fallback paths, under
   [28](28-animation-container-decoders.md). Existing direct JXL/JP2 and
   APNG/AVIF/HEIF sequence adapters keep their behavior while losing shared
   image-rs types.
5. Complete TIFF, EXR, QOI, BMP, ICO, DDS, farbfeld, HDR, PNM and TGA under [27](27-direct-still-decoders.md).
   Common-format speed work does not waive coverage of a less common format.
   Prioritize the individual zune codecs for BMP, QOI, PNM, HDR and farbfeld,
   verifying their header APIs and compatibility gaps. TGA remains a manual
   reader. Every manual candidate must compete with image-rs and compatible
   direct crates before acceptance; libpng is excluded from this research scope.
6. Remove image-rs and the libheif image integration only when every row below
   has passed its replacement checks. Rebuild and validate every supported wheel
   platform and CPU variant, then compare the completed plugin with the original
   image-rs baseline.

Each stage is independently reviewable and reversible. While migration is in
progress, a fallback must not silently turn a recognized malformed file into a
different decoder's interpretation. Probe and decode must select compatible
routes and continue to detect a file changing after probing.

## the route audit step 1 asked for

`src/format.rs` is now the inventory, and it is complete rather than a
signature table with an `Other` bucket: every container this plugin can be
handed has a variant, including the three the crate had no entry for at all
(jpeg xl, jpeg 2000) and the four heif spellings. The table below is what the
code says the route is, and it is the same list the old build took.

| route | claimed by | formats |
| --- | --- | --- |
| container probe, no decode | `formats::heif::image_info`, `formats::avif::image_info` | heif, heic, hif, avif |
| native module, extension-selected | `formats::jxl`, `formats::jp2` | jxl, jp2, j2k, j2c, jpx |
| native module, probe-selected | `formats::avif`, `formats::heif`, `formats::webp` | avif, heif, heic, webp |
| row stream | `formats::png::stream` | png, apng (still, non-indexed and indexed) |
| animation adapter | `animation::{apng, frames, heif, jxl}` | png, gif, webp, jxl, avif, heic |
| generic still decode | `src/still.rs` | png, jpeg, gif, webp, tiff, dds, bmp, ico, hdr, exr, qoi, pnm, farbfeld, tga, avif, heif |

The extension aliases the old table accepted are the ones `Format::of_path`
accepts, and the four entries that table did not have (`jfif` was already
there; `dib`, `cur`, `heics`, `hif`, `j2k`, `j2c`, `jpx`, `jpf`, `jpc` are the
additions) only widen which files are recognized, never which decoder a
recognized file reaches.

The content side is where the audit found something worth writing down. The
crate's own signature table matches `ftyp` with its masked `avif` entry for
*any* ISO base media file, so a `hevx` heic was read as the crate's avif and
handed to the libheif hooks. This plugin's table reads the actual brand, which
is more accurate, and `src/still.rs` therefore keeps the crate's own order
(extension seed, hook guess, then the crate's sniffing) rather than steering it
with the more accurate answer: the decoder that succeeds is the authority, and
the plugin's identification is used for diagnostics and metadata. The comment
on `still::decode_reader` is the record of that.

Every line below is now answered by a decision rather than by an open question:
[27](27-direct-still-decoders.md) for the still formats' codecs,
[28](28-animation-container-decoders.md) for the animated ones and the
container fallbacks, and [29](29-decoder-types-without-image.md) for the shared
types. The boxes stay unchecked because none of it is implemented.

## removal checklist

- [ ] Every enabled image-rs format in `Cargo.toml` has a replacement for its
  currently accepted subtypes: AVIF, BMP, DDS, EXR, farbfeld, GIF, ICO, JPEG,
  PNG, PNM, QOI, TGA, TIFF, WebP and HDR. HEIF/HEIC, JXL and JP2 remain supported.
- [ ] Interlaced PNG and APNG poster/one-presentation cases, static GIF/WebP,
  animated GIF/WebP, monochrome AVIF, RGB HEIF, and AVIF containers refused by
  `Meta::native_eligible` no longer require image-rs.
- [ ] Frames preserve the existing output format, integer depth, float values,
  alpha behavior, orientation, timeline and frame properties. Palette content
  that happens to look gray is still handed out in its existing format.
- [ ] TIFF and ICO selection remain one still picture per input. Their pages
  and alternatives do not become a new animation API.
- [ ] Production and test code no longer import image-rs. The lossless WebP
  encoder in `src/formats/webp.rs` tests also has a replacement.
- [ ] `libheif-rs` loses its `image` feature and decoder-hook registrations.
  `cargo tree --locked --offline -i image` currently shows both the root crate
  and `libheif-rs` as parents, so deleting the root dependency alone is insufficient.
- [ ] Check normal, build and dev dependency graphs, supported targets and
  release features for transitive reintroduction of `image`. Reconcile the lock
  file with the actual graph. Packages merely present in `Cargo.lock`, such as
  `ravif` in this checkout, are not proof that a release links them.
- [ ] New native inputs have exact license texts, notices, source/relinking
  obligations and working Windows/Linux/macOS packaging. Preserve the disabled
  libheif defaults in `vcpkg.json` and do not pull in x265 incidentally.
- [ ] The performance gate below and the release validator pass. A known
  baseline failure is documented and resolved separately, not called a pass.

## required baseline and performance gate

Follow [BENCH.md](../BENCH.md) before and after **each implementation slice**,
and compare the final result to the original image-rs build as well as to its
immediate predecessor. Save the baseline library, its SHA-256, commit, dependency
versions, build flags, host CPU, selected CPU variant and corpus hashes. Load
each library explicitly in a fresh process with plugin autoload disabled.

Measure clip creation, first-frame latency, complete sequence wall time,
process CPU time, peak RSS/private memory and decoded/intermediate buffer bytes.
For animations also measure metadata-only open, time per presentation,
forward/reverse/shuffled playback beyond the cache window, and decode counts.
Probe pixel-decode counts should be zero on paths that can discover their
metadata without rendering. Count alpha decoding separately from color and
distinguish cache misses and backward replay from gratuitous repeated work.

Use `Read` and both outputs of `ReadAlpha`, ICC export on/off, rotation on/off,
`mismatch=True` on mixed corpora, and `prefetch=0`, default and 16 with the same
memory budget. Include multiple concurrent clips and small, large, gray, RGB,
YUV, palette, alpha, deep and float pictures. Cover every migrated format with a
correctness corpus even when no large performance corpus exists yet.

Alternate baseline/candidate order, use at least three passes and report all
passes and their spread. Separate warm filesystem-cache runs from controlled
cold-cache runs. Recreate clips so VapourSynth's frame cache cannot replace
decoding with cached frames. Compare identical planes and properties; [25](25-decode-column-parity.md)
explains why a smaller output or a renamed timing column is not a faster decode.
PNG's streamed `read` already includes frame placement, so end-to-end totals are
the common comparison when stage boundaries move.

Claim an improvement only when it exceeds repeat-run variation on the relevant
workload. Investigate and fix any significant speed or memory regression before
landing a slice. Removing a type dependency can be performance-neutral, but
that must be reported as neutrality, not as a speedup. No removal approval is
implied by this research document.

## research-session baseline

The existing validator was attempted before editing these plans, followed by
`cargo build --release --locked` with the repository's local vcpkg paths and
another release-validator attempt. The build passed. Both usable validator runs
stopped at `tests/fixtures/animation.avif` with
`PluginLoadingError(NoMatchingDecoderInstalled)` from libheif. Earlier checks
printed no `FAIL` lines, but the validator did not finish. The sandbox's first
attempt could not import `_ctypes`; the two runs above were outside the sandbox.

The current release DLL has SHA-256
`E10ED35EE38828CF387EB97EF1039E3B5FC9CD54981FF51C32762ABCD91CEFDD`.
The existing harness was run on the Windows research host, explicitly loading
that DLL, with `--imgseqs-only --reps 3 --extra --prefetch 16`. Each corpus has
35 files and uses `snek - p%03d.png` or `snek - p%03d.jpg`:

| corpus | mode | frame totals across three passes (s) | best open + frames (s) |
| --- | --- | --- | ---: |
| `sandbox/png` | default | 0.302, 0.359, 0.467 | 0.308 |
| `sandbox/png` | `prefetch=0` | 1.002, 1.093, 0.880 | 0.885 |
| `sandbox/png` | `prefetch=16` | 0.367, 0.328, 0.356 | 0.338 |
| `sandbox/jpeg` | default | 3.569, 3.202, 3.295 | 3.341 |
| `sandbox/jpeg` | `prefetch=0` | 8.929, 8.821, 8.304 | 8.413 |
| `sandbox/jpeg` | `prefetch=16` | 2.002, 2.199, 1.903 | 2.038 |

Logs are under ignored `target/image-rs-research-{baseline-png,baseline-jpeg}.log`
and `target/image-rs-research-validator-{before,release}.log`. These are current
baseline samples, not replacement-decoder results or a full memory/format
benchmark. There is no candidate implementation to compare, and the spread
already requires care before claiming a small improvement. Repeat and extend
the baseline at the start of future implementation work.

After writing the plans, the release build passed again and the DLL's SHA-256
was unchanged. The validator stopped at the same `animation.avif` decoder error.
The same benchmark commands were repeated:

| corpus | mode | frame totals after documentation edits (s) | best after / best before |
| --- | --- | --- | ---: |
| `sandbox/png` | default | 0.332, 0.380, 0.403 | 1.099 |
| `sandbox/png` | `prefetch=0` | 0.924, 1.031, 0.924 | 1.050 |
| `sandbox/png` | `prefetch=16` | 0.365, 0.320, 0.374 | 0.976 |
| `sandbox/jpeg` | default | 2.845, 2.606, 2.732 | 0.814 |
| `sandbox/jpeg` | `prefetch=0` | 7.907, 7.592, 8.367 | 0.914 |
| `sandbox/jpeg` | `prefetch=16` | 1.785, 2.145, 1.939 | 0.938 |

The after logs are `target/image-rs-research-after-{png,jpeg}.log` and
`target/image-rs-research-validator-after.log`. These separate batches ran the
identical binary, so their differences establish measurement variability, not
an implementation improvement or regression. No CPU or peak-memory comparison
was made in this documentation session. Future decoder experiments need the
paired, alternating baseline protocol above rather than attributing such batch
differences to a source change.

The subsequent Windows AVIF sequence fix is recorded in
[30](30-windows-avif-sequence-decoder.md). It enables libheif's existing dav1d
backend and lets the full release validator finish. The failed runs above
remain the historical research baseline, not the state of the repaired build.
Future image-rs migration experiments must take a fresh baseline from that
working build before comparing performance.

## source evidence

The local Cargo registry's exact `image-0.25.10` source was inspected, especially
`src/codecs/{jpeg/decoder.rs,avif/decoder.rs,tiff.rs,openexr.rs}` and its feature
table. The versioned [image source](https://docs.rs/crate/image/0.25.10/source/)
and [libheif-rs source](https://docs.rs/crate/libheif-rs/3.0.0/source/) are the
upstream references. The supporting plans link the candidate codec APIs.
