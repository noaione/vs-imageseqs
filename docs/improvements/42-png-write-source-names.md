# 42 PNGWrite source names

**status**: implemented. `src/writer.rs` has the `output_dir` mode; its unit
tests and `tests/pngwrite.vpy`'s `test_source_names` cover it.

## what landed

`output_dir` is in `src/writer.rs`. A still takes its `ImgSeqPath` stem. An
animation's `ImgSeqAnimationIndex` repeats while a picture is held, so its
output frames use the writer's own frame index instead, and the property only
distinguishes an animation from a still. The directory is created recursively
while the node is created. `tests/pngwrite.vpy` covers still names, the
animation names, a missing `ImgSeqPath`, duplicate still stems, recursive
creation, the destination arguments and `overwrite`. `cargo test` is 363 tests
(362 passed, 1 ignored), `tests/pngwrite.vpy` passes 311 checks, and clippy
and fmt are clean. `tests/readalpha.vpy` passes unchanged, and a three-pass
`target/bench/png-write-async.py` run after the change stayed in the recorded
range: 0.74x of bounded Pillow at six frames in flight and 0.55x at twelve,
with the same file sizes.

## the problem

`PNGWrite` resolves its destination when the node is created. A multi-frame
clip must therefore number its output through `output_path` (`%d`, `%0Nd` or
`%%`), and a literal path is accepted only for a one-frame clip. This is the one
part of the writer that a caller cannot express naturally.

The source name is only known per frame. `Read` writes it as `ImgSeqPath`, and
an animated file's picture position as `ImgSeqAnimationIndex`, both on the
frame. The picture position repeats while a picture is held, so it names a
presentation rather than an output frame. That is too late for the
creation-time template either way.

`nao-manga-rls` wants one PNG per input file under the input's own stem. Today
it writes into a private staging directory as `page-%06d.png` and renames each
file after the request returns. That works, but the caller owns a second naming
scheme, a staging directory and one `os.replace` per page, and the writer is
never asked for the name the caller actually wants.

## the change

Add an `output_dir` argument that derives each destination at write time from
the frame:

```python
writer = core.imgseqs.PNGWrite(
    clip,
    output_dir=r"exports",
    compression=6,
    overwrite=False,
)
```

- `output_path` and `output_dir` are mutually exclusive. Exactly one is
  required.
- In `output_dir` mode the writer reads `ImgSeqPath`, takes its basename and
  `file_stem`, and writes `output_dir/<stem>.png`.
- If the frame carries `ImgSeqAnimationIndex`, it is an animation, and the name
  is `output_dir/<stem>-<index:04d>.png` (four padded digits), where `index` is
  the writer's own frame index. The property only distinguishes an animation
  from a still; it repeats while a picture is held, so it cannot name the
  output. A still has no animation index and gets no suffix.
- `start_number` is refused with `output_dir`; there is no number to offset.
- `overwrite`, `always_save`, `compression`, `depth`, `alpha`, `icc_profile`,
  `matrix` and `debug` keep their current meaning.
- The existing temporary-file publication is unchanged. The writer creates its
  temporary file in `output_dir` and publishes with a rename (`overwrite=True`)
  or a linking create (`overwrite=False`), so the caller no longer needs a
  staging rename.
- A relative `output_dir` is resolved against the working directory while the
  node is created, exactly as `output_path` is.
- `output_dir` is created recursively at node creation when it is missing, like
  `mkdir -p`; a path that cannot be created is an error.
- The `ImgSeqPNGWritePath` receipt is the resolved destination.
- A one-frame clip works too; a literal path is no longer the only way to
  name one frame.

`output_path` keeps its current grammar and behavior. Existing scripts do not
change.

## why this shape

`ImgSeqPath` is the property every `Read` frame already carries, and it names
the file the frame came from. A mode that uses it removes the only reason the
caller has to invent a second naming scheme.

The destination cannot be precomputed at node creation without decoding the
clip, so it is resolved in `AllFramesReady`, where the frame and its properties
are available. That is also where the writer already does its packing,
compression and publication.

`output_dir` is deliberately a directory and not a new template token. A `%s`
token (`exports/%s.png`) was considered; it is a smaller parser change, but it
keeps the caller in the naming business and still needs the same collision
handling. An explicit `names=[...]` list was considered too; it can be validated
at creation and would work for a clip that is not from `Read`, but the caller
must build it, and it does not answer "same filename" on its own. Both remain
possible later; `output_dir` is the smallest thing that makes the common case
work.

The suffix is `.png`, never the source's extension. `PNGWrite` writes PNG, so a
`page.jpg` source becomes `page.png`; a source that is already PNG keeps the
same name exactly. A caller that needs a different extension, a prefix or a
suffix has no mode that derives it yet; `name_from` and `names` are left for
later, and `output_path` can only number a name the caller already writes.

## collisions and refusals

The writer is the right place to detect collisions because it is the only place
that sees every frame's properties. It keeps a per-instance
`Mutex<HashMap<PathBuf, usize>>` from destination to frame index:

- a destination already used by a **different** frame is an error naming both
  frame numbers and the path. Duplicate still stems and the same still file
  listed twice fail this way. An animation's names carry the writer's frame
  index, so its held pictures cannot collide.
- a destination used by the **same** frame is allowed, so `always_save` can
  rewrite a frame and its receipt stays coherent.
- the entry is inserted before the write and removed if publication fails, so a
  retry can use the path again.

The check is per frame, so a collision is reported when the second frame is
requested, not when the node is created; the writer has no frame properties
before `AllFramesReady`.

On Windows the map key is a case-folded path. Two source stems that differ
only by case name one file, and `overwrite=True` would otherwise let the second
frame replace the first without a refusal.

Other refusals:

- `output_dir` is created recursively if missing; an empty path, a path that
  exists as a file, or one that cannot be created is an error.
- `output_path` and `output_dir` together, or neither, is an error.
- `start_number` with `output_dir` is an error.
- a frame with no `ImgSeqPath` is an error that names `output_path` as the
  alternative. A clip from `BlankClip`, `resize`, or any source that does not
  set the property is not a `Read` sequence.
- a property that is not a UTF-8 string is an error; `Read` writes
  `ImgSeqPath` as `Value::Utf8`.
- a source path with no usable file stem (for example `..`) is an error.
- the basename is the only part used, so a source path cannot leave
  `output_dir`. A name that the filesystem refuses (a Windows reserved name, a
  character the volume does not accept) fails at write time with the OS error;
  the writer does not sanitize names.
- an existing destination is the `overwrite` decision: `False` is the current
  path-qualified refusal, `True` replaces it. There is no separate skip mode.

Two writer instances with the same `output_dir` and the same source names can
still race each other, as two template writers can today. With
`overwrite=False` the publication primitive refuses the loser; with
`overwrite=True` the last one wins. The duplicate map is per instance and is
not a cross-process lock.

## compatibility and cost

- The plugin identifier, namespace and filter set are unchanged; the wheel
  gains no file and no dependency.
- `output_path` becomes optional in the argument signature, because exactly one
  of the two destinations is required. Existing calls that pass it are
  unaffected.
- `output_dir` creation is the one filesystem side effect at node creation; a
  node with `output_path` still creates nothing, and no PNG is written until a
  frame is requested.
- No reader, no frame format, no property and no pixel changes.
- No new dependency and no native change.
- The writer's encode path is the same. The only cost removed from the caller is
  the staging rename; this is an ergonomics change, not a performance plan. The
  nmanga handoff's end-to-end benchmark can measure the removed `os.replace`
  if that is wanted, but it should not be claimed as a writer speedup.

## how it is checked

Unit tests in `src/writer.rs`:

- argument validation: both destinations, neither destination, an empty
  `output_dir`, a path that exists as a file, a path that cannot be created,
  `start_number` with `output_dir`;
- a nested `output_dir` is created recursively at node creation, creating the
  node writes no PNG, and the first frame is written into the directory;
- name derivation: a still, an animated frame, a source with no extension, a
  source with several dots, a source with no usable stem, a frame without
  `ImgSeqPath`;
- the duplicate map: two frames resolving to one path is an error, the same
  frame is allowed, a failed publication removes its entry;
- `output_path` mode still resolves its template and `start_number` exactly as
  before.

`tests/pngwrite.vpy` gains a `test_source_names` section:

- two still fixtures written with `output_dir` land as their own stems and
  decode to their frames;
- the animated GIF fixture lands as `animation-0000.png`,
  `animation-0001.png`, ... and every frame decodes to the expected
  presentation;
- two copies of one fixture with the same basename in different directories are
  refused with the collision message;
- a `BlankClip` is refused and the message names `output_path`;
- `start_number` with `output_dir` is refused;
- an existing destination is refused with `overwrite=False` and replaced with
  `overwrite=True`;
- a clip mixing formats and sizes gets a name per frame;
- the existing `output_path` sections keep passing unchanged.
- a nested `output_dir` is created recursively; a path that exists as a file
  is refused.

Then:

```powershell
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
.\.venv\Scripts\python.exe tests\pngwrite.vpy
.\.venv\Scripts\python.exe tests\readalpha.vpy
```

No benchmark is required for acceptance: the writer's rows are unchanged. If
the nmanga end-to-end benchmark is run, it is the handoff's benchmark, not this
plan's.

## what lands with it

- `README.md`: the `PNGWrite` argument table gets `output_dir`, `output_path`
  becomes optional, and one example shows the source-name mode.
- `CHANGELOG.md`: a user-facing entry for the new argument.
- `AGENTS.md`: the writer paragraph in `source layout` gains the mode.
- `docs/improvements/README.md`: this page is the index entry.
- `docs/IMPLEMENTATION.md` and `docs/HANDOFF.md`: the mode's design and the
  tree state.
- `docs/BENCH.md`: no row; the encode path is unchanged and no speedup is
  claimed.
- The nmanga handoff can drop its staging rename for `deblur2` and
  `posterize2`; `autolevel3` still needs its per-page decision, so its staging
  section remains until that is settled.

## what it leaves

- `name_from`, a caller-named property instead of the fixed `ImgSeqPath`.
- A `names=[...]` list for a clip that is not from `Read`.
- Prefix, suffix and extension arguments in `output_dir` mode; a template
  can carry a fixed one today.
- Sanitizing a source name that the output filesystem refuses.

## not planned

- `skip_existing`: `overwrite=False` refuses an existing destination and
  `overwrite=True` replaces it. There is no third policy.
