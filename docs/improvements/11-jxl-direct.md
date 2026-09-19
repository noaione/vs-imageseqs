# 11 — jxl without the image integration

- status: implemented
- touches: `Cargo.toml`, `src/formats/mod.rs`, `src/formats/jxl.rs` (new),
  `src/decoder.rs`, `src/pixel.rs` (one function), `docs/IMPLEMENTATION.md`,
  fixtures, `tests/readalpha.vpy`
- expected: a jxl that states an orientation reports it and is handed out the way
  the file describes, `apply_rotation=False` gives the stored picture back, and
  the codestream's colour encoding and bit depth reach `ImageInfo` — which is
  what [08](08-color-metadata.md) and [10](10-nominal-bit-depth.md) are waiting
  for, and what [09](09-exif-orientation.md) left open for jxl
- risk: medium — the decode loop becomes ours, and jxl is one of the two formats
  whose decode a mistake changes pixels rather than properties

## problem

jxl is the one format the plugin reads through a decoder it did not write:
`src/decoder.rs::register_decoder_hooks` calls
`jxl_image_rs_integration::register_image_decoding_hook()`, which registers the
`jxl` extension and the two codestream signatures with `image`, so a `.jxl` file
is opened by `ImageReader` like a png and answered by that crate's
`ImageDecoder` adapter. The adapter is 367 lines around the `jxl` crate
(`libjxl`'s Rust decoder, BSD-3-Clause, pure Rust, no native dependency of its
own — the same crate the adapter wraps, so the dependency does not change, only
the layer).

That adapter exposes `dimensions()`, `color_type()`, `icc_profile()` and pixels,
and nothing else. Everything the codestream states about itself that this
repository's open plans need is behind it and dropped:

| what the file states | where the `jxl` crate has it | who needs it |
| --- | --- | --- |
| the display size | `basic_info().size`, already mapped by the orientation | the probe |
| the orientation code | `basic_info().orientation`, exif-numbered 1 to 8 | 09, and the defect below |
| the nominal bit depth | `basic_info().bit_depth`, `Int { bits_per_sample }` or `Float { .. }` | [10](10-nominal-bit-depth.md) |
| the extra channels | `basic_info().extra_channels` (`ec_type`, `alpha_associated`) | the alpha path |
| the colour encoding | `embedded_color_profile()` → `Simple(JxlColorEncoding { white_point, primaries, transfer_function, rendering_intent })`, or `Icc(..)` | [08](08-color-metadata.md) |
| the container's exif box | `aux_boxes(JxlAuxBoxType::Exif)` | nothing yet: the codestream's own field is the authoritative one for jxl, and this is the box a jxl written by a camera also carries |
| the output format | `set_pixel_format(JxlPixelFormat { color_type, color_data_format, extra_channel_format })` | 10, and 03's habit of asking for what the frame wants |

and the adapter answers three of them with a value of its own choosing instead:
`color_type()` maps every integer depth above 8 to `L16`/`Rgb16` (so a 10-bit jxl
is handed out as `Gray16`/`RGB48`), `icc_profile()` returns the *output* profile
the decoder converted to rather than the embedded one, and `orientation()` is not
implemented at all, which is what makes the defect below silent.

Probing is not the cost: the adapter stops at the `WithImageInfo` state, so the
probe already reads headers only. A module of our own would stop in the same
place.

## the defect it hides

**a jxl that states an orientation is rotated by the decoder, whatever the
plugin was asked for.** `JxlDecoderOptions::adjust_orientation` exists in the
`jxl` crate, defaults to true, and is read nowhere in 0.7.4 — the field is
declared and the render path applies `image_metadata.orientation`
unconditionally. The adapter takes the default, so the pixels arrive display
oriented and `basic_info().size` is the display size, while `ImgSeqOrientation`
reports 1 for a file that states 6.

Measured, with the two tools at hand and the plugin beside them:

```console
> cjxl tests/fixtures/orientation-6.png target/bench/jxl-probe/orientation-6.jxl -q 90
Compressed to 165 bytes including container (110.000 bpp).

> djxl target/bench/jxl-probe/orientation-6.jxl roundtrip6.png
Decoded to pixels.
3 x 4, 0.001 MP/s, 12 threads.        # the png on disk is 4 x 3

> python target/bench/jxl-probe/probe.vpy
orientation-1.jxl: 4x3 fmt=Gray8 orientation=1
    [2, 3, 3, 5] [5, 5, 6, 8] [8, 8, 10, 11]
orientation-6.jxl: 3x4 fmt=Gray8 orientation=1
    [8, 5, 2] [8, 5, 3] [10, 6, 3] [11, 8, 5]
orientation-1.png: 4x3 fmt=Gray8 orientation=1
    [1, 2, 3, 4] [5, 6, 7, 8] [9, 10, 11, 12]
orientation-6.png: 3x4 fmt=Gray8 orientation=6
    [9, 5, 1] [10, 6, 2] [11, 7, 3] [12, 8, 4]
```

So `apply_rotation=True` happens to hand out the right picture *for the wrong
reason* — the decoder did it, not the plugin — and the two things 09 promised are
both false for this format: `ImgSeqOrientation` says 1 rather than 6, and
`apply_rotation=False` still hands out the rotated picture instead of the stored
one. Every other format obeys the contract; jxl cannot, through this adapter.

## change, as built

`src/formats/jxl.rs`, a module in the shape of `webp.rs` and `heif.rs`: a
container probe, a decode, and a pair of `owns`/`handles` answers. Two
details of the wiring were already right for it — `probe` consults
`formats::heif::image_info(path)` before it opens an `image` decoder, and
`format_decoder` is consulted before `image` is asked — so the module hangs off
both hooks. Both hooks answer by extension alone, because the adapter used to
register the same extension with `image` (`register_decoding_hook("jxl", ..)`
plus two `register_format_detection_hook` signatures), and `image` 0.25.10 has no
jxl `ImageFormat` of its own. The signatures are not lost to that: the module
peeks at the first bytes of the file it is about to hand to the decoder and
refuses a file that starts with neither a codestream nor a container.

**the probe** parses the file header and stops:
`JxlDecoder::<states::Initialized>::new(JxlDecoderOptions::default())`,
`process` into `WithImageInfo`, then `basic_info()` for the size, the
orientation, the depth and the extra channels, and `embedded_color_profile()` for
the colour. `ImageInfo` is filled the way the other formats fill it:
`width`/`height` are the size the decoder holds, `orientation` is the code, and
`transform` is

```text
apply_rotation = true   identity, because the decoder already handed out the display picture
apply_rotation = false  the file's code inverted, so the writer undoes it
```

The inverse is `Rotate90 ↔ Rotate270` and every other code itself —
`pixel::inverse_orientation`, beside `Transform::from_orientation`, with a unit
test that runs the eight codes over a grid and asserts the inverted one puts the
grid back. The displayed picture is then the decoder's own size with the
identity transform, the stored picture is the transposed size with the inverted
transform, and nothing in the write path changes: `write_decoded_planes` already
takes a transform and a source raster bigger than one of them.

One detail of the probe is a coupling that has to be stated: the crate reports
the size with the width and height **already swapped** for the four transposing
codes, whether or not a code was applied to the raster, and it renders every
code unconditionally — `JxlDecoderOptions::adjust_orientation` exists but is read
nowhere in 0.7.4. So "the size from `basic_info()` is the display size" and "the
raster is the display raster" are two assumptions that happen to agree today.
The `apply_rotation=False` path is the one that would break if a future crate
honoured the option: it would apply the inverse code to a raster that is already
stored. `test_orientation_jxl` in `tests/readalpha.vpy` is the guard, because it
asserts the stored picture and the stored size together.

**the decode** is the state machine the adapter's `decode_into` ran: `process`
to `WithFrameInfo` (looping on `NeedsMoreInput` with the next slice of the
file), one `JxlOutputBuffer::new_from_ptr` over an interleaved buffer sized from
the size and the pixel format, then `flush_pixels` until it returns false.
`set_pixel_format` asks for `U8 { bit_depth: 8 }`, `U16 { endianness: native,
bit_depth: 16 }` or `F32` from the depth, and for the alpha extra channel as the
one `JxlExtraChannel` whose `ec_type` is `Alpha` — the extra channels are named
rather than counted, so a depth or spot channel is not mistaken for alpha.
`DecodedImage` gets `Pixels::Interleaved { color_type, buffer }` and the
`PixelFormat` the probe recorded, which is the same shape the `image` path hands
over.

The buffer is allocated 8-byte aligned so the `u16` case can be drawn straight
into it, and the rare `Vec<u8>` that the allocator does not align is filled
through a small `Aligned` window instead of a second full copy. That is where the
adapter's extra copy of every 16-bit jxl goes away: it allocated a `Vec<u16>`
whenever the buffer it was given was not aligned and copied the whole image into
it.

**what this unblocks**, and why it was done before 08 and 10 rather than beside
them:

- 09's jxl row: the code is a property again, and `apply_rotation=False` is the
  stored picture.
- 08's jxl row: "only if the integration exposes it" resolves to "it does, at
  `embedded_color_profile`", with `JxlPrimaries::{SRGB, BT2100, P3}` and
  `JxlTransferFunction::{BT709, Linear, SRGB, PQ, DCI, HLG, Gamma}` mapping into
  the H.273 codes that plan's table already lists.
- 10's jxl row: `bit_depth.bits_per_sample()` is the nominal depth, and
  `JxlDataFormat::U16 { bit_depth }` is how a frame that holds 10 bits asks for
  them.

## validation

- **the fixture that failed before**: `orientation-6.jxl`, `cjxl -d 0` from
  `tests/fixtures/orientation-6.png`, added to `tests/fixtures` beside the png
  and webp families that `make-orientation-fixtures.py` documents, with
  `alpha-rgba8.jxl` (`cjxl -d 0` from the existing alpha fixture) beside it. The
  validator asserts the display size 3x4, `ImgSeqOrientation == 6`, the display
  sample order, and — with `apply_rotation=False` — the stored 4x3 and the stored
  order; it also checks that the same page read as a png and as a jxl is the same
  frame, that the two mix into one sequence, and that the jxl alpha page matches
  its png channel for channel.
- **frame parity on everything else**: no sandbox set carries an orientation or a
  10-bit file, so every one of the 35 jxl pages had to come out bit-identical.
  `frame-parity.py` over the jxl set before and after differs by six *names* in
  the sorted listing (the new `alpha-rgba8.jxl` fixture sorts among the
  `alpha-*.png`/`.heic` entries) and by no hash at all: every plane of every frame
  is unchanged.
- **the alpha path**: `ReadAlpha` over `alpha-rgba8.jxl` gives the same two clips
  as `alpha-rgba8.png`, which is what the adapter produced before this plan.
- **the probe stayed a header read**: 35 pages, `mismatch=True`, clip creation
  measured three times per row — before, 2.3 ms per clip (0.066 ms per file);
  after, 2.3 ms per clip (0.064 ms per file). A probe that decoded would have
  been seconds across 35 pages. The `apply_rotation=False` row read 4.6 ms in
  that batch, but a second batch of the same build put the whole set at 2.9 ms
  and 3.4 ms, so the second header parse that path costs is worth fractions of a
  millisecond per file rather than the doubling the first pair suggested — and it
  is what makes `ImgSeqOrientation` non-empty for a jxl at all.
- **no regression in the pipeline**: a same-batch interleaved A/B against a
  baseline DLL built from the pre-plan tree, twelve pages, `prefetch=0`,
  `apply_rotation=True`, minimum of three: decode 443.35 ms and 457.94 ms total
  before, 442.19 ms and 457.22 ms after. The png control A/B in the same session
  was equal too (36.16/50.56 against 35.55/49.62), which is what says the machine
  was not drifting between the two rows. Single batches of this set have read as
  443 ms and as 564 ms for the same build.
- `cargo test --locked` 75 passed, `cargo fmt`, `cargo clippy --workspace
  --all-targets --locked -- -D warnings` clean, `tests/readalpha.vpy` all checks
  passed with 14 of them under `test_orientation_jxl`, and `cargo build --release
  --locked` produced a DLL 20,992 bytes smaller than the one it replaces
  (8,397,312 to 8,376,320), because the adapter and its `image` traits are gone.
- **no licence work**: `THIRD_PARTY_NOTICES` states that Rust crate licences are
  out of scope, and `jxl` is BSD-3-Clause pure Rust, so dropping one crate and
  adding the one under it changes nothing native. If the notices' rule ever
  widens, both crates are BSD-3-Clause and both need a line.

## left over

- **the probe reads the header twice when the rotation is off**: the code has to
  be reported and the size the decoder hands out is the one it applied the code
  to, so the stored size and the code can only be had from a second header parse.
  It costs 0.065 ms per file and only when `apply_rotation=False`, and it is what
  makes `ImgSeqOrientation` non-empty for a jxl at all.
- **`adjust_orientation` is a trap for a future upgrade**: the option exists in
  0.7.4, is read nowhere, and the render applies the codestream's code whatever it
  says. If a later version honours it, the option must be left `true` — the
  `src/formats/jxl.rs` module doc and the size coupling above record the
  reasoning.
- **decoding straight into the frame's planes**: `JxlOutputBuffer::new_from_ptr`
  takes a byte stride, so the planes of the frame could be handed over the way
  `formats::webp` hands its planes to the writer, skipping the interleaved buffer.
  It waits for a reason to do it: the interleaved buffer is the same one the
  `image` path allocates, so this plan is not slower than what it replaced.
- **the parallel runner**: `flush_pixels` takes
  `Option<&mut dyn JxlParallelRunner>` and the module passes none, so one frame
  decodes on one thread inside the plugin's own lookahead pool, which is the
  design [01](01-lookahead-scheduling.md) chose. Passing a runner would nest two
  pools and is not proposed.
- **animation, previews and tone mapping** are in `basic_info()` and stay
  unread, exactly as they do for every other format here: one frame per file is
  the plugin's whole contract.
- **a 10-bit jxl fixture is a hand-written pgm and one `cjxl` call**, not a
  missing ingredient: the first draft of this plan said `cjxl` could not produce
  one, and that was wrong twice over. `cjxl -d 0` reads a `PGM` whose `MAXVAL`
  is 1023 as a native ten-bit image with no flag at all, and
  `--override_bitdepth=N` states the depth for any input. A 4x3 grid of `1..12`
  is 25 bytes, `jxlinfo` prints `10-bit Grayscale` for it, and the ten-bit values
  survive a round trip exactly, which is what [10](10-nominal-bit-depth.md) wants
  for its jxl row.
