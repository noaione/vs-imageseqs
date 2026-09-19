# 09 — applying the exif orientation

- status: implemented
- touches: `src/decoder.rs`, `src/source.rs` (the argument), `src/clip.rs`
  (`expected_bytes`), `src/pixel.rs` (the transform and the writes),
  `tests/make-orientation-fixtures.py`, `tests/fixtures/`, `tests/readalpha.vpy`,
  `README.md`, `docs/BENCH.md` (the fair comparison note)
- expected: a file whose exif says 6 comes out the way its thumbnail looks;
  `ImgSeqOrientation` keeps saying what the file said either way
- risk: medium — it swaps width and height, and the clip's format is decided
  from that size before any frame exists

## problem

The probe reads the orientation and `ImgSeqOrientation` carries the exif code to
every frame, but nothing in `src/` transformed anything: the picture a graph got
was the stored one. `image` behaves the same way and leaves `Orientation::apply`
to its caller, so a phone photo, a scanned page or a camera-written webp came
out sideways and the only way to notice was one integer in the property map.

`docs/BENCH.md`'s fair comparison recorded the consequence: bestsource was
opened with `apply_rotation=False` because imgseqs never transformed the
picture. That is not bestsource's default, so the two plugins disagreed about
the same file.

## what was decided

The transform is applied, behind `apply_rotation`, and the argument defaults to
`1` (on): a source that hands out a picture nobody can look at without extra
setup is the surprising one, and bestsource's own default makes the same choice.
`apply_rotation=0` is the escape hatch and gives the old behaviour exactly —
stored picture, stored size, same `ImgSeqOrientation`.

The property is the file's answer either way, so a graph that wants to know what
the file claimed rather than what was done reads `ImgSeqOrientation`; a code
other than `1` on a frame whose size is not the stored size says a transform has
already happened.

## what was built

- `Transform` in `src/pixel.rs` is three flags — a transpose and a mirror per
  axis — because the eight exif codes are the identity, a mirror, a transpose
  and the mirrors composed with it. `from_orientation` is the only place the
  codes appear, and the order is the one `image` documents, so `Rotate90FlipH`
  is the plain transpose (exif 5) and `Rotate90` is not (exif 6). The reverse
  mapping was written first and is wrong; the unit test that pins all eight
  expectations is what caught it.
- `Transform::output_size` is what the clip is sized from and
  `Transform::source_of` is what the writer reads per destination sample.
  `decoder::probe` resolves the transform once and both `ImageInfo` and
  `DecodedImage` hand out the transformed size next to the stored one, so
  `source.rs` builds the clip from it, `clip.rs::expected_bytes` sizes the
  lookahead budget from it, and `mismatch` compares what the frames will really
  be. A folder that mixes a rotated page with an upright one is rejected at
  creation, which is the check that the size is decided in the probe.
- the writers take the transform: `write_planar` and `write_decoded_planes` walk
  the frame's own dimensions, and a frame can be a row or a column smaller than
  the decoder's plane on an odd size, so the "does it fit" test is made against
  the source turned the same way the frame was. `ReadAlpha` transforms both
  clips, and opaque alpha is filled at the frame's size.
- the planar path takes the transform per plane, so a 4:2:0 chroma plane is
  transformed with its own dimensions and the rule from
  [03](03-webp-yuv-output.md) stands: only the frame's own rows and columns are
  written, whatever the decoder's canvas is.
- avif and heif report no orientation, because the `image` hooks do not read
  their containers' exif, and jxl reported none when this plan was written
  because the adapter it was read through did not implement `orientation()`,
  while the decoder under that adapter applied the code anyway. avif and heif are
  unchanged here and are the first left over below; jxl was closed by
  [11](11-jxl-direct.md), which dropped the adapter for the crate under it.

## the write path

A transform that transposes reads *across* the decoder buffer while the frame is
written *along* it, which the identity path never does: it copies whole rows.
Four things came out of measuring it, each one from the last:

- **one sample at a time is 9x the identity write.** 12 megapixel pages measured
  382 ms/frame against 43, which is a cache line fetched per three-byte sample.
  The destination is walked in square blocks of `TRANSFORM_BLOCK` = 8 samples
  instead (`for_each_block`), so the reads of one block stay inside whole cache
  lines and the block's other rows reuse them before the walk moves on. Samples
  move through `copy_nonoverlapping` under the bounds checks `plane_target`
  already made rather than through the `Sample` trait.
- **a mirror is not a transpose.** Every code that does not transpose reads the
  buffer along the row it is writing, so the writer copies whole rows: the row
  the mirror names, and then `reverse_samples` turns it around, which is one
  contiguous pass where a reversed gather is a sample at a time. That took the
  horizontal mirror from 2.13x the identity write to 1.09x.
- **a transpose has one walk per frame, not one per plane.** The interleaved
  path used to fill each channel with its own pass over the whole decoder
  buffer, so an `RGB24` page was read three times. `write_transposed_planes`
  reads each source sample group once and writes every channel of it, which is
  the same destination traffic for a third of the reads and a third of the loop.
- **a per-sample move has to be a constant size.** The planar writer took its
  sample size from the format at run time, so each sample was a call into the
  copy routine: the same transposed yuv page then measured 9.5x the identity
  write. `write_decoded_planes` dispatches on `bytes_per_sample` into a
  `write_sample_planes::<T>` whose sample size is a constant, and the same page
  measures 2.9x.

Measured on four 12 megapixel pages, prefetch=0, the `convert` stage only. The
interleaved pages are `RGB24` png, the planar ones are lossy webp (`YUV420P8`,
which is one and a half samples per pixel over three planes), and the identity
rows are the control that the same machine in the same state is the other
number:

| orientation | interleaved, first version | interleaved, final | identity | planar, before the sample size fix | planar, final | identity |
| --- | --- | --- | --- | --- | --- | --- |
| 6 `Rotate90` | 381.7 ms | 71.5 ms | 43.2 ms | 137.9 ms | 38.4 ms | 13.4 ms |
| 2 `FlipHorizontal` | 71.7 ms | 47.3 ms | 43.2 ms | 15.8 ms | 14.1 ms | 13.4 ms |
| 4 `FlipVertical` | 64.7 ms | 46.7 ms | 43.2 ms | — | — | — |
| 5 `Rotate90FlipH` | 378.8 ms | — | 43.2 ms | — | — | — |
| 7 `Rotate270FlipH` | 387.6 ms | — | 43.2 ms | — | — | — |

A single transposed plane with no other channel to share the read with is the
one shape that cannot use the one-walk path: 16 bit gray png measures 43.9 ms
against 18.0 ms identity and gains nothing here. The block size was swept from 4
to 128, separately for rows and columns, on all three shapes: 8 a side won on
every one of them and everything from 8 to 64 measured within 15% of it, so the
constant is small-square rather than delicate.

The mirrored codes and the identity are now the same row-wise move and cost the
same; the transposing ones are at about 0.6 ns per memory operation, which is
where scalar code stops. A blocked SIMD transpose is the only thing left to try
there, and it is not done.

## validation

- `tests/make-orientation-fixtures.py` writes `orientation-1.png` … `-8.png`:
  4x3 gray 8-bit pngs holding `row * 4 + column + 1`, with a hand-written `eXIf`
  chunk — png allows a bare tiff header — of one tag, 118 bytes each. The check
  in `tests/readalpha.vpy` is arithmetic rather than a digest: every code is
  compared sample by sample with the picture the exif table implies.
- the same script cuts `orientation-2.webp`, `-6.webp` and `-8.webp` out of
  `orientation-split.webp`, a lossy webp of four quadrants of four grey levels.
  A lossy webp is decoded straight into its own planes, so those three are the
  planar write path: the validator asserts the frame stays `YUV420P8`, that the
  chroma plane is its own size, and compares the four quadrants by their order,
  which is as much as a lossy encode keeps. The bitstream is made once by hand
  (`magick … -quality 90 -define webp:method=4`) and the script copies it and
  writes only the `EXIF` chunk and the extended header that announces it.
- the same eight png files with `apply_rotation=0` must hand out the stored
  picture at the stored 4x3 and still report the file's code, which the
  validator checks after the rotated pass.
- `files=[orientation-6.png, orientation-1.png]` fails at creation with both
  sizes named, `apply_rotation=0` accepts it as 4x3, and `mismatch=True` accepts
  it as a 0x0 clip whose frame 0 is 3x4.
- the unit tests cover every code's expectation, that each orientation reads
  every source sample exactly once, that the codes round trip through
  `Orientation::from_exif`/`to_exif`, and the blocked walk's coverage and row
  order.
- `cargo test --locked` (65 passed), `cargo fmt`, `cargo clippy --workspace
  --all-targets --locked -- -D warnings`, `cargo build --release --locked` and
  `tests/readalpha.vpy` all pass.
- **frame parity**: all six sandbox sets print the same 103 lines as before this
  plan, which is the control that no file without exif changed. None of the sets
  carries an orientation, so they take the identity path, whose code is
  untouched.
- **no regression on the sets**: the six-set stage and wall clock runs land
  inside the run-to-run noise this machine has (±10% between repeats of the same
  command), and the identity write is unchanged code.

## left over

- **avif and heif exif are not read**, so a rotated avif keeps reporting no
  orientation and comes out the stored way whatever the argument says. Reading
  the `Exif` item in the avif box walk is the same walk
  [08](08-color-metadata.md) extends for `nclx`, and the two should be done
  together if either is. [12](12-heif-avif-yuv-output.md) built that walk out
  into `src/formats/avif.rs` and read the colour through it, but left the `Exif`
  item alone; on the heif side libheif applies the container's `irot`/`imir`
  itself, which is why a rotated heic already comes out display-oriented with
  `ImgSeqOrientation` reporting 1 and why reporting it means reading those boxes.
- **jxl was worse than unread and is now fixed** by
  [11](11-jxl-direct.md). The file states its
  orientation in the codestream, the `jxl` crate the plugin reached jxl through
  applied it to the pixels itself (`JxlDecoderOptions::adjust_orientation`
  defaults to true and is read nowhere in 0.7.4) and reported the display size,
  so a jxl that stated 6 was handed out 6's way while `ImgSeqOrientation` said 1
  and `apply_rotation=False` did not undo it. The `image` adapter in between
  did not implement `orientation()`, which is what kept this silent.
  `src/formats/jxl.rs` reads the code from `basic_info()` and inverts it for the
  stored picture, and `test_orientation_jxl` in `tests/readalpha.vpy` is what
  holds it: a jxl whose codestream states 6 now reports 6, hands out the display
  picture, and hands out the stored 4x3 one under `apply_rotation=False`.
- **a transposed plane is written one sample at a time**, which scalar code
  cannot do faster than about 0.6 ns per memory operation. A blocked SIMD
  transpose is the next step if the write path ever matters more than it does:
  a rotated lossless page is 158 ms of frame time against 110 ms identity, and
  every other format spends its time in the decoder.
- **`mismatch=True` plus rotation** is two ways to get a clip whose frames
  disagree, and only one of them is a property a caller can read. The creation
  log line (`apply_rotation=`, `mismatch=`) is the only place that says which
  did what. Adding the hint to the mismatch message was considered and is not
  done.
- the sizes in the mismatch error are now the sizes the frames will be rather
  than the sizes the files store, which is what the message should name; its
  wording still reads "has 4x3 Gray8, expected frame 0 … to be 3x4 Gray8".
