# 09 — applying the exif orientation

- status: proposed
- touches: `src/decoder.rs`, `src/source.rs` (the argument), `src/clip.rs`
  (`expected_bytes`), `src/pixel.rs` (the writes), fixtures,
  `tests/readalpha.vpy`, `README.md`, `docs/BENCH.md` (the fair comparison note)
- expected: a file whose exif says 6 comes out the way its thumbnail looks, and
  only when the caller asks for it; `ImgSeqOrientation` keeps saying what the
  file said either way
- risk: medium — it swaps width and height, and the clip's format is decided
  from that size before any frame exists

## problem

The probe reads the orientation and `ImgSeqOrientation` carries the exif code to
every frame, but nothing in `src/` transforms anything: the picture a graph gets
is the stored one. `image` behaves the same way and leaves `Orientation::apply`
to its caller, so a phone photo, a scanned page or a camera-written webp comes
out sideways and the only way to notice is one integer in the property map.

`docs/BENCH.md`'s fair comparison already records the consequence: bestsource is
opened with `apply_rotation=False` "(imgseqs records the orientation but never
transforms the picture)". That is not bestsource's default, so the two plugins
disagree about the same file and the bench has to switch one of them off to
compare anything at all.

## what has to be decided

Applying the transform is not a property change. Orientations 5 to 8 swap width
and height, so the clip's own size, the frame allocation and the lookahead byte
estimate all move with it, and a folder that mixes one rotated page with upright
ones stops being a constant-format clip.

So the decision is which plugin this one is:

- **report only** (today). The property is the signal and the caller rotates.
  Safe, and nothing that works breaks — but every consumer that previews the clip
  or treats the plugin as an ordinary source sees the stored picture, and the
  bench keeps its note.
- **apply it**, behind an argument. `rotation=True` puts the pixels where the
  file's own metadata says they belong. The probe then has to answer the
  transformed size and the writer has to place each sample at the transformed
  index.

The plan below assumes the second, with the argument defaulting to `False`, so
today's behaviour is what every existing caller keeps and the note in `BENCH.md`
stays true; the other default is one line in the argument table if bestsource
parity is the wanted answer.

## change, as planned

- `ImageInfo.width`/`height` keep meaning the size the file stores, and the
  transformed size becomes a second answer beside them. `src/source.rs` builds
  the clip from the transformed one, so `mismatch=True` compares what the frames
  will really be, and `src/clip.rs::expected_bytes` sizes the lookahead budget
  from it.
- `rotation` joins `mismatch` in the argument list: validated at creation, and
  printed by the creation log beside `mismatch=` so a log line says which
  interpretation produced the clip.
- `ReadAlpha` transforms both clips, so the alpha plane stays aligned with the
  colour plane by construction rather than by two code paths agreeing.
- the eight exif codes are the identity, a mirror and a transpose with their
  compositions, so `src/pixel.rs` needs those two operations and a way to
  compose them, not eight branches. The planar path takes the same pair per
  plane: a 4:2:0 chroma plane is transformed with its own dimensions, and the
  rule from [03](03-webp-yuv-output.md) stands — only the frame's own rows and
  columns are written, whatever the decoder's canvas is.
- the property is written either way. A graph that wants to know what the file
  claimed rather than what was done reads `ImgSeqOrientation`, and `README.md`
  says which pair of that property and the new argument means a transform has
  already happened.
- for jpeg, png's `eXIf`, webp's `EXIF` and tiff, nothing new is read:
  `decoder::probe` already has the value. avif, heif and jxl report
  `NoTransforms` because the `image` hooks do not read their containers' exif,
  which is why `formats::heif::image_info` answers `NoTransforms` too. Whether
  the probe should start reading those boxes is a left over, not part of this.

## validation

- **fixtures for the four classes**, written by hand the way the other fixtures
  are written: the exif APP1 segment of a jpeg is a tiff header with one tag and
  a couple of dozen bytes, so a python script can write orientations 2, 3, 6 and
  8 into copies of an existing fixture (`magick -set exif:Orientation` also
  works if the hand-written segment is not wanted). Keep them small.
- **the check is arithmetic, not a hash.** `mono-alpha.png` holds `8 + 11x + 23y`
  grey, so a rotated frame's sample order can be asserted directly: the same
  numbers in the order the rotation implies. That is the same trick the
  monochrome table already uses, and it names the bug instead of hiding it in a
  digest.
- **`rotation=False` on the same files** must still hand out the stored picture
  and the same `ImgSeqOrientation`, and `mismatch=False` over a folder that
  mixes a rotated page with upright ones must fail at creation rather than at
  the first frame — that is the check that the size is decided in the probe.
- **the sandbox sets are the control**: none of them carries exif, so a
  `rotation=True` run over all of them has to produce the frame-parity dump it
  produces today. `target/bench/frame-parity.py` prints exactly that comparison.
- `cargo test --locked`, `cargo fmt`, `cargo clippy --workspace --all-targets
  --locked -- -D warnings`.

## left over

- **avif, heif and jxl exif are not read**, so a rotated avif keeps today's
  `NoTransforms` and would come out the stored way whatever the argument says.
  Reading the `Exif` item in the avif box walk is the same walk
  [08](08-color-metadata.md) extends for `nclx`, and the two should be done
  together if either is.
- **`mismatch=True` plus rotation** is two ways to get a clip whose frames
  disagree, and only one of them is a property a caller can read. If both land,
  the creation log line is the only place that says which did what.
