# 25 - the `decode` column is not the same amount of work

status: measured, not implemented. this is about the benchmark and not about the
plugin: the two `decode` columns it puts side by side stop at different points,
and the plan is to report that instead of adding a hand-out that would win the
column.

## what the two columns are

the nmanga benchmark in the sibling `vs-nimages` checkout reports one `decode`
per pipeline. Pillow's is `Image.open` + `image.load` + `image.convert("L")`,
which always ends at one byte per pixel. the plugin's is the `total` its own
debug log prints for one frame at `prefetch=0`, which ends at whatever format
that file's container states.

on `sandbox/posterize-check` those are not the same thing. 44 of the 49 pages are
palette pngs, so the plugin hands out `RGB24`, 36 MB per frame on a 2903x4128
page, while Pillow's timed column ends at `L`, 12 MB. the graph then pays for the
difference in the benchmark's own `resize` column, 0.30 s to 0.42 s on that
suite. the `total` column is therefore fair and the `decode` column is not.

## the three pairings, measured

`target/bench/decode/png-decode.py` puts the same files through each side. the
figures below were taken after [22](22-png-decode-path.md) landed, in one batch,
`prefetch=0`:

| set | pages | Pillow `load` + `L` | plugin `total` | ratio |
| --- | ---: | ---: | ---: | ---: |
| `sandbox/png` | 35 | 1.190 s | 0.820 s | 0.689x |
| `sandbox/posterize-check` | 49 | 1.733 s | 1.544 s | 0.891x |
| `sandbox/level-check` (jpeg) | 129 | 1.879 s | 1.702 s | 0.906x |

before 22 the posterize row was 1.31x, so what this page is about is settled in
the plugin's favour rather than left as a tie: its `total` is a smaller number
than Pillow's column on all three sets now, including the one where it hands out
three bytes per pixel and Pillow's column stops at one. the jpeg set is the
control and its path is untouched.

**the `read` stage is no longer a decoder-only figure for png.** before 22 it
was the `image` decode alone and the plane write was `convert`; a png this
plugin walks now places its rows inside `read`, so `convert` is zero and `read`
is the decode and the placement together. the old table this page held compared
`read` against Pillow's whole column and found the plugin's decoder smaller on
all three sets; that comparison cannot be made on a walked png any more, and
the honest one is `total` against Pillow's `decode`, which is the table above.
what still isolates a decoder is `target/bench/decode/inflate-bench`, which
measures inflation with no frame involved at all.

## why this is a plan and not a fix in the plugin

the obvious way to win the column is to hand out `Gray8` for a palette page
whose palette holds only greys. that would be wrong twice over: a frame's format
is the file's own colour type, which is what the plugin documents and what a
graph relies on, and it would change every frame of a corpus that already reads
correctly. the column is the thing to fix.

## the intended edit

* `target/bench/decode/png-decode.py` already reports the format the plugin
  handed out; it grows a row that matches it, so a run can print "Pillow to the
  plugin's format" and "the plugin to Pillow's format" next to the two native
  columns. the `read`-only column the table above uses is the one already in its
  `--stage` output.
* the sibling `vs-nimages` owns its own reporting: its `decode` column should
  either carry the format the plugin handed out beside it, or the posterize rows
  should be compared as `decode + resize`, because that is where a format choice
  is paid. that repository's `docs/BENCH.md` is where the result belongs.
* whatever the reporting becomes, the `total` column must not move: this plan
  changes no pixel and no plugin code.

## how to check

* `.venv/Scripts/python.exe target/bench/decode/png-decode.py --dir
  sandbox/posterize-check --pattern "*.*" --stage --pillow-mode gray` prints the
  plugin's format and both sides' stages, which is the table above.
* the same on `sandbox/png` and `sandbox/level-check`, where the plugin's `read`
  stage must stay below Pillow's `load` and the shapes must agree with the
  numbers recorded here.
