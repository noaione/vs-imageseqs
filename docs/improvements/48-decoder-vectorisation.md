# 48 — a faster decode path for the formats this tree decodes itself

- status: partially implemented. The kernel step was implemented on x86-64,
  measured slower than the code it replaced and reverted, and the record is
  below; it was then asked again on aarch64, where the eight bit kernel still
  loses to the compiler's own `ld3`/`ld4` but the sixteen bit big-endian one
  wins, and that round landed `src/planar.rs`: 3% to 7% off a sixteen bit
  png, 9% off an eight bit rgba page and 29% off a farbfeld. The gif compose
  and its NEON blend landed too: 1.27x on a 1024x1024 animation and 1.14x on a
  still gif. The `bmp`,
  `dds`, `hdr`, gif-still and apng-blend steps landed: 1.33x to 1.40x on the
  bitmap corpus, 15% off the surface corpus's decode stage, 0.82x to 0.85x on
  a radiance frame, 0.96x on an exr frame, 1.64x to 1.89x on a one-picture gif,
  and an apng blend whose divisor is now free (that one did not move a frame).
  All byte identical and both validators passing. `tiff` was investigated and
  left alone; the `tga` half landed with a corpus of its own; the compose kernels
  are gif's arm blend and the sixteen bit apng blend, and the eight bit apng
  blend was already vector, so the only vector half that stays unwritten is the
  webp reciprocal, which has no vector form at 41 bits a product.
  The buffered write landed last: one pass
  over the decoder's buffer instead of one a plane, 0.62x to 0.82x on the
  `convert` stage. The shared buffer landed too: `Pixels::Interleaved` holds its
  canvas in an `Arc<[u8]>`, which is 2% to 4% on an animation's `fetch` (the
  copy it removes was real but small) and is what lets a cached presentation be
  handed out without a copy at all. The Windows host round then answered the
  four questions this page left to it: the compose row's AVX2 and AVX-512
  blocks were written, measured at the memory system's ceiling and **declined**;
  the one-pass write was confirmed a win on both x86-64 libraries (1.03x to
  1.40x of the per-plane gather); the sixteen bit big-endian walk was found
  unvectorised there and given an AVX2 block, which is 1.34x on a farbfeld and
  1.16x on a gray16 page; and the arm rows were not re-measured. See [what the
  x86-64 host found](#what-the-x86-64-host-found).
- touches: `src/planar.rs` (new), `src/formats/{dds,bmp,tga,ico,pnm,farbfeld,hdr,png,exr,tiff}.rs`,
  `src/pixel.rs` (`write_planes`),
  `src/decoder.rs` (`RowSink`), `tests/readalpha.vpy`, `tests/pngwrite.vpy`,
  `docs/BENCH.md`, and the two prototype crates under `target/bench/plan48/`
- expected: the decode stage of the formats this tree reads itself faster, from
  four algorithmic changes that need no intrinsic and no new dependency, with no
  change to a single output byte. The first kernel this page proposed was
  implemented, measured slower than the code it replaced, and reverted; that
  measurement is below
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

Four steps, in the order they should land, plus one that the first round of
implementation refuted and this page keeps as the record. The three middle ones
are independent of each other.

### 0. the interleave kernel — measured, not worth it

**This was the plan's first step, and it was implemented, measured and
reverted.** It is recorded here because the numbers matter to whoever reads
this next.

`src/decoder.rs:227` (`RowSink::place_rgb8`), `:255` (`place_rgba8`), `:301`
(`place_bgr8`) and `src/pixel.rs:752` (`extract_channel`) are one loop written
several times, and the prototype priced a vector version of it at 4.0x over a
*scalar* gather. A `src/planar.rs` was written with three kernels (AVX-512BW for
the word-aligned strides, AVX2 for the rest, NEON for aarch64), a runtime probe
per `src/convert/simd.rs`, and the scalar loop as the tail and the oracle. Both
row-sink and `extract_channel` were rewired to it, and the parity was exact: the
unit test holds every stride, channel and length byte for byte.

It was slower than what the tree already does, so it is gone. Measured by
alternating two builds inside one session (`target/bench/plan48/ab.py`, best of
five rounds, the only way on a machine whose run-to-run spread on these stages
is 40%):

| stage | `v4` build | baseline build |
| --- | ---: | ---: |
| `dds` convert, 4 channels | 1.07x **slower** | 0.66x, i.e. 1.5x faster |
| `qoi` convert, 4 channels | 0.95x | 0.66x |
| `qoi` convert, 3 channels | 0.95x | 1.55x **slower** |
| `tiff` convert, 3 channels | 0.99x | — |
| `bmp` read (row sink) | 1.00x | 1.01x |

Four things came out of it, and they are why the rest of this page is
algorithmic work rather than kernel work:

1. **the wide builds already vectorise this loop.** A hand kernel has to beat
   the compiler's own 512-bit loop in the library a user with AVX-512 loads, and
   it did not: the shuffle-and-store a byte gather needs is what LLVM emits
   from the stride-4 loop already. The prototype's 4.0x was against a scalar
   loop this tree does not run in those builds.
2. **a kernel is not portable between the builds that ship.** The choice is
   made at run time, so a host with AVX-512 runs the 512-bit kernel even when it
   loaded the baseline library, and a host without it runs the 256-bit kernel
   inside a build whose own loops may be wider. That is a tax on every host
   that is not the one the kernel was tuned on.
3. **the row sinks were already one pass.** `place_rgb8` splits a row into three
   planes in one walk, and the compiler vectorises that walk: replacing it with
   three kernel calls was `1.00x` at best.
4. **a stride of three is the case no kernel wins.** Three bytes a pixel puts a
   channel's byte at a different offset in every lane, so the gather needs four
   shuffles and a combine where the compiler's loop is one wide shuffle. It is
   also the stride `qoi`, `jpeg`, `jxl`, `hdr` and most `tiff` pages use, which
   is most of the traffic.

What that leaves is the arithmetic around the loop: the tiles that are read
with a division per sample, the pictures that are built and then walked a second
time, and the composites that walk a canvas to blend a patch.

### 0b. the interleave on arm — the swap is what pays

The arm round re-asked step 0's question on the one library an arm host loads.
`target/bench/plan48/proto-arm/` prices the three shapes the tree runs at
1200x900, best of seven rounds of 32 passes, ns a pixel:

| shape | per-channel gather (today) | one pass scalar | NEON block |
| --- | ---: | ---: | ---: |
| gray+alpha 8 bit | 0.523 | 0.042 | **0.028** |
| rgb 8 bit | 0.792 | **0.060** | 0.101 |
| rgba 8 bit | 1.057 | **0.080** | 0.086 |
| gray+alpha 16 bit big endian | 0.609 | 0.100 | **0.078** |
| rgb 16 bit big endian | 0.943 | 0.609 | **0.210** |
| rgba 16 bit big endian | 1.279 | 0.821 | **0.190** |

Three things follow, and they are the three the x86 round found in a different
order:

1. **the per-channel walk is the bug, wherever it is.** png's general path
   read the row once a channel with a runtime stride and, for sixteen bits,
   built and copied a two byte array a sample. The one pass is 12x to 13x
   that loop for an eight bit page and 6x for sixteen bit gray+alpha; for
   sixteen bit rgb and rgba the one pass alone is only 1.5x, because the
   scalar swap is slow as well.
2. **the eight bit vector is not worth it for three and four channels.** LLVM
   already turns the one-pass zip into `ld3`/`ld4` and three or four stores,
   in a tighter loop than a hand block: 0.060 and 0.080 against 0.101 and
   0.086. Only two channels gain, 0.028 against 0.042, and only because a
   `ld2` pair is cheaper than the walk's two streams.
3. **the sixteen bit big-endian vector is worth it.** The swap defeats the
   compiler's vectoriser, so the scalar one pass is still 0.61 to 0.82 ns a
   pixel and `ldN` + `rev16` is 1.3x to 4.3x that.

`src/planar.rs` is the result: one pass, a plane count at a time, with the
aarch64 kernel for the two shapes that measured a win (`ld2` at eight bits and
`ldN` + `rev16` at sixteen) and the scalar zip everywhere else. `png.rs`'s
channel walk and `RowSink::place_rgba16` (farbfeld) both call it;
`src/decoder.rs` gained `place_interleaved` for the shape a png row has, which
writes only the planes a call hands out, because a colour-only read drops the
alpha channel.

Measured end to end against a build of the same commit without the change,
alternating two plugins inside one batch (`target/bench/plan48/ab.py`, best of
five rounds, `prefetch=0`, `debug=1`):

| set | read before | read after | total before | total after | change |
| --- | ---: | ---: | ---: | ---: | ---: |
| png rgba16 | 16.56 | 15.38 | 16.65 | 15.47 | **0.93x** |
| png rgb16 | 15.48 | 14.50 | 15.57 | 14.59 | **0.94x** |
| png la16 | 8.84 | 8.56 | 8.90 | 8.62 | 0.97x |
| png gray16 | 5.87 | 5.57 | 5.92 | 5.62 | 0.95x |
| png rgba8 | 8.53 | 7.77 | 8.59 | 7.83 | **0.91x** |
| png la8 | 3.44 | 3.19 | 3.48 | 3.23 | 0.93x |
| png rgb8 | 4.89 | 4.88 | 4.94 | 4.94 | 1.00x |
| png gray8 | 2.21 | 2.20 | 2.24 | 2.23 | 1.00x |
| farbfeld | 1.77 | 1.25 | 1.81 | 1.28 | **0.71x** |

The png corpus is `target/bench/png-depth`, eight 1200x900 pages of each
colour type from `target/bench/make-png-depth-corpus.py`; step5's corpus has
no png page. The sixteen bit rows are inflate bound, which is why a 4x loop is
3% to 7% of a frame; farbfeld has nothing to inflate, so the same kernel is
29% of its frame.

Gates: every plane of every frame identical on `png-depth`, `step5-corpus`,
the 40 png fixtures and the farbfeld fixture, both clips, under both builds;
`tests/readalpha.vpy` 1034 checks and `tests/pngwrite.vpy` 311, no warnings;
373 unit tests, clippy and fmt clean. The untouched formats were swept the
same way and none moved beyond noise (bmp 0.99x, dds 1.00x, ppm 0.99x, qoi
0.99x, tiff 1.00x, exr 0.98x, hdr 1.00x, ico 1.03x at 0.12 ms a frame).

**the x86 build is not measured here.** The one-pass scalar replaced a
per-channel gather that is worse on every architecture, but the shipping
`.avx2` and `.avx512` libraries should be A/B'd on a Windows host the way
step 0's table was before that can be claimed as a win there. The arm
kernel's own dispatch is a `#[cfg]`, so both x86 libraries keep the scalar
walk unchanged.

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

**landed, first half.** The row store is in: a block's four pixels are
contiguous, so a row is one 12 or 16 byte store rather than four of 3 or 4. On
`target/bench/step5-corpus`'s DXT5 pages the decode stage went 3.629 → 3.084 ms
at `x86-64-v4` (0.85x) and 3.103 → 3.010 ms on the baseline build, and the
picture is byte identical on the corpus and on the six odd surfaces of
`target/bench/dds-odd`. The colour kernel above is not written: the palettes and
the index gather are already multiply-highs, and the measurement to justify it
is the one this page's step 0 says has to beat the compiler's own loop.

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


**landed, the bitmap half.** `Rows::fill` hands a 24-bit or a 32-bit row to
`place_bgr8`/`place_rgba8` directly, and `Bitfield` carries a widening table
built once per width. Measured on `target/bench/step5-corpus`'s 1200x900
24-bit pages, best of four alternating rounds: the decode stage went 4.721 →
3.551 ms on the baseline build and 3.911 → 2.796 ms at `x86-64-v4`, which is
**1.33x to 1.40x on the frame**, and every plane of every frame of both clips
is byte identical. The bitfield table is not on that corpus (it has no bitfield
page), so its share is untested; it is kept because it removes a `u32` division
per channel per pixel and cannot change a byte.

**Landed, the targa half, and the corpus was the problem.** The pages this
tree held were top-down 24-bit, which the row sink takes, so the buffered
paths were never measured. `target/bench/make-tga-corpus.py` writes the four
shapes that keep the buffered path — raw bottom-up, RLE, sixteen bit 5-5-5 and
an eight bit palette — 32 pages of 1200x900, and the four are very different:
the two paths that build a sample at a time (`expand`'s 5-5-5 arm and the
colour map) now write r,g,b themselves instead of leaving blue first for a
second pass, which is **2.24x** the read on the 16-bit pages and **1.80x** on
the palette ones, while the plain memcpy paths are unchanged (0.542 → 0.542 ms
bottom-up, 3.779 → 3.805 RLE, inside noise) because a fused byte copy of that
shape measured 20% slower than memcpy plus the vectorised swap. The whole
buffered corpus went 1.852 → 1.496 ms (**1.24x**), every page byte identical
under both builds, and the fifteen fixtures and the eight row-sink pages too.
The 32-entry widening table this page proposed is not written: the `5` in
`bmp::expand(value & 0b1_1111, 5)` is a literal, so the divisor is already a
constant and LLVM already emits the multiply-high. The win was the pass, not
the table.

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


**landed, the hdr third.** `raster` now writes three `f32` planes instead of a
12 byte a pixel picture, and a stored row that is not transposed moves into its
output row as three contiguous runs of the scanline rather than an index and a
bounds check a pixel. Measured on `target/bench/step5-corpus`'s pages, best of
four alternating rounds: the `convert` stage went 6.488 → 4.068 ms at
`x86-64-v4` and 6.083 → 4.088 ms on the baseline build, and the frame
17.679 → 14.447 ms (**0.82x**) and 18.521 → 15.737 ms (**0.85x**). Every plane of
every frame of the corpus and of the nine `tests/fixtures/hdr-*.hdr` pages is
byte identical, which includes the four orientation fixtures whose scanlines are
stored turned round.

The `exr` and `tiff` thirds are not written yet. The `hdr` float conversion
(`hdr.rs:257`) still does exactly three operations in the same order, one
multiply a component, because folding `/256` elsewhere or letting it fuse into
an FMA changes the last bit of some values.
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
  quotients, and `src/animation/apng.rs:642`'s `blend` divided by a runtime
  `maximum` four times a pixel. **Landed, the apng half**: `blend` is now
  `blend8` and `blend16`, each with its own depth's maximum as a literal, so the
  division is the multiply-high LLVM already emits for a constant divisor. The
  arithmetic and its rounding are untouched, and the picture is byte identical
  over the 240 frames of `target/bench/apng-corpus` and the 16-bit page. The
  **frame did not move** (0.99x on `fetch`, within this machine's noise) because
  a blend is not what a frame of that corpus spends its time in: it is mostly
  deflate and full-canvas disposal copies. It is kept because the divisor is
  now free and no byte changes, not because it was measured to pay.
  **Landed, the webp half**: the reciprocal is a 256-entry table, which is the
  division entry for entry, and a blend cannot index the one entry a division
  cannot produce. On `target/bench/plan48/anim-corpus`'s 1024x1024 webp the
  frame went 1.92 → 1.64 ms (**0.85x**, 1.18x faster) on a build of the same
  tree with and without the table, and every frame of that file and of
  `tests/fixtures/animation.webp` is byte identical. The vector half of
  `over` is not written: the table already removed the division, which was the
  expensive step, and the rest is three multiplies, a rounding shift and a
  per-lane divisor table lookup that a vector would have to gather -- a small
  slice of the loop at a real risk of moving a byte.
- **Measured, the two vector halves that are not written.** The eight bit
  blend is already the compiler's own NEON: `blend8` disassembles to `dup.4s`,
  `umull.2d`, `ushll`, `xtn` and `st1.s` -- four samples a vector -- so a hand
  kernel would be that loop with more code around it, and it is not written.
  The sixteen bit one was scalar and now has a kernel: a four sample block
  held as `u32` lanes, `vmull_u16` for each side of the mixture, and the
  division by `65535` as the multiply-high its own shape allows
  (`x + 1 + (x >> 16)` shifted down sixteen, exact for every numerator the
  blend can produce because the largest is `65535 * 65535`). The corpus is
  `target/bench/apng16-corpus`, which `target/bench/make-apng16-corpus.py`
  writes by hand because neither Pillow nor ImageMagick writes an APNG: two
  1024x900 sixteen bit RGBA pages of four frames, three of them `over` with a
  quarter, a half and three quarters coverage. `fetch` went 6.26 → 6.08 ms
  (**2.9%**) and every frame of both pages, of the 80 frame eight bit corpus and
  of the 121 animation fixture frames is byte identical. Only 2.9% because
  `blend16` is called a pixel at a time with that pixel's own alpha, so a
  block is four samples and half the work is the setup; one blend is 1.92 ms
  of the 15.6 ms a presentation costs and three of four frames blend, so the
  whole blend is 9% of a file's `fetch` and a row kernel that took the source
  alpha channel as the multiplier could have most of it. That is not written:
  sixteen bit APNG is the rarest shape this tree reads, and 9% of it is under
  what a restructure of the compose loop costs to review. The vectors of
  `webp.rs`'s `over` are the same story with a hard floor: the product
  `blended * scale` is up to 41 bits, so a lane holds one sample at 64 bits and
  two samples a vector, and the divisor is a table gather that NEON has no
  instruction for.
- `src/animation/gif.rs:192`–`:203` decoded a single picture **three times** to
  answer a probe and then a frame. The picture the peek decoded is the picture,
  so one LZW pass replaced three. **Landed**: a one-picture 1024x1024 gif went
  20.912 → 11.084 ms a frame at `x86-64-v4` and 19.632 → 11.939 ms on the
  baseline build, which is **1.64x to 1.89x**, and every frame of the still gif,
  of `tests/fixtures/animation.gif` and of the 55 gif frames in that directory is
  byte identical.
  **Landed, the compose and its arm blend.** `Canvas::compose` walks only the
  rectangle's rows and columns now, one `copy_from_slice` a row, and the
  per-sample `blend` became `blend_row`: the disposal branch hoisted out of the
  pixel loop, then on aarch64 a `vbslq_u32` select four pixels a block with
  the alpha byte of every pixel as its mask, and `Background` disposing to a
  zero operand so one kernel serves both arms. `place`, the still path, got
  the same row walk. On a 1024x1024 canvas the compose loop went 0.897 →
  0.424 → 0.237 ms for a full-canvas `Keep` frame and 0.912 → 0.425 → 0.188 ms
  for a `Background` one, which is 3.8x to 4.9x; a 32x32 patch is clone-bound
  at 0.068 → 0.057 ms, so the corpus frames being full-canvas is why the
  kernel matters more than the rectangle there. End to end on
  `target/bench/plan48/anim-corpus`: the gif animation's `fetch` went 2.00 →
  1.57 ms a frame (**0.78x**, 1.27x faster) and the still gif 4.45 → 3.89 ms
  (**0.88x**), with every plane of every frame of both clips byte identical
  under the two builds and `tests/readalpha.vpy` at 1034 checks. The x86-64
  libraries keep the scalar row, which is itself 2.1x the old per-pixel walk;
  the AVX2 and AVX-512 blocks the section above proposes are not written.
- `Pixels::Interleaved`'s buffer becoming an `Arc<[u8]>` (`decoder.rs:414`)
  removes a full canvas copy from every cached presentation, every held output
  frame and every skipped intermediate: the fixtures show 15 output frames from
  4 pictures, so 11 of every 15 canvas copies are duplication. It is the widest
  change in this step and the one that has to come last, because until the buffer
  is shared by pointer, reusing it breaks the rule that a cached presentation is
  never mutated.
  **Landed**: the buffer is an `Arc<[u8]>`, so `presentation`'s cache hit copies a
  pointer rather than the canvas. Measured on `target/bench/plan48/anim-corpus`
  and `target/bench/apng-corpus` against a build of the previous commit: the
  gif's `fetch` went 1.50 → 1.47 ms, the webp's 1.58 → 1.55 and the apng's
  1.59 → 1.53, which is **2% to 4%** and less than this page expected. The copy
  it removes is about 2 MB an output frame at these sizes, and memory
  bandwidth makes that about 0.05 ms: the duplication was real but never the
  frame's cost. Every frame is byte identical, and the type is what lets a
  cached presentation be shared at all, which is why it had to come last.
  `Pixels::Planar` still owns its planes, so a heif sequence's frames keep their
  copies; that path is rare enough that the same treatment is not written.

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

**on arm the pass count is what pays, and the vector is the swap.** the arm
round found the one-pass walk 12x to 13x the per-channel gather at eight bits
with no intrinsic at all, and the hand block slower than the compiler's own
`ld3`/`ld4` for three and four channels. The kernel is kept for the shapes
where it beats the walk: two eight bit channels and every sixteen bit
big-endian one, where the swap is what stops the compiler vectorising.

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
  table has a faster decode stage, and **no set is slower**. The dds, bmp, tga,
  pnm, farbfeld and hdr rows are the ones this plan is claiming; the animation
  corpus is where step 4 shows.
- `docs/BENCH.md` gains a `decoder` section with the before and after of that
  table. `AGENTS.md` gains the `src/planar.rs` sentence only if a later round
  lands one; the kernel this page proposed did not.

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

the compose row is priced the same way, and its canvas sizes are what separate a
block's cost from a frame's:

```powershell
cargo test --release --lib -- --ignored --nocapture the_compose_measured
```

## what the x86-64 host found

Every row this page recorded before this round was measured on one arm64 Mac.
The two x86-64 libraries the wheel ships (`.avx2` for `x86-64-v3`, `.avx512`
for `x86-64-v4`) were re-asked on a Windows host, an i5-11400H under Windows 11
with rustc 1.99. Three of the four questions are now answered, two by
measurement and one by the generated code; the fourth, re-measuring the arm
rows, was not taken. Each item below says which.

### 1. the gif compose's AVX2 and AVX-512 row blocks: declined

The blocks were written, held to `blend_row_scalar` sample for sample by a
forced test, dispatched the way `src/convert/simd.rs` dispatches, and measured
at three canvas sizes in all three libraries. [BENCH.md's compose row
section](../BENCH.md#the-compose-row-on-x86-64) has the tables. The short of
it:

- the wide builds already vectorise the row. At 1024x1024 the compiler's own
  loop costs 0.61 ns a pixel in the `.avx2` library and 0.71 in the `.avx512`
  one, and the best block costs 0.59 to 0.63 there; the only library where a
  block wins is the baseline one (2.0x), which is step 0's result asked again.
- at 256x256, where a canvas sits in cache, both blocks win 1.2x to 2.6x. At
  4096x4096 everything is 1.0x to 1.1x. A canvas is twelve bytes a pixel of
  traffic and this host's single-core ceiling is 19 GB/s, which the scalar walk
  already reaches: on x86 the arithmetic is not what a row costs.
- end to end the gif animation does not move (4.922 to 4.930 ms a frame on the
  baseline library across ten alternating rounds), while a control build that
  skips the blend altogether saves 0.32 ms. The blend is 12 MB of traffic a
  presentation, and a block cannot remove that.

This host's spread on that row between runs is up to 40%, so each cell above is
one run and the ratio to trust is the end-to-end one. So the blocks are reverted.
`src/animation/gif.rs` keeps the arm kernel and
gains the ignored `the_compose_measured` test that prices this question, and
this page's own rule, *land the block only where it wins*, is what declined
them.

### 2. the one-pass `write_planes`, and the targa fold

The one pass wins on both x86-64 libraries: 1.09x and 1.40x on a qoi's
`convert` stage, 1.03x and 1.22x on a tiff's, 1.09x and 1.21x on an rgba8
apng, at the baseline and `x86-64-v4` respectively, best of five alternating
rounds ([BENCH.md, the buffered write on
x86-64](../BENCH.md#the-buffered-write-on-x86-64)). That is less than the arm64
round's 1.2x to 1.6x and never nothing, so the pass stays, and the wide
builds' own `place8`/`place16` vectorise it: a `write_planes::<u16, 4>`
monomorphisation carries 48 `pshufb`.

The targa fold holds too. `target/bench/make-tga-corpus.py` now writes the
four buffered shapes this host was missing (32 pages of 1200x900), and against
a build of the same tree with the fold reverted the two folded shapes read
1.59x and 1.64x faster in the baseline library and 1.64x and 1.78x in
`.avx512`, while the two plain-copy shapes do not move (a second look at eight
rounds put them at 1.02x and 0.85x, which is this machine's noise around an
unchanged path). Every plane of every one of the 32 pages is byte identical.
The numbers are in [BENCH.md, the buffered
targa](../BENCH.md#the-buffered-targa).

### 3. `place16be`'s x86-64 half: written and landed

The wide builds did not vectorise the swap: in the `x86-64-v4` build
`formats::png::Rows::<RowStream>::fill`, which is where a sixteen bit page's
row walk is inlined, had no `pshufb` at all, while `planar::place8::<3>` and
`<4>` had 21 and 6. The runtime plane count was what stopped it, and that is
what a block removes.

So `src/planar.rs`'s x86 module carries an AVX2 kernel for one, two and four
channels: a block is one thirty-two byte load, the swap is a shift pair and a
`pshufb` a channel reads that channel's words out with the two lanes joined by
a dword permute. Three channels has no whole block and keeps the whole-row
walk, which is why `place16be`'s x86 arm runs the full scalar walk when the
kernel answers zero rather than the tail loop.

Measured against a build of the same tree without it, best of five alternating
rounds: farbfeld 6.188 to 4.614 ms a frame on the baseline library (1.34x) and
5.676 to 4.093 on `.avx512` (1.39x); a gray16 page 4.319 to 3.734 (1.16x); a
la16 page 1.05x; an rgba16 page unchanged, because a sixteen bit png is
inflate bound; and rgb16, which the block declines, unchanged as the control.
Every plane of every page of the png depth corpus, of the png fixtures and of
the eight farbfeld pages is byte identical. [BENCH.md, the sixteen bit
big-endian walk](../BENCH.md#the-sixteen-bit-big-endian-walk) has the table.

### 4. the rows the arm rounds moved: not re-measured

Those tables still have one host each. Re-measuring them wants the arm rounds'
own before builds and the targa, png depth and sixteen bit apng corpora, none
of which are in this checkout. The animation corpora are, and the gif
animation row is now measured on both hosts; item 2's three sets and the
formats around them are the no-regression sweep this host can vouch for, and
none of them moved.

The gates on that host are the ones this page already lists: `cargo test
--locked`, clippy with `-D warnings`, `cargo fmt --all -- --check`,
`tests/readalpha.vpy` and `tests/pngwrite.vpy`, and the frame parity the
corpora were built for. `docs/HANDOFF.md` has the commands.

## what is left over

the jxl decode path is 381 ms a frame serially and has never been profiled, and
qoi has no `docs/BENCH.md` row at all. Both are measurements to take, not
features to flip, and neither is in this plan. `target/bench/plan48/{blocks,
planes,animation,crates}.md` are the four read-only audits this page is written
from, and they hold the per-loop inventory (anchors, costs, and the parity
fixtures) that each step's implementation should start from.

the `write_planes` half landed too. `src/pixel.rs`'s one pass fills every plane
a non-transposed frame asks for from one read of the buffer, with the mirrors
and the alpha drop folded in, so a buffered decoder's `convert` stage is 0.62x
on a 1024x1024 rgba8 apng, 0.78x on a tiff and 0.82x on a qoi at 1200x900,
and every plane of every frame of every fixture is byte identical. It stops
where the transform transposes or the frame's depth needs a shift, so a 10 or
12 bit jxl still takes the per-plane walk.
The eight bit three and four channel kernel is written and held to the scalar
path by the test but is not dispatched, because it is 1.2x to 1.7x slower
than the compiler's loop. The gif compose's x86-64 blocks were written and
declined, and the shipping libraries have now been measured with the one pass;
both are [above](#what-the-x86-64-host-found). What that host leaves is item 4,
re-measuring the arm rounds' rows on this one, which needs their corpora and
their before builds and is the only thing on the page still unmeasured.

the sixteen bit apng blend is the one kernel whose arm half is a block smaller
than it would like: a pixel is four samples, so the setup is half of it, and the
9% a presentation spends blending bounds a row kernel at 9% of a file's `fetch`
on the rarest shape this tree reads. The webp reciprocal is declined outright:
its product is 41 bits and its divisor is a table gather. Both decisions are in
the animation section above, with their numbers.
