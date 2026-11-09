# 48 — a faster decode path for the formats this tree decodes itself

- status: proposed, not implemented
- touches: `src/formats/{dds,bmp,tga,ico,pnm,farbfeld,hdr,png,exr,tiff}.rs`,
  `src/decoder.rs` (`RowSink`), `src/pixel.rs`, a new `src/planar.rs`,
  `src/animation/{gif,apng,webp}.rs`, `src/animation.rs`,
  `tests/readalpha.vpy`, `tests/pngwrite.vpy`, `docs/BENCH.md`
- expected: the decode stage of every format this tree reads itself 2x to 4x
  faster, from one shared interleave kernel and four algorithmic changes, with
  no new dependency and no change to a single output byte
- risk: medium. It moves where every sample of those formats is written, so it
  ships with the parity gates that already exist and a kernel-versus-scalar
  unit test per kernel, the way `src/convert/simd.rs` does

## problem

The decoders that this tree owns are still loops over one pixel. `bmp.rs`,
`tga.rs`, `dds.rs` and `pnm.rs` are ports of `image`'s readers and keep its
shape: a per-pixel arm, a per-channel widening, a `copy_from_slice` per pixel.
`hdr.rs` builds a 12 byte a pixel `f32` picture and then walks it three times.
`pixel.rs` writes an interleaved buffer to planes one channel at a time, which
reads every byte once per plane.

The build already has two vector variants (`.avx2` for `x86-64-v3`, `.avx512`
for `x86-64-v4`), so plain generic Rust in those two libraries *is* compiled with
AVX2 or AVX-512 available. That helps a contiguous loop and does nothing for a
scatter, which is why the per-pixel arms are still per-pixel. The one hand
written kernel this tree has is the yuv to r,g,b conversion in
`src/convert/simd.rs`, and that is the shape every kernel below should copy: a
runtime `is_x86_feature_detected!` probe, `#[target_feature]` kernels, NEON
unconditionally on aarch64, the scalar loop as both the oracle and the tail.

## evidence

### where a frame's time goes today

`target/bench/plan48/measure.py` reads the `debug=1` stage log
(`decode= open= metadata= buffer= read=` from `src/source.rs`, plus the
`allocate`/`convert`/`properties` of the frame the worker builds). 1200x900
corpora, `target/bench/step5-corpus`, 8 frames a set, `prefetch=0`, mean of 5
runs, milliseconds a frame. `base` is `target/release/vs_imageseqs.dll` (no
`-C target-cpu`), `v4` is the same tree built with `x86-64-v4`, which is what
the wheel ships.

| set | decode base | decode v4 | convert base | convert v4 | total v4 |
| --- | ---: | ---: | ---: | ---: | ---: |
| bmp 24-bit | 3.379 | 2.761 | — | — | 2.857 |
| dds DXT5 | 2.974 | 3.642 | 1.372 | 0.951 | 5.583 |
| tga 24-bit | 2.675 | 1.502 | — | — | 1.605 |
| pnm `P6` | 2.716 | 1.664 | — | — | 1.792 |
| qoi | 5.467 | 5.577 | 1.211 | 0.760 | 7.107 |
| hdr | 8.072 | 7.403 | 3.197 | 4.063 | 13.389 |
| farbfeld | 4.471 | 3.007 | — | — | 3.115 |
| tiff | 5.728 | 5.258 | 1.159 | 0.695 | 6.619 |
| exr | 15.048 | 14.146 | 2.226 | 2.238 | 17.133 |
| ico (a 256x192 page) | 0.161 | 0.157 | 0.046 | 0.017 | 0.263 |

`open` is zero for most of these because the row streams read inside `fill`, and
exr carries its whole 14 ms in `open` because the `exr` crate reads the file
itself. Three things follow from the table:

1. **The vector variants are worth 1.5x to 1.8x on the contiguous decoders**
   (tga 2.675 → 1.502, pnm 2.716 → 1.664, farbfeld 4.471 → 3.007) and nothing at
   all on the scattered ones (bmp 1.22x, dds 0.82x, tiff 1.09x).
2. **`dds` is 22% slower in the shipping build than in the baseline build**
   (3.642 against 2.974), reproducibly over two rounds of the same run. That is
   the auto-vectoriser making `write_block`'s per-pixel slice copy worse at
   `x86-64-v4`, not a kernel doing anything. Anything measured below is measured
   against the `v4` column, because that is the library a user loads.
3. **The deinterleave is a visible share of a frame** wherever the decoder hands
   out an interleaved buffer: 1.0 to 1.4 ms for dds/qoi, 0.7 to 1.2 ms for tiff,
   2.2 ms for exr, and 3.2 to 4.1 ms for hdr, which is a third of its frame.

the animation path, same method, on a 1024x1024 canvas (`target/bench/apng-corpus`
for apng, `target/bench/plan48/anim-corpus` for the gif and the animated webp):

| set | fetch base | fetch v4 | convert base | convert v4 |
| --- | ---: | ---: | ---: | ---: |
| apng | 5.657 | 4.848 | 1.827 | 1.197 |
| gif | — | 5.888 | — | 1.004 |
| animated webp | — | 8.337 | — | 1.355 |

### what a kernel is actually worth

`target/bench/plan48/proto` prices the two families before the plan asks for
them. Both are compiled the way the baseline library is, so the comparison is
against the scalar code the tree ships in that library.

| kernel | today's shape | candidate | gain |
| --- | ---: | ---: | ---: |
| 3 channel `u8` deinterleave, 1.08 Mpx | 0.806 ms | 0.202 ms AVX2 | **4.0x** |
| 4 channel `u8` deinterleave | 0.834 ms | 0.236 ms AVX2 | **3.5x** |
| the same written as one scalar pass | 0.943 ms | — | 0.86x, i.e. **worse** |
| DXT5 block into a 1200x900 surface | 1.633 ms | 1.057 ms AVX2 | **1.55x** |

Two lessons the plan is built on. The deinterleave win is the **vector**, not
the pass count: moving from one read per plane to one read in total, in scalar
Rust, is slower than what the compiler already does with the strided gather. And
the DXT5 block gains only 1.55x from the vector alone, because the identical
work also sits in the palette and the alpha level tables, which the prototype
left scalar.

The last row also prices the real defect. The prototype decodes 1.08 Mpx of
DXT5 in 1.633 ms, and `dds.rs` takes 2.974 ms in the *same* build, before the
1 MB read and the 4.3 MB buffer are counted. Roughly 1.8x of that stage is the
shape of the code rather than the arithmetic in it: a `[[u8; 4]; 16]` array per
block, then sixteen bounds-checked `copy_from_slice` calls. The kernel is worth
having second, not first.

### what must not be hand-written

Two of the three x86-64 libraries compile the generic loops with a wide ISA, so
these are already done and a kernel for them would be wasted work:
`pixel.rs`'s `copy_samples` with a non-zero shift (a `vpsrlw`), every whole-row
`copy_from_slice`, `fill_plane` and `fill_alpha_plane` (a broadcast), hdr's RLE
runs (`copy_from_slice` and `fill` are already the widest store the machine
has), and pnm's ASCII raster, which is a parse rather than a loop over samples.
`place_rgba8` is a stride of four, which is a case the interleaved-access
vectoriser sometimes takes, so it is measured before it is written.

## the work

Six steps, in the order they should land. The first is the one that pays for the
most formats, and the last three are each independent of the others.

### 1. one interleave kernel, four call sites

`src/decoder.rs:227` (`RowSink::place_rgb8`), `:255` (`RowSink::place_rgba8`),
`:301` (`place_bgr8`), `:335` (`place_rgba16`) and `src/pixel.rs:752`
(`extract_channel`) are the same loop written five times. The kernel is a byte
shuffle a channel: a block is as many pixels as one register of source bytes
holds, which the prototype measured as 8 pixels for three channels (24 bytes)
and 8 for four (32 bytes) on AVX2, twice that on AVX-512 and half on NEON, and
the pixels a block cannot cover are the scalar loop's. `place_bgr8` is the same
kernel with the shuffle's first and third control bytes swapped, and
`place_rgba16` is the same kernel with a byte swap folded into the mask, so it
is one kernel with a channel count and a swap, not five to keep in step. The
new module is `src/planar.rs`, a sibling of `src/convert/simd.rs` and dispatching
the same way.

Reached by every row-stream reader (`bmp`, `tga`, `png`, `pnm`, `farbfeld`) and
by `extract_channel`, which every buffered reader goes through (`jpeg`, `jxl`,
`webp`, `qoi`, `hdr`, `dds`, `tiff`, `exr`, `apng`). From the prototype table:
**4.0x on the loop, 1.3x to 2.5x on the frame** of the formats whose decode is
mostly this loop, and 0.3 to 0.5 ms off the `convert` stage of every format that
buffers.

### 2. dds: the block, as a block

Three changes, cheapest first.

- `write_block` (`dds.rs:458`) writes a row at a time. A block's four pixels are
  contiguous, so the whole-block arm becomes four 12-byte or four 16-byte stores
  instead of sixteen 3 or 4-byte bounded copies. That arm is already chosen by a
  whole-block test (`dds.rs:466`) that must stay exactly where it is, because
  the clipped blocks at a surface's right and bottom edge write fewer pixels and
  a widened store is precisely how the next row gets corrupted.
- the widening of the two endpoints (`dds.rs:282`) and the two lerps
  (`dds.rs:326`) become table lookups and multiply-highs. `dds::widen` truncates
  and `bmp::expand` rounds, and `dds.rs:609` asserts both in one test so they
  cannot be merged by accident. The `/3` replacement `(n * 0xAAAB) >> 17` is
  exact for every `n` a block can produce (at most 766), and that is proven
  beside that test rather than asserted.
- the sixteen two-bit colour indices expand through one `pshufb` per four
  pixels, from a 256-entry control table built once, and the DXT5 alpha indices
  gather from their eight-entry table with a second `pshufb` whose control bytes
  are the indices themselves. This is the prototype's 1.55x, and it grows once
  the palette tables above stop spending six divisions a block.

Expected **2.5x to 3.5x on the dds decode stage**, which is 3.6 ms to about
1.2 ms a frame at 1.08 Mpx. DXT1 and the colour half of DXT3/DXT5 share the
kernel; DXT3's four-bit alpha is a `pmullw` widening of a nibble.

### 3. bmp and tga: two passes become one

- `bmp.rs:143`'s `expand` becomes a table on the `Bitfield`, filled while the
  header is parsed. `Bitfields16` and `Bitfields32` (`bmp.rs:868`, `:880`) call
  it with a **runtime** `len`, so today every channel of every pixel is a `u32`
  division. A 256-entry table is byte-identical by construction: it holds the
  results of the expression it replaces.
- `bmp.rs:830`'s `Rgb24` arm only swaps blue and red, which `place_bgr8` does
  while deinterleaving. Deleting that arm deletes a whole per-pixel pass and its
  `line` buffer. The same argument is what `tga.rs:507`'s separate swap pass
  needs, and `tga.rs:485` and `:140`'s 5-5-5 arithmetic become the 32-entry
  table `bmp::expand` already implies.
- a palette page widens its entries once, at parse, instead of a `palette.get`
  with an `Option`, a branch and a `x * channels` multiply per pixel.

Expected **1.6x to 1.9x on a 24-bit bitmap** (2.76 → ~1.5 ms), and 2.5x to 4x on
a bitfield page, which the step-5 corpus does not have but the fixtures do.

### 4. hdr, exr, tiff: the algorithmic three

These are not kernel work and they are the largest single row in the table.

- `hdr.rs:396`–`:423` builds a 12 byte a pixel `f32` picture and scatters into
  it, recomputing all three flip and transpose branches inside the inner loop.
  Per-plane pointers with the row targets hoisted out, exactly what `exr.rs:305`
  already does, removes both the intermediate picture and the three passes that
  read it back. `hdr` is 13.4 ms a frame and 3.2 to 4.1 ms of that is the write.
- `exr.rs:305` rebuilds a `checked_mul`/`checked_add` offset chain per channel
  per pixel. The size check that refused the file at probe time already answers
  the question that chain is asking, so the plane offsets move out of the loop.
- `tiff.rs:1276` interleaves what `tiff.rs:1310` (`planes_in_place`) can already
  hand over untouched, and `tiff.rs:1179` (`expand_narrow`) does two divides per
  sample for a divisor the pixel width makes a constant.

Expected **1.15x to 1.30x on hdr** and 1.05x to 1.15x on exr and tiff. The
`hdr` float conversion (`hdr.rs:257`) stays exactly three operations in the
same order, one multiply a component, because folding `/256` elsewhere or
letting it fuse into an FMA changes the last bit of some values.

### 5. animation: compose the rectangle, not the canvas

The gif LZW walk and the palette expansion are the `gif` crate's, and the png
inflate is the `png` crate's, so neither is in scope for a kernel. What is ours
is the composition around them, and there the win is algorithmic.

- `src/animation/gif.rs:273`–`:291` walks the **whole canvas** and bounds-checks
  every pixel of it for a rectangle that is usually a small patch. Per row inside
  the rectangle, one `copy_from_slice` of `4w` bytes, then blend only those
  bytes. A 1024x1024 canvas with a 32x32 patch goes from a million branchy
  iterations a frame to a megabyte of row memcpy and a thousand blended pixels.
- the blend itself then becomes a select between two four-byte values rather than
  arithmetic: 8 pixels a block on AVX2, 16 on AVX-512, 4 on NEON, with
  `Background` disposing to a zero operand so one kernel serves both arms. The
  two deliberate departures from the specification stay: the background colour
  is never used, and `Any` disposes as `Keep`.
- `src/animation/webp.rs:249` divides by the blended alpha once per pixel
  (`(1 << 24) / blended_alpha`). A 256-entry constant table holds those exact
  quotients, and `src/animation/apng.rs:642`'s `blend` divides by a runtime
  `maximum` four times a pixel, which monomorphising into an 8-bit and a 16-bit
  body turns into a literal, and therefore into the multiply-high LLVM already
  uses for a constant divisor.
- `src/animation/gif.rs:192`–`:203` decodes a single picture **three times** to
  answer a probe and then a frame. The picture the second peek decoded is the
  picture, so one LZW pass replaces three. That is the cheapest real win in the
  whole group and it is on the still path, not the exotic one.
- `Pixels::Interleaved`'s buffer becoming an `Arc<[u8]>` (`decoder.rs:414`)
  removes a full canvas copy from every cached presentation, every held output
  frame and every skipped intermediate: the fixtures show 15 output frames from
  4 pictures, so 11 of every 15 canvas copies are duplication. It is the widest
  change in this step and the one that has to come last, because until the buffer
  is shared by pointer, reusing it breaks the rule that a cached presentation is
  never mutated.

Expected **1.3x to 2.5x on a gif animation** of a large canvas, 1.15x to 1.5x on
apng, and 1.05x to 1.25x on the animated webp, all of it small on the 16x12
fixtures, which is why the measurement has to be a 1024x1024 corpus.

### 6. what this plan does not do

- **no crate, and no feature flip.** Every decoder already runs the widest path
  its crate offers: `zune-jpeg` has the AVX2 IDCT behind `x86` (enabled) with a
  real runtime probe, `jxl`'s `all-simd` is the union of the per-ISA features,
  `hayro-jpeg2000`'s `simd` is on, `wpd`'s assembly is a default feature. There
  is nothing left off that would buy anything.
- **no extraction.** `qoi` is the one crate small enough to port (about 27 KB,
  MIT/Apache, a frozen format) and it would be **no faster**: its loop is the
  reference algorithm and is bound by output traffic. `hayro-jpeg2000`'s outer
  shell is already `jp2.rs`'s, and the 236 KB that remains is 95% bit-plane
  arithmetic, which [43](43-hayro-jpeg-2000.md) measured and
  [44](44-hayro-jpeg-2000-050.md) re-measured at 0.5% to 0.9% for upstream's
  own faster `main`. The png unfilter is 46 KB entangled with a 32 KiB lookback
  buffer and is not the cost, which is what [22](22-png-decode-path.md) found
  when a wider baseline moved `read` by 1 ms. `dav1d` is C and hand-written
  assembly, and `libheif`/`libde265` are LGPL-3.0, so porting either is a
  derivative work that would relicense a MPL-2.0 plugin.
- **no png `unstable`** (a nightly toolchain for all three wheel variants, for a
  filter that is not where a png frame's time goes), **no `zlib-rs` `avx512` or
  `vpclmulqdq`** (compile-time, not runtime-detected, so it would `SIGILL` the
  baseline and `.avx2` libraries), **no `tiff` `webp` or `zstd`** (a second webp
  decoder after [39](39-libwebp-removal.md), and a C build in three variants),
  **no dav1d or wpd thread budget** ([19](19-avif-thread-budget.md) measured
  that), and **no fork of the MQ coder.**
- **no kernel for a loop the compiler already vectorises**, listed above.
- **no forward walk for jxl.** `jxl.rs:246` records that a decoder kept alive
  across presentations was tried and fails on a non-uniform codestream, so the
  per-presentation decoder stays. It is a measurement question, not a fix.
- **no housekeeping in this plan.** `wpd`'s `threads` feature is inert because
  the plugin passes `n_threads: 1`, and dropping it would be byte-identical and a
  few KB lighter, but it is not a speed change and it belongs in its own
  commit. The same is true of the `AGENTS.md` sentence that says `zlib-rs`
  inflates every png the reader decodes: `png 0.18.1` inflates through
  `fdeflate`, and that sentence should be corrected when this work lands.
- **no change to what a reader checks.** A kernel that stops validating a row's
  length is how the guarantees `png.rs:671` and `pixel.rs:1313` were added get
  lost.

## decisions this plan makes

**the vector is worth more than the pass count.** the prototype's one-pass
scalar deinterleave is slower than the strided gather it would replace, and both
are beaten by the vector. So each step here either adds a kernel or removes a
pass *and* adds a kernel, and no step is justified by pass counting alone.

**`dds` is written before it is vectorised.** its current shape costs about 1.8x
the same arithmetic written plainly, before any kernel, so the row stores and the
tables come first. A kernel on top of the current shape would hide the defect
rather than remove it.

**the pnm 16-bit rescale is not converted to a multiply-high.**
`(f32::from(v) * factor).round()` is ties-away-from-zero on a rounded `f32`
quotient, so a vector form is admissible only with an exhaustive check over all
65536 inputs for the file's `maxval`. The 8-bit case is a 256-entry table holding
that expression's own results, which is provably exact, and it is the only form
this plan takes.

**the measurement is against the shipping build.** the `.avx512` library is what
a user loads, and it is 22% slower than the baseline library on dds today. Every
row in the acceptance table is therefore a `v4` build against a `v4` build, and a
kernel that only wins in the baseline library is not a win.

## acceptance

- `cargo test --locked`, `cargo clippy --all-targets --locked -- -D warnings`,
  `cargo fmt --all -- --check` clean.
- one unit test per kernel holding it to the scalar loop **sample for sample**,
  the way `src/convert/simd.rs` already is, plus the kernel-by-name seam that
  lets a test run a kernel the dispatch on this machine would not pick.
- `tests/readalpha.vpy` and `tests/pngwrite.vpy` pass, with the log free of
  warnings.
- `target/bench/frame-parity.py` run under the baseline and the candidate
  library over every set below: **0 differing lines**, for both clips.
  `target/bench/row-sink-parity.py` for the row streams, `dds-odd-check.py` for
  the odd surfaces, and `ab-simd-bytes.py` for a two-build byte comparison.
  The sets are `target/bench/step5-corpus` (one file a format),
  `target/bench/dds-odd` (six odd block surfaces), `target/bench/lowbit-gray`
  (the two narrow gray tiffs), `target/bench/png-edge`, `target/bench/apng-corpus`,
  `target/bench/plan48/anim-corpus`, and `tests/fixtures` (270 files, including
  the seven dds, fifteen bmp, fifteen tga and four ico fixtures whose module
  tests are each format's oracle).
- `target/bench/plan48/measure.py` re-run per build: every set in the evidence
  table has a faster decode stage, `convert` is faster wherever step 1 matters,
  and **no set is slower**. The dds, bmp, tga, pnm, farbfeld and hdr rows are the
  ones this plan is claiming; the animation corpus is where step 5 shows.
- `docs/BENCH.md` gains a `decoder` section with the before and after of that
  table, and `AGENTS.md`'s `src/formats/` paragraph names `src/planar.rs`.

## how to measure

```powershell
# the DLL cannot be loaded from python's own plugin directory: the installed
# wheel shadows it, so measure.py disables auto-loading, like decode-time.py.
.\.venv\Scripts\python.exe target\bench\plan48\measure.py `
  target\x86_64-pc-windows-msvc\release\vs_imageseqs.dll `
  target\bench\step5-corpus "*.dds" 5
```

```powershell
# the prototype, which needs no vcpkg and no VapourSynth
cargo run --release --manifest-path target/bench/plan48/proto/Cargo.toml
```

a kernel's own throughput is measured the way the conversion's is:
`cargo test --release --lib -- --ignored --nocapture the_kernels_measured`.

## what is left over

the jxl decode path is 381 ms a frame serially and has never been profiled, and
qoi has no `docs/BENCH.md` row at all. Both are measurements to take, not
features to flip, and neither is in this plan. `target/bench/plan48/{blocks,
planes,animation,crates}.md` are the four read-only audits this page is written
from, and they hold the per-loop inventory (anchors, costs, and the parity
fixtures) that each step's implementation should start from.
