# 50 — a dds row stream, built and measured, decided against

- status: **decided against**. Four attempts were written, measured and reverted;
  the buffered reader stayed, and the 1.12x it gives up to the `image` crate is
  not the copy this plan was about.
- touches: `src/formats/dds.rs` (three experimental branches, all reverted)
- expected: a DirectDraw surface decoded straight into the frame, with the
  intermediate 4.3 MB `Vec` gone and the measured 0.86 ms a file of extra
  traffic with it.
- risk: the one that mattered was not correctness — every attempt was byte
  identical — but speed: each stream was slower than the copy it removed, which
  is what stopped it.

## problem

`still::Decoder::read(self, buffer)` in the `image` crate filled the caller's
frame directly, while `src/formats/dds.rs` decodes into a `Vec` of its own that
the plugin then copies into the frame. On a 3000x3000 DXT5 page that is 4.3 MB
of write, read and write again, about 0.86 ms, against a measured gap of 1.15 ms
between this reader and the one it replaced ([26](26-remove-image-rs.md)).

The shape of the fix looked obvious, and `src/formats/ppm.rs` is the proof that
the shape works: a format answers `Pixels::Stream` and hands each decoded row to
`RowSink`, which is one entry per plane. `src/formats/png.rs:650` is the model.

## evidence

Four builds, each measured against the buffered reader on the corpus this tree
has, then reverted:

| attempt | milliseconds | buffered | verdict |
| --- | ---: | ---: | --- |
| stream, decode a block row into a scratch line | 67.1 | 60.7 | slower |
| stream, transpose per block | 62.5 | 60.7 | slower |
| stream, block row decoded in place | 66.1 | 60.7 | slower |
| whole lines copied instead of pixels | 57.7 | 55.3 | slower |

and the arithmetic was ruled out separately: `widen`, `from_565`, `colour_block`
and `alpha_levels` were compared function by function against `image`'s
`dxt.rs` and are already the same algorithm. The regression is real -- fifteen
interleaved pairs, paired ratio median 1.124, faster in 3 of 15 -- and it is not
in the code those attempts touched.

The rule the tree kept from this is in the index beside `ppm`'s win: streaming
pays when a decoder's own output order already matches the frame's, and loses
when it has to transpose, because then it trades one bulk copy for per-pixel
work. A DXT block is four by four pixels of one surface, so every stream here
transposes.

## what is left

Nothing. [34](34-input-routing-and-planar-decode.md)'s DXT colour kernel is the
other half of the same question and it is a *different* proposal: it does not
move memory, it makes the per-block interpolation cheaper. It is still open and
still unmeasured, because it needs a quiet machine and this one is not. The
`dds` row is closed.
