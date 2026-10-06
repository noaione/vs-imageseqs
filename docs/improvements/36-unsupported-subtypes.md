# unsupported subtypes, and the route to each one

Status: **all ten routes are implemented**, and a page this reader refused afterwards
-- a ycbcr TIFF whose strip is a JPEG -- is implemented too, in its own section below.
Written 2026-10-06 against the tree
plan 34 left, and worked through in the order at the end of this page: the TGA two
byte map entries, the bare DIB, cursors, the gray+alpha TIFF, the flat gray EXR,
twelve bit subsampled AVIF/HEIF, a heic storing av1, the ISO composition boxes, the
JP2 channel definitions and TIFF WebP are in the tree with their fixtures and their
checks -- the heic storing av1 as a fixture alone, because the path already worked,
the fragment box as a recorded non-issue, because no decoder here reads one -- and the
palette of a JP2, which was a refusal until OpenJPEG's own box layout was read, is
expanded here too. What stays refused by decision is the section below.

[34](34-input-routing-and-planar-decode.md)'s phase-4 row and the plan-34
candidate table are where these items were listed as "each need an individual
decision and evidence". This page is that decision written down: for the refused
subtypes it takes up, what the container actually states, where the
change lands, what file would prove it and what check would accept it.

Two sets are deliberately outside this page. The **`not planned`** entries at the
end of [the index](README.md) are decisions against (ffmpeg, a dav1d thread
budget, upstream WebP threading, `AUTO_MAX_WORKERS` above four, the python
helper, gpu output, manual SIMD for the rgb deinterleave). The items plan 34
itself defers as needing "new interpretation or codec policy" rather than an I/O
refactor -- camera RAW, gain maps, deep EXR, arbitrary multiband, HDR XYZE
conversion -- stay deferred, and the last section is the one refusal this page
keeps as a decision rather than a route.

The rule every item here has to satisfy is the index's rule for correctness work:
a reproducible case or a clear code-path defect first. Every item starts from the
second -- a named refusal in the code -- and needs the first, which is why the
fixture is the first step of a route rather than a detail of it; the five that
have landed each have one. No item is
accepted on a picture that "looks right": each names the reference decoder or the
fixture pair its acceptance compares against, which is what the low-bit gray TIFF
slice just did with Pillow.

| subtype | refused by | route | first step |
| --- | --- | --- | --- |
| TGA colour map of 15/16-bit entries | `tga.rs`'s entry-size check | widen with the shared table, attribute bit is alpha | a hand-written fixture pair |
| bare DIB | the router's signature rule | a strict structural probe for `.dib` | the DIB payload of an existing icon |
| CUR | `ico.rs` takes no cursor | accept type 2, alpha from the AND mask | an icon's twin as a cursor |
| float gray+alpha TIFF | the layout catch-all | a directory-built layout, `La32F` label | a hand-written 2-sample float page |
| TIFF WebP compression | the crate's own decoder | read the strip here and hand it to libwebp | a `tiffcp -c webp` file |
| JP2 gray+alpha and palettes | the component-count rule | read `cdef`, then `pclr` | a hand-written `cdef` box |
| 12-bit subsampled AVIF/HEIF | `yuv_format`'s table | extend the table once the samples are checked | a 12-bit 4:2:0 encode |
| a heic that stores av1 | nothing, probably | make the file and find out | the same, from an avif's item |
| ISO composition offsets, edit lists, fragments | `sequence.rs` reads `stts` alone | `ctts`, then `elst`, then `moof` | one hand-written box at a time |
| flat grayscale Y/Y+A EXR | `exr.rs` selects R,G,B | select `Y` by name the same way | an extension of the EXR writer |

## TGA colour map entries of fifteen and sixteen bits

**Today.** `src/formats/tga.rs` parses a colour map only when
`map_entry_bits.div_ceil(8)` is three or four, so a file whose map entries are
fifteen or sixteen bits stops at "a colour map entry of 15 bits is not
supported" ([`tga.rs:223`](../../src/formats/tga.rs)). A *direct* fifteen and
sixteen bit image is already read: it goes through the round-to-nearest widening
table the bitmap reader shares ([`bmp::expand`](../../src/formats/bmp.rs)), which
is the same arithmetic a map entry needs.

**The format says.** A TGA colour map entry is one, two, three or four bytes.
The two byte spelling is five bits each of blue, green and red with one
attribute bit, and the specification puts that bit at the top of the word for a
sixteen bit entry. A fifteen bit entry has no attribute bit at all, so it can
only be opaque.

**The route.** Accept `entry_size == 2`. Widen the entry with
`bmp::expand`-shaped code at five bits a channel and, for a sixteen bit entry,
take bit fifteen as alpha; a fifteen bit entry is opaque. One detail makes this
smaller than it looks: the data offset is already computed as
`map_length * map_entry_bits.div_ceil(8)` ([`tga.rs:253`](../../src/formats/tga.rs)),
which is two bytes an entry for both widths, so only the map's own expansion and
the alpha rule change -- the pixel data is already being read from the right
place.

**Fixture.** A hand-written pair in `tests/make-tga-fixtures.py` (the existing
maker) that holds the same indexed picture twice, once with a map of three byte
entries and once with two byte entries whose five bit channels round to the same
eight bit values. Pillow writes neither, and a hand-written TGA is an eighteen
byte header, the map and the indices.

**Acceptance.** Both spellings decode to the same bytes, and the last index in
the file reaches the last map entry, which is what catches a map read one entry
short. A fifteen bit map is opaque, which `ReadAlpha` has to agree with.

## A bare DIB

**Today.** `identify::from_extension` already maps `.dib` to `Format::Bmp`
([`identify.rs:67`](../../src/formats/identify.rs)), but `Format::Bmp` declares a
leading signature, and the router only reaches a format's module once that
signature matches. A bare device-independent bitmap has no `BM` file header at
all -- it starts at the DIB header -- so it never gets there, which is the third
family `identify.rs` names as needing "a distinct extension-assisted structural
probe" ([`identify.rs:93`](../../src/formats/identify.rs)).

**The route.** `bmp.rs` already parses the bare DIB an icon holds, through the
same header walk with `file_header: false`
([`bmp.rs:493`](../../src/formats/bmp.rs)), so the mechanism exists; what is
missing is a *decision* about the file, which belongs next to the TGA and ICO
hints rather than in the bitmap reader. The structural probe should be strict
enough that no other file can pass it: the first four bytes are one of the DIB
header sizes the reader knows (12, 40, 52, 56, 108, 124), the stated bit count
and compression code are ones the reader takes, and the raster the header implies
fits inside the file. Anything else keeps the current refusal.

**Fixture.** The DIB payload of an existing icon fixture, written out on its own
by `tests/make-bmp-ico-fixtures.py`, beside the same picture as a `.bmp`.

**Acceptance.** The bare DIB and the bitmap holding the same picture decode to
the same bytes, alpha and properties, and a text file renamed `.dib` is refused.
Note for `tests/routing.py`: the check deliberately skips `.dib` (the router's
own skip list names it), and it should keep skipping it, because a renamed file
gives the probe no extension to be assisted by. The acceptance for this item is a
pair, not a rename.

## CUR

**Today.** `identify::from_extension` maps the format to the extensions
`["ico", "cur"]` ([`identify.rs:74`](../../src/formats/identify.rs)), so a `.cur`
is routed to the icon module -- and `ico.rs` refuses it, in its own words:
"hot spot rather than a depth: this reader takes no cursor files"
([`ico.rs:136`](../../src/formats/ico.rs)). The directory of a cursor is shaped
like an icon's except that the two fields this reader scores entries by hold the
hot spot instead of the colour planes and the bit count.

**The route.** Accept directory type 2. Select the entry the way an icon is
selected but without the depth field -- the largest picture, with the hot spot
read as a hot spot and reported nowhere, because this plugin has no property for
it. The payload is the same bare DIB or PNG an icon holds, so the decode is the
existing one; the alpha is the cursor's own: a cursor carries an AND mask besides
the payload, and the mask's bit means "keep the picture" rather than "opaque", so
the rule is `payload_alpha * mask_bit` and not a choice between them.

**Fixture.** The same picture as one of the icon fixtures, written as a cursor by
`tests/make-bmp-ico-fixtures.py` (a cursor is an icon with type 2 and two
directory fields moved, so the maker can write both from one array).

**Acceptance.** The cursor and its icon twin decode to the same colour bytes, the
alpha clip is the mask combined with the payload's own alpha where it has one,
and a `.cur` renamed to another extension is still refused -- the family is
extension-assisted on purpose, and `tests/routing.py` skips it for the same
reason it skips TGA.

## The float gray+alpha TIFF

**Today.** `src/formats/tiff.rs` has `Tiff::GrayA` arms for eight and sixteen
bits ([`tiff.rs:120`](../../src/formats/tiff.rs)) and nothing for the float
spelling, so a two-sample 32-bit page falls to "the tiff colour type ... is not
one this reader takes" ([`tiff.rs:171`](../../src/formats/tiff.rs)). **The 8- and
16-bit gray+alpha page is already read** -- that half is not open, and the
candidate row in plan 34 was corrected to say so.

**The format says.** A gray page with alpha states it three ways: a
`PhotometricInterpretation` of zero or one, `SamplesPerPixel` of two, and an
`ExtraSamples` tag whose value says whether the second sample is associated
(2) or unassociated (1) alpha. `SampleFormat` 3 says the samples are IEEE floats.
The pinned decoder collapses some of those combinations into a colour type this
reader cannot use, which is why reading the directory is the route rather than
waiting for the colour type to name the shape.

**The route.** Build the layout from the directory when the colour type is
`Multiband`: photometric 0 or 1, two samples, 32 bits, sample format 3, and an
`ExtraSamples` value that names alpha. Hand out `Gray32F` for the colour clip and
the second sample as the alpha clip, which is exactly the shape
`Tiff::GrayA(8 | 16)` already produces, so the writer and the alpha extraction
need a float arm rather than a new mechanism. Add `SourceColorType::La32F` for
the `ImgSeqOriginalColorType` label: a new label is additive, and the property
has always been allowed to gain one.

**Fixture.** `tests/make-tiff-gray-fixtures.py` already writes a minimal
little-endian TIFF by hand for the low-bit pages, so this is the same writer with
two samples, 32 bits a sample, sample format 3 and an `ExtraSamples` entry. Two
files: associated and unassociated alpha, because the tag is the part that must
not be guessed.

**Acceptance.** The colour plane is the first sample and the alpha plane the
second, bit-exact against Pillow reading the same file as a float gray+alpha
image, and both fixtures report `La32F` while still handing out one colour clip
and one alpha clip.

## TIFF WebP compression, and the hook the crate does not have

**Today.** `Cargo.toml` enables `deflate`, `fax`, `jpeg` and `lzw` on the pinned
`tiff` crate, so a strip whose compression is WebP is not read. The compression
code is `0xC351` in the crate's own tag table, so a probe can name it.

**The crate cannot be handed a decoder.** Its decompression is a `match` inside
the crate, where `CompressionMethod::WebP` builds its own reader over
`image_webp::WebPDecoder`, and 0.11.3 has no decompressor trait or injection
point to replace it with. That is why this cannot be solved by configuration:
the `webp` feature *is* that decoder.

**The route is therefore to supply the decompression here.** A TIFF whose
compression is WebP is a strip of WebP bytes, and the plugin already links
libwebp for webp files, so the reader can take the strip's own `StripOffsets`
and `StripByteCounts` out of the directory, hand those bytes to libwebp and
walk the rows it produces. The shape exists twice already in this tree: a
palette page is read here rather than by the pinned decoder, and the native
ycbcr path reads its raster the same way. Nothing about the pixel policy
changes -- the strip decodes to the samples the uncompressed spelling of the
same picture holds.

**Fixture.** A WebP-compressed TIFF from libtiff's own `tiffcp -c webp`, which
is the honest source: a hand-written file that says a code it does not contain
is the lie the LZW low-bit fixture was deliberately built to avoid.

**Acceptance.** The WebP-compressed file decodes to the same bytes as the
uncompressed spelling of the same picture, and a file whose compression is a
code the reader does not take is still refused by name rather than by file.

**Landed.** The strip is handed to libwebp, which is the decoder the webp files
already go through, and the layout the probe describes is the one the directory
states, so a WebP page is r,g,b or gray like any other. The compression check moved
into the shared layout walk, which is what makes the second half of the acceptance
hold: a code this reader does not take is refused at the probe with the code in its
message, rather than by the crate at the decode. The fixture pair is one raster in
two containers -- `tiff-webp-uncompressed.tiff` and `tiff-webp.tiff`, whose strip is
libtiff's own webp bitstream -- and the validator compares them sample by sample.
The fixtures are libtiff's own, which is what the plan asked for: `tiffcp -c webp`
makes the compressed spelling from the uncompressed one this script writes, and the
file the refusal is pinned against is a real `tiffcp -c zstd` page rather than a
raster that claims a code it does not hold. `tiffcmp` agrees the two spellings hold
the same samples, which is the acceptance's first half stated by libtiff itself.

## ycbcr TIFF pages whose strip is a JPEG

**Today.** The ycbcr path reads the raster itself, because the crate's chunk reader
refuses this photometric, so a page whose strips are jpegs was refused by name even
though the crate's *whole-picture* decode takes it.

**Landed.** The probe now takes the jpeg codes (6 and 7) for a ycbcr page and records
the sampling as one sample a pixel, and the decode hands the page to the crate and
splits what it returns into three planes. Two measurements made the shape: the
crate's `colortype()` for such a page is `YCbCr(8)`, and its `read_image()` returns
interleaved ycbcr at full resolution -- the chroma is already upsampled, which is why
the format is `YUV444P8` whatever the page says its encoder used. Every other
compression of a ycbcr page is still refused by name, which is what the existing
`tiff-ycbcr-lzw.tiff` and `tiff-ycbcr-16bit.tiff` checks pin. The fixture is
`tiff-jpeg-ycbcr.tiff`, `tiffcp -c jpeg` from the same source raster the webp
fixtures are cut from, and the validator pins its format, its size and the jpeg's own
luma and chroma samples.

## JP2 gray+alpha, palettes and the `cdef` box

**Today.** `src/formats/jp2.rs` accepts one or three components and rejects two
and more than three, in its own words, "rather than silently losing a channel"
([`jp2.rs:9`](../../src/formats/jp2.rs)). The reason is real: without reading the
container's channel definitions, three components might be rgb, or ycbcr, or one
colour channel beside two others that mean something else.

**The format says.** A JP2 file's `cdef` box assigns every component a type
(colour, opacity, premultiplied opacity, unspecified) and an association
(which colour channel, or the image as a whole). `pclr` spells a palette, with
`cmap` naming which component each index channel comes from. Those boxes, not the
component count, are the statement of what the samples mean.

**The route.** Read `cdef` in the header walk the probe already makes over the
JP2 boxes: one colour component with association 1 and one opacity component is
gray+alpha; three colour components plus one opacity is rgba; a `cdef` that names
nothing falls back to the count rule that is there today. Then `pclr`: the
palette path is the one the TIFF and PNG readers already implement, where the
indices are expanded here rather than by the codec. Signed and mixed-precision
components are a separate decision and are in the last section.

**Fixture.** A JP2 written by hand around an OpenJPEG codestream, exactly the
shape `avif-split-extents.avif` is hand-written in: the repo has an OpenJPEG
decoder but no writer, so the boxes around a codestream are easier to write than
a codestream.

**Acceptance.** The gray+alpha fixture hands out a gray colour clip and an alpha
clip whose bytes match `opj_decompress`'s two components; a `cdef` that names
three colour channels is rgb and not a refusal; and an unlabelled two-component
file keeps today's refusal, because nothing in it says which sample is alpha.

**Landed: `cdef`.** The channel definitions are read in the box walk the probe
already makes, and the two shapes that need them are acted on: one colour
component beside one opacity is `La8`/`La16`, three colour components beside one
opacity is `Rgba8`/`Rgba16`. A `cdef` that names fewer channels than the file
holds is not a statement about that file's components, so the count rule stands
and an unlabelled two-component file keeps its refusal. The decode needed one
change of its own: `jpeg2k`'s `get_pixels` hands a two-component image back as
`La8` only when OpenJPEG has marked a component alpha, which is what the same
`cdef` makes it do -- so the two readers agree about the file by construction
rather than by a rule written here. The fixtures are `mono-alpha.png` and
`alpha-rgba8.png` through `opj_compress` losslessly, so the validator checks the
colour and alpha clips sample by sample against the pngs they came from, and the
unlabelled variant is the same codestream with the box dropped.

**Landed: `pclr`, and the route is open after all.** A palette page is expanded
here: the codestream holds indices and the colour is in the `pclr` box beside it, so
the decode asks OpenJPEG for the codestream's own components rather than for the
picture it would build, and looks each index up in the entries. That is what
`jpeg2k`'s `DecodeParameters::ignore_pclr_cmap_cdef()` is for -- an opt-in builder,
not the default, which is why `Image::from_bytes` hands over a post-palette
picture. What actually blocked this slice was the fixture: OpenJPEG's `pclr` reader
wants `NE`, `NPC`, **one `Bi` for each palette column** and then the entries, with
`Bi` counting bits *minus one* the way a codestream's `Ssiz` states a precision.
A single `B` byte and an eight for eight bits both made it fail silently, at
`return OPJ_FALSE` with no message, which is why it took reading `jp2.c` rather
than guessing. The fixture is the index codestream this script's header compresses
by hand, wrapped in four entries of three components and a `cmap` mapping three
channels onto them; the same codestream without the boxes is a plain gray page, and
the validator checks the expansion plane by plane against what `opj_decompress`
makes of the same file.

## Twelve bit subsampled AVIF and HEIF

**Today.** `yuv_format(chroma, depth)` maps 4:2:0 and 4:2:2 at eight and ten bits
and 4:4:4 at eight, ten and twelve
([`avif.rs:613`](../../src/formats/avif.rs)); heif's table is the same shape
([`heif.rs:292`](../../src/formats/heif.rs)). Twelve bit 4:2:0 therefore answers
`None` and the page keeps the rgb the reader builds, which is correct but is not
the planes the container holds. VapourSynth *has* the formats --
`PixelFormat::Yuv420P12` and `Yuv422P12` exist in `src/pixel.rs` -- so this is a
table waiting for evidence, not for a format.

**The route, and why it is measure-first.** The plan's caution is that the
backend's samples and chroma geometry have to be checked before the table is
extended: a twelve bit page must arrive as twelve bit samples (not sixteen with
a shift this reader would then apply twice), and its chroma planes must be
`ceil(width / 2)` by `ceil(height / 2)`. So the first step is a decode harness,
not an edit: encode one 12-bit 4:2:0 and one 12-bit 4:2:2 avif, and ask dav1d
what it produced. Then the same for a heic through libheif, whose plane sizes
come from its own handle rather than from the container's `ispe`. With those two
answers the edit is four match arms and the matching arms in the plane writer's
depth table.

**Fixture.** `avifenc` with a twelve bit source: a `P5` PGM at `MAXVAL` 4095 for
gray and a 48-bit `P7` PAM for the subsampled colour cases, both of which the
existing makers already write at other depths.

**Acceptance.** The clip is `YUV420P12` or `YUV422P12`, each plane has the size
its format implies, `_Matrix` and `_Range` are the ones the container states, and
the samples equal a reference decode of the same file (`avifdec`'s yuv output, or
the rgb conversion of it) rather than merely looking plausible.

## A heic that stores av1

**Today.** Nothing refuses it as such: the router keys on the `ftyp` major brand,
a `heic` brand goes to `heif.rs` ([`identify.rs:157`](../../src/formats/identify.rs)),
and from there libheif owns the file. `heif.rs` never inspects the item's codec.
So this is probably a working path with no file to prove it, which is why the
deferred list says only that "nothing in the corpus is one".

**The route is to make the file.** The local libheif port enables the dav1d
backend, so a heic whose primary item is `av01` should decode through the same
call as any other heic. The cheapest fixture is derived, not encoded: take the
coded item out of `avif-yuv420p.avif` and write a container around it with a
`heic` major brand and an `av01` item type, exactly as `avif-split-extents.avif`
is hand-written from the same coded item. If that decodes, the item is a fixture
and a validator section and the code does not change. If libheif refuses it, the
fallback route is one brand test in `avif.rs`: a `heic` major brand whose primary
item is `av01` is still this walker's file, and item decoding is code that
already exists.

**Acceptance.** The derived heic decodes to the same samples as the avif it came
from, through both `Read` and `ReadAlpha`, and `ImgSeqOriginalColorType` is the
label the plane layout implies.

**Landed, and the answer is that nothing was wrong.** `heic-av1.heic` is the coded
item of `avif-yuv420p.avif` in a container branded `heic`, so the router sends it
to `heif.rs` and libheif's dav1d backend decodes the av1 item; the plugin needed
no brand test at all, and the fixture is the whole change. The one thing that did
have to be fixed is the container's `av1C`: the hand-written property boxes of
the split-extent fixture padded it to seven bytes, and the three bytes after the
four the format defines are read as config OBUs. The plugin's own avif reader
looks at the flags byte alone, so the padding never mattered there -- but libheif
reads them and refused the file with `Unknown OBU type 0 of size 264`, which is
how a container that lied about its bitstream was caught.

## ISO composition offsets, edit lists and fragments

**Today.** The avif and heif sequence walker reads `stts` and takes each sample's
decode time as its presentation time
([`sequence.rs:11`](../../src/animation/sequence.rs),
[`sequence.rs:279`](../../src/animation/sequence.rs)). A `ctts` box, an `elst`
box or a fragmented file therefore displays at the wrong instants, or does not
display at all -- and the module's own header says the plan's contract is one
presentation a sample, which is only true for a file with none of the three.

**The route, in the order the boxes are worth having.**

1. **`ctts`.** A composition offset moves a sample's presentation time relative
   to its decode time. The spec's rule for the first sample to land before zero
   (clamp rather than wrap) is the one detail that has to be written down and
   tested, because it is what a naive addition gets wrong.
2. **`elst`.** An edit list's first entry may be an empty edit that delays the
   whole track, and its last may truncate. For this plugin's "plays once"
   contract an empty edit is a delay and a truncating edit is an end, so both
   belong in the segment's start and length rather than in its pictures.
3. **Fragments (`moof`, `traf`, `trun`, `tfdt`).** A fragmented file's samples
   are spread over fragments, each with its own base decode time. The timeline is
   the concatenation, so the walker grows a fragment loop and the `stbl` path
   stays as the non-fragmented case.

**Fixture.** One hand-written container per box, from the same writer style the
split-extent fixture uses: three files whose expected total duration and frame
count are known by construction, not by asking libheif -- whose per-sample
duration this build is known to report wrongly, which is why the walker exists.

**Acceptance.** Frame count, first tick and last tick for each fixture, in the
validator's animation section, beside the existing timeline checks. A file with
none of the three boxes decodes exactly as it does today, which is the control
that says the walker did not change the common case.

**Landed: `ctts`.** A composition-to-sample box is read, and a sample is
presented at its decode time plus its offset. The two details the plan names are
the two that needed writing down: a time that lands before zero is clamped, which
is what a version zero box's unsigned offsets make easy to get wrong by
subtracting, and a track whose offsets put a sample before the one before it is
refused, because this reader replays the pictures in the order they are decoded
and has nowhere to put a presentation that has to be shown before the one it
follows. The fixture is `animation.avif` with a version one box inserted by the
animation maker -- no encoder here writes one -- and it pins both: the first
sample is composed forty ticks before zero and the second a hundred after its
decode time, so the holds become 180, 70, 110 and 240 ticks and the four pictures
land on output frames 0, 5, 6 and 9 of fifteen, with the same four pictures the
plain fixture holds.

**Landed: `elst`, in part.** An edit list is read, and each of the three shapes
it can take is decided rather than ignored. An *empty* edit is a delay, which a
clip whose frames are one per output tick has nowhere to put -- the pictures are
all there, only later -- so it is read and not acted on. A normal edit that
starts partway into the media is a **leading skip**, which is refused: the
pictures are decoded in order, and there is no way to hand out the first ones for
the second half of the media. A normal edit shorter than the media is a
**truncation of the end**, which is what the timeline now carries. The fixture
pair is `animation.avif` with its identity edit replaced by one that ends the
track at 300 of its 600 ticks, and by one that starts 250 ticks in: the first is
eight frames at 24 fps with its fourth picture never shown, and the second is
refused by name.

**Fragments: a recorded non-issue.** A fragmented file's samples are not in the
movie box at all -- they are in the `moof` boxes that follow it -- so its `moov`
states no samples, and this walker takes its timeline from that box. The question
the plan asks is whether the walker should grow a fragment loop, and the answer is
that there is nothing to loop over: **libheif has no fragment parsing at all** --
`moof`, `traf`, `trun`, `tfdt` and `mvex` appear nowhere in its 1.23.1 sources,
and the word *fragment* does not either -- so a timeline read from fragments would
be one no decoder here could replay. It is not merely unimplemented, either: a
sequence-branded file whose movie box states no samples is refused outright, in
two different ways, which is what pins the shape. With an empty `stsz` beside its
`stsc` it reports *Number of samples in 'stsc' box exceeds sample sizes in 'stsz'*
and with both emptied it reports *'stsc' box with zero entries*. `animation-fragmented.avif`
is `animation.avif` with its four sample tables emptied, and it is what pins this
side: the walker finds no timeline (a Rust test) and the plugin refuses the file
by name rather than describing a sequence nothing can decode (the validator).

## Flat grayscale Y/Y+A EXR

**Today.** `exr.rs` selects the colour channels by name under `COLOR_CHANNELS =
["R", "G", "B"]` ([`exr.rs:51`](../../src/formats/exr.rs)) and takes `A` as the
alpha clip. A part that holds only `Y` is not selected, so a grayscale EXR is
refused or read as a different part -- the deferred list's natural `Gray32F`
output.

**The route.** The mechanism is the one already there, with a second name set:
`Y` for the colour plane and `A` for alpha, so a part holding `{Y}` is `Gray32F`
and `{Y, A}` is `Gray32F` plus an alpha clip. The part-selection predicate has to
be the same one the probe uses, which `exr.rs` already pins for the rgb case with
`the_part_the_probe_chose_is_the_part_that_is_decoded`; that test is what keeps a
gray part from being chosen at the probe and an rgb one at the decode. The
display-window canvas stays unreconstructed, as the module's header says.

**Fixture.** `tests/make-tiff-exr-fixtures.py` already writes a multipart EXR by
hand, so a single-part `Y` file and a `Y`+`A` file are two more calls to its
header and channel writers.

**Acceptance.** The colour clip is `Gray32F`, its plane is the `Y` samples, the
alpha clip is the `A` samples where the file states one, and the part the probe
named is the part that was decoded.

## what stays refused by decision

**JP2 signed samples, and components of two widths.** VapourSynth has no signed
integer format, so a signed component cannot be handed out as the numbers it
holds; and a frame's format names one depth, so components of two widths would
have to be widened into it. Both stay refused, at the probe, with a message that
names the component, its width and which of the two it is -- which is what
`jp2.rs`'s `a_signed_or_mixed_precision_page_is_refused_by_name` pins. The
refusal is the answer for a file this tree meets rarely, and it is a refusal
about the samples rather than a claim that the file is malformed.

The rest of the plan-34 deferrals -- camera RAW, gain maps, deep EXR, arbitrary
multiband, HDR XYZE conversion -- stay deferred, as the preamble says.

## what the order is

Cheapest and most certain first, and the compression items last because they
change the wheel rather than the reader:

1. TGA two byte map entries -- the offset arithmetic is already right and only
   the expansion is missing.
2. Bare DIB -- the reader exists, the candidate is strict and a pair proves it.
3. CUR -- the same reader one type word away, with an alpha rule of its own.
4. Float gray+alpha TIFF -- the writer exists, one layout arm and one label.
5. Grayscale EXR -- the channel selection exists, one name set.
6. Twelve bit subsampled AVIF/HEIF -- measure, then four match arms.
7. A heic storing av1 -- make the file first and find out that nothing is wrong.
8. ISO `ctts`, then `elst`, then fragments -- one box a slice, one fixture each.
9. JP2 `cdef`, then `pclr` -- the box reader is the work.
10. TIFF WebP by reading the strip here and handing the bytes to libwebp.

Every slice in that order owes the repository the same three things the low-bit
gray TIFF slice owed it: a fixture with a maker in `tests/`, a refusal or a
picture that the validator's own section pins, and a note in `CHANGELOG.md`,
because each of these turns a file that was refused into a file that decodes.
