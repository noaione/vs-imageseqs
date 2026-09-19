# 11 — jxl without the image integration

- status: proposed
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

## change, as planned

`src/formats/jxl.rs`, a module in the shape of `webp.rs` and `heif.rs`: a
container probe, a decode, and a pair of `handles`/`output_format` answers. Two
details of the wiring are already right for it — `probe` consults
`formats::heif::image_info(path)` before it opens an `image` decoder, and
`decode` consults `format_decoder` first — so the module hangs off both hooks
with a `handles` test on the extension and the 12-byte codestream and container
signatures. That test is what the adapter used to register with `image`
(`register_decoding_hook("jxl", ..)` plus two `register_format_detection_hook`
signatures), because `image` 0.25.10 has no jxl `ImageFormat` of its own.

**the probe** parses the file header and stops:
`JxlDecoder::<states::Initialized>::new(JxlDecoderOptions::default())`,
`process` into `WithImageInfo`, then `basic_info()` for the display size, the
orientation and the depth, and `embedded_color_profile()` for the colour.
`ImageInfo` is filled the way the other formats fill it: `width`/`height` are the
size the decoder holds, `orientation` is the code, and `transform` is

```text
apply_rotation = true   identity, because the decoder already handed out the display picture
apply_rotation = false  the file's code inverted, so the writer undoes it
```

The inverse is `Rotate90 ↔ Rotate270` and every other code itself — a
`const fn inverse(Orientation) -> Orientation` beside
`Transform::from_orientation` in `src/pixel.rs`, with a unit test over the eight
codes asserting `inverse(inverse(code)) == code` and the one pair that is not its
own inverse. The displayed picture is then the decoder's own size with the
identity transform, the stored picture is the transposed size with the inverted
transform, and nothing in the write path changes: `write_decoded_planes` already
takes a transform and a source raster bigger than one of them.

**the decode** is the state machine the adapter's `decode_into` runs: `process`
to `WithFrameInfo` (looping on `NeedsMoreInput` with the next slice of the file),
one `JxlOutputBuffer::new_from_ptr` over an interleaved scratch buffer sized from
the display size and the pixel format, then `flush_pixels` until it returns false.
`set_pixel_format` asks for `U8 { bit_depth: 8 }`, `U16 { endianness: native,
bit_depth: 16 }` or `F32` from the depth, and for the alpha extra channel when the
file has one and the clip is `ReadAlpha` — `extra_channels` names the type, so a
depth or spot channel is not mistaken for alpha the way a count is.
`DecodedImage` gets `Pixels::Interleaved { color_type, buffer }` and the
`PixelFormat` the probe recorded, which is the same shape the `image` path hands
over today.

**what this unblocks**, and why it is proposed before 08 and 10 rather than
beside them:

- 09's jxl row: the code is a property again, and `apply_rotation=False` is the
  stored picture.
- 08's jxl row: "only if the integration exposes it" resolves to "it does, at
  `embedded_color_profile`", with `JxlPrimaries::{SRGB, BT2100, P3}` and
  `JxlTransferFunction::{BT709, Linear, SRGB, PQ, DCI, HLG, Gamma}` mapping into
  the H.273 codes that plan's table already lists.
- 10's jxl row: `bit_depth.bits_per_sample()` is the nominal depth, and
  `JxlDataFormat::U16 { bit_depth }` is how a frame that holds 10 bits asks for
  them.
- one copy per 16-bit jxl is saved: the adapter cannot write into an unaligned
  buffer, so it allocates a `Vec<u16>` and copies the whole image into it, where
  the write path already moves samples one at a time.

## validation

- **the fixture that fails today**: `orientation-6.jxl`, `cjxl -q 90` from
  `tests/fixtures/orientation-6.png`, added to `tests/fixtures` beside the png
  and webp families that `make-orientation-fixtures.py` documents. The validator
  asserts the display size 3x4, `ImgSeqOrientation == 6`, the display sample
  order, and — with `apply_rotation=False` — the stored 4x3 and the stored order.
  That test cannot pass before this plan and must pass after it, which is the
  whole point of the plan.
- **frame parity on everything else**: no sandbox set carries an orientation or a
  10-bit file, so every jxl page must come out bit-identical to the current
  build. That is the control, and it is a strong one: the decode loop is new
  code, and the only parameter it changes is the format it asks for.
- **the alpha path**: `ReadAlpha` over an `alpha-rgba8.jxl` (`cjxl -q 95` from
  the existing fixture) gives the color clip and `[9, 8, 7, 6, 5, 4]` on the alpha
  plane, which is what the adapter already produces today; the equality of the
  two clips' alpha is the check that the extra channel handling did not regress.
- **the probe stays a header read**: the jxl page set's clip creation time must
  stay where the debug log puts it today, because the module stops in the same
  state the adapter stopped in. A probe that decoded would show up as a
  many-second regression across 35 pages.
- `cargo test --locked`, `cargo fmt`, `cargo clippy --workspace --all-targets
  --locked -- -D warnings`, one `Read` over each sandbox set with
  `target/bench/stage-split.py`, and `target/bench/frame-parity.py` against the
  current dump.
- **no licence work**: `THIRD_PARTY_NOTICES` states that Rust crate licences are
  out of scope, and `jxl` is BSD-3-Clause pure Rust, so dropping one crate and
  adding the one under it changes nothing native. If the notices' rule ever
  widens, both crates are BSD-3-Clause and both need a line.

## left over

- **decoding straight into the frame's planes**: `JxlOutputBuffer::new_from_ptr`
  takes a byte stride, so the planes of the frame could be handed over the way
  `formats::webp` hands its planes to the writer, skipping the interleaved
  scratch buffer. It waits for a reason to do it: the scratch buffer is the same
  one the `image` path allocates today, so this plan is not slower than what it
  replaces either way.
- **the parallel runner**: `flush_pixels` takes
  `Option<&mut dyn JxlParallelRunner>` and the adapter passes none, so one frame
  decodes on one thread inside the plugin's own lookahead pool, which is the
  design [01](01-lookahead-scheduling.md) chose. Passing a runner would nest two
  pools and is not proposed.
- **animation, previews and tone mapping** are in `basic_info()` and stay
  unread, exactly as they do for every other format here: one frame per file is
  the plugin's whole contract.
- **a 10-bit jxl fixture** is not reachable with `cjxl`, which keeps the input's
  depth (a 16-bit png gives a 16-bit jxl). A `PAM` with `MAXVAL 1023` is the
  likely route if 10 needs a jxl row in its own table; `avifenc -d 12` already
  covers the plan's own fixture.
