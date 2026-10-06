# 40 frame source indices

**status**: implemented

## the defect

`ImgSeqIndex` was written from the output frame number. `FrameBuilder::produce`
is handed the frame the filter was asked for, and it passed that index straight
through `ClipFrames::build` into `color::set_frame_properties`, so the property
named the frame's position in the output clip.

For a list of stills the output frame number *is* the file's position, which is
why this went unnoticed for two releases. Once an animation precedes a file,
every frame of every later file is numbered by the animation's frame count as
well: a list of one 15-frame animation and one still reported the still as
frame 15 rather than file 1.

Two README statements described the property at once, and they were not the
same statement, which is what surfaced the defect:

- the frame-properties table said "file's position in the input list", which
  `docs/IMPLEMENTATION.md` repeated as "its position in the list";
- the animated-images section said "the picture's position within its file, so
  an animation's frames are numbered from zero within that animation".

They cannot be one property, because an animation contributes several output
frames: the output frame number is neither the file's position nor the
picture's. `docs/improvements/38-png-write.md` also leans on the second reading
when it says a writer must number by its own frame index rather than an
`ImgSeqIndex` inherited from a source.

## the change

- `animation::FrameRef` carries `file`, the segment's position in the table,
  which is the path's position in the `files` list. `SegmentTable::resolve`
  already computes that index to find the segment, so the field costs nothing.
- `color::SourceIndices { file, animation }` is what a frame is now built from.
  `ImgSeqIndex` is `file`. `ImgSeqAnimationIndex` is the picture's position
  within that file, which `FrameBuilder::produce` fills with the presentation
  index for an animated segment and leaves `None` for a still, because a still's
  one picture is already named by the file's own index.
- A still therefore carries no `ImgSeqAnimationIndex` at all, the way
  `ICCProfile` is absent when export is off and `ImgSeqOrientation` is absent
  for a file that states none.
- Both clips of a `ReadAlpha` call carry both names, because every clip's frames
  are built from the same `SourceIndices`.

## how it is checked

- `animation::tests::the_table_orders_stills_and_animations` asserts the file
  index of a still before, inside and after an animation.
- `tests/readalpha.vpy` (`test_source_indices`) asserts that two stills report
  files 0 and 1 and no picture position; that an animation's 15 frames all
  report their own file while `ImgSeqAnimationIndex` repeats across the ticks
  each picture is held; that the still after it reports the next file and no
  picture position; and that the animation placed second is file 1 for every one
  of its frames, which is the ordering the output frame number got wrong.
- The alpha clip is checked on every one of those frames.
