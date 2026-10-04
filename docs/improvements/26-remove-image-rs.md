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
- **tiff and exr** — fixtures **landed** in `2205e87`, the modules still to do.
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
