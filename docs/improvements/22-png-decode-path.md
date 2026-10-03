# 22 - a png decode path that does not go through `image`

status: measured, not implemented. the plan is a `src/formats/png.rs` that
decodes and probes with the `png` crate directly, the way
[11](11-jxl-direct.md) dropped the `image` adapter for jxl. the numbers below
are the evidence that it is worth doing; they were taken on
`i5-11400h`, windows 11, rust release build, `prefetch=0`, best of three
alternating rounds.

## the question

the nmanga benchmark in the sibling `vs-nimages` checkout times a python
Pillow pipeline against this plugin on the same pages and reports a `decode`
column for each. Pillow's column is `Image.open` + `image.load` +
`image.convert("L")`; the plugin's column is the `total` its own debug log
prints for one frame at `prefetch=0`. on the posterize suite Pillow's column is
the smaller one, which is the thing to explain:

| suite | pages | Pillow decode | plugin decode | ratio |
| --- | ---: | ---: | ---: | ---: |
| `sandbox/posterize-check` | 49 | 1.948 s | 2.550 s | 1.31x |
| `sandbox/posterize-check` (48 png only) | 48 | 1.663 s | 2.258 s | 1.36x |
| `sandbox/level-check` (jpeg) | 129 | 2.220 s | 2.088 s | 0.94x |

jpeg is a win and png is a loss, so it is the png path and not the harness.
`target/bench/decode/png-decode.py` reproduces both columns with the same
staging, and its per-stage table is where the loss is:

| stage | posterize-check, 48 png | level-check, 129 jpeg |
| --- | ---: | ---: |
| `read` (the decode) | 28.08 ms/frame | 10.31 ms/frame |
| `convert` (the plane write) | 16.35 ms/frame | 4.36 ms/frame |
| `total` | 46.42 ms/frame | 15.74 ms/frame |

## what the png path costs today

`src/formats/png.rs` decodes nothing; `image` does, and `image`'s png decoder
sets `Transformations::EXPAND` and hands back one whole `ImageBuffer` per file.
for a palette page that is an `Rgb8` buffer of 36 MB, which the plugin then
copies into the frame. so every page is written twice and read once more than
it has to be.

two measurements say what each half costs.

**the inflate backend is already the best one available.** a decoder swap is
not where the time is. `target/bench/decode/inflate-bench` drives four decoders
over the `IDAT` streams of the same pages and checks every one of them against
`flate2` before timing:

| backend | into a reused buffer | one-shot with its own `Vec` |
| --- | ---: | ---: |
| `fdeflate` (shipped) | 688.5 ms, 853 MB/s | 796.0 ms, 738 MB/s |
| `flate2` + `zlib-rs` | 882.0 ms, 666 MB/s | 1018.1 ms, 577 MB/s |
| `miniz_oxide` | 916.6 ms, 641 MB/s | 1291.7 ms, 455 MB/s |
| `zune-inflate` | cannot write into a caller's buffer | 1042.3 ms, 564 MB/s |

560 MiB of inflated image data, five rounds, best of each. `fdeflate` has no
architecture specific code and no SIMD of its own, so nothing about a wider
instruction set changes this row; `zlib-ng`, which is what Pillow's png codec
links, is not in the lockfile and crate downloads are not available here.

**the extra pass is worth about a quarter of the decode side.**
`target/bench/decode/inflate-bench --bin png-path` decodes every page two ways
and refuses to time them until both produce the same bytes row for row:

* today: `next_frame` into a flat buffer, then that buffer copied into a
  stride-padded buffer shaped like a VapourSynth plane. both buffers are
  allocated inside the timed loop, so the allocator and the first touch of the
  destination are paid by both shapes.
* proposed: `next_row` straight into the stride-padded buffer.

| shape | posterize-check, 48 png, 1474 MiB of output |
| --- | ---: |
| today | 1924.3 ms |
| proposed | **1475.4 ms, 0.767x, 23.3% off** |

the same binary times the probe: `png`'s own header reader, which answers the
size, the colour type, the bit depth, the palette and the embedded icc profile
without decoding a sample, is 1.62 ms for all 48 files, 33 us each.

## x86-64-v3 and x86-64-v4

`target/bench/decode/build-variants.ps1` builds the plugin once per
microarchitecture baseline. each `RUSTFLAGS` value invalidates the whole
dependency graph, so every variant is a full rebuild; `-C target-cpu` is the
only difference between them.

| configuration | `sandbox/png` | posterize-check 49 | level-check 129 |
| --- | ---: | ---: | ---: |
| default (`x86-64`) | 1.217 s | 2.550 s | 2.088 s |
| `x86-64-v2` | 1.206 s | | |
| `x86-64-v3` (avx2) | **1.148 s** | 2.356 s | 1.877 s |
| `x86-64-v4` (avx512) | 1.204 s | **2.278 s** | **1.791 s** |

the gain is real, between 6% and 11%, and it is mostly in the plane write and
not in the decoder:

| `convert` stage | `sandbox/png` | posterize 48 png | posterize 49 | level-check |
| --- | ---: | ---: | ---: | ---: |
| default | 8.06 ms | 16.35 ms | 16.59 ms | 4.36 ms |
| `x86-64-v3` | 7.18 ms | 12.25 ms | 13.09 ms | 2.97 ms |
| `x86-64-v4` | 7.29 ms | 13.67 ms | 12.95 ms | 2.91 ms |

the `read` stage moves far less (24.54 -> 23.53 ms on `sandbox/png`,
32.99 -> 30.81 on posterize-check), which is what a copy loop auto-vectorized
to 256 bits looks like and what an unchanged scalar inflate looks like. v3 and
v4 are within run to run noise of each other; both need avx2 for v3 and
avx512f/bw/vl for v4, which is a distribution decision and not a free win.

an i5-11400h reports avx2, avx512f and avx512bw, so the v4 build runs here.
the three builds were also checked for behaviour and not only for speed.
`tests/readalpha.vpy` run against each of them through `IMGSEQS_PLUGIN` produces
byte identical logs, 527 `ok` checks each, so a wider baseline moves no sample,
no format and no property. the logs are in `target/bench/decode/`. the run ends
on a pre-existing `animation.avif` failure (`NoMatchingDecoderInstalled`) that
the shipped default build shows too, so it belongs to this checkout's avif setup
and not to these builds. the repository's own benchmark, which the plan is not
trying to move, reads the same shape it always did on `sandbox/png`: `imgseqs`
0.478 s against `bestsource` 0.682 s, 1.43x on frames and 3.23x including the
open, with `prefetch=0` at 1.165 s.

## the intended edit

`src/formats/png.rs` grows a `handles`/`decode` pair beside the `cICP` reader
it already has, and `src/decoder.rs` prefers it for a file whose extension is
`.png` and whose header this module can follow, falling back to `image` for
anything it refuses the way [17](17-avif-container-robustness.md) refuses a
container it does not follow.

* the probe reads the header with `png::Decoder::read_header_info`, which gives
  the width, the height, the bit depth, the colour type, the palette and the
  icc profile without inflating anything, and keeps the existing chunk walk for
  `cICP`. a probe must promise exactly the format `image` promised, so the
  output colour type is computed the same way `image` computes it from
  `Transformations::EXPAND`.
* the decode drives `png::Reader::next_row` and writes each row into the
  frame's plane at `stride`, which is the plane-shaped shape the prototype
  measured. a sixteen bit page keeps two byte samples and a palette page
  arrives already expanded, exactly as `image` hands them over today.
* alpha is the one thing that is not a straight row copy: a file with an alpha
  channel needs `demand` to say whether an alpha clip exists, and the two
  planes are filled from one interleaved row the way
  `src/formats/webp.rs` already does it.

the animation path is untouched: `src/animation/apng.rs` already drives `png`
itself, at `Transformations::IDENTITY`, because it composes at the file's own
depth.

## how to check

* `target/bench/decode/png-decode.py --dir sandbox/posterize-check --pattern
  "*.*" --stage --pillow-mode gray` before and after: the `imgseqs` row should
  move from about 1.31x Pillow to about 1.0x, and `convert` should disappear
  from the stage table because the decode now writes the frame itself.
* `target/bench/frame-parity.py` over the six `sandbox` sets and
  `tests/fixtures`, which is the byte for byte check this change needs: it is
  the one change here that moves where every sample is written.
* `tests/readalpha.vpy`, whose png fixtures cover the palette, the depth and
  the alpha rows.
* `cargo test --locked`, and the bit depth rows of the validator, because a
  sixteen bit page is the arm most likely to be got wrong.

## what this page does not decide
two questions the first draft of this page carried are their own plans now: the
posterize-check corpus is 44 palette pages whose grey content the plugin hands
out as `RGB24` where Pillow's timed column stops at `L`, so the two `decode`
columns are not the same amount of work, and that is
[25](25-decode-column-parity.md); and shipping a wider build instead of the
default one is [23](23-cpu-variant-avx2.md) for the `avx2` variant and
[24](24-cpu-variant-avx512.md) for the `avx512` one.

what is left here is the decode path itself, which the numbers above say is
where the extra pass is and therefore worth doing whatever the benchmark
columns turn out to be comparing.
