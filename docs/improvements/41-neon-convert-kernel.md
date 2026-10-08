# 41 - a NEON kernel for the yuv conversion

status: implemented. `src/convert/simd.rs` holds `mod neon` beside `mod x86`,
dispatched by `rgb_row` on `target_arch = "aarch64"`, and the kernels are held
to the scalar path by the same test that holds the x86 ones.

## why it is not a variant

[23](23-cpu-variant-avx2.md) and [24](24-cpu-variant-avx512.md) are about
shipping more than one *library*, because x86-64 has levels a binary has to be
compiled for separately: a build with AVX2 in it is not one a processor without
AVX2 can run, so `hatch_build.py` builds one library a level and
`manifest.vs` lets the core pick.

aarch64 is not that. NEON is part of the architecture's baseline that every
aarch64 processor is required to have, so one library carries the kernel, there
is no feature to detect, no second build, and nothing to add to the wheel. The
dispatch in `rgb_row` is a `#[cfg]` rather than a runtime probe, and the
`is_x86_feature_detected!` ladder beside it has no arm counterpart.

## the kernel

The x86 kernels take eight and sixteen columns of a row at a time; a NEON
register is 128 bits, so this one takes four. Everything else is the same
shape, because the file already had the shape: a block loads its lanes, moves
each lane's two chroma taps into place, runs the interpolation, the matrix and
the quantisation together, and returns the range it filled for the scalar path
to finish.

The one part that is not a translation is the tap gather. x86 moves the taps
with `vpermps`, an eight-way permute of a register the load filled. Its arm
counterpart, `vqtbl1q_u8`, looks bytes up in one 128 bit table, which is
exactly the four `f32` lanes a block holds here, so the taps are a byte table
each: `TAPS_DIRECT`, `TAPS_LEFT`, `TAPS_MID`, `TAPS_FIRST` and `TAPS_RIGHT`
name the byte every output lane starts at. Both are one instruction, so the
gather is not what separates two kernels whose blocks are a different width.

The loads are deliberately the block's own width: four bytes read as one word
for an eight bit plane and `vld1_u16` for a sixteen bit one, rather than a load
of eight that would read past the row's last block.

## held to the scalar path

`a_kernel_is_the_scalar_path_sample_for_sample` gained the arm kernel without
losing the x86 ones. The `wide: bool` the forcing hook took is a `Kernel` enum
now, and `simd::KERNELS` is the list the build has -- `avx2` and `avx512` on
x86-64, `neon` on aarch64 -- so the test forces every kernel the host can run
rather than a fixed pair, and still fails if one takes no block of a row wide
enough to have one.

That test is the whole of the correctness argument, and it is worth saying how
it was checked rather than assumed: corrupting one weight in `WEIGHT_LEFT` from
`0.5` to `0.4` makes it fail on the **dispatched** path, which is the NEON
kernel on an arm host. A kernel that was never reached would not have noticed.

## the x86 half is type checked on an arm host

`#[cfg(target_arch = ...)]` means an ordinary `cargo check` on a Mac compiles
none of the x86 tree, so a change that only breaks x86 is invisible there. This
change broke it: `mod x86` imported the test-only `Kernel` unconditionally,
which is an unresolved import in a release build.

`target/bench/x86check/` is the check. It stands `src/convert/simd.rs` up
against the smallest set of types it names -- `Converter`, `Rows`,
`Coefficients`, `Taps`, with the real types' own derives -- and compiles that
for the other target:

```bash
rustc --edition 2024 --crate-type lib --target x86_64-apple-darwin \
  --emit=metadata -o target/bench/x86check/x86.rmeta target/bench/x86check/lib.rs
```

Add `--cfg test` to cover the paths behind `#[cfg(test)]`, which is where
`rgb_row_forced` and `Kernel` live. It checks that the file parses and types
for x86-64, not what it computes; an x86 host running `cargo test` is still what
checks the arithmetic.

## what was measured

`docs/BENCH.md` has the tables. In short: 1.245 ns a luma sample against the
scalar path's 6.16 on a centred 4:2:0 frame, which is 4.9x, and level with AVX2
on three of four shapes and 1.3x it on ten bit 4:2:0. The ratio is much smaller
than x86-64's 12x to 42x for a reason worth keeping in view: arm's scalar loop
is 2.4x to 9x the x86 one's speed before either kernel runs.

The batch is the check that it buys what it is for. Six pages, three routes,
five passes: converting in the writer was **1.23x** the `resize` route on the
scalar library and is **1.01x** on the NEON one, the same "level, not a win"
reading x86-64 reached at 2.51x to 0.98x.

## how to check it

```bash
cargo test --locked
cargo test --release --lib -- --ignored --nocapture the_kernels_measured
.venv/bin/python target/bench/make-ab-corpus.py
.venv/bin/python target/bench/ab-simd-bytes.py \
  --dir target/bench/ab-corpus --pattern "*" \
  --scalar target/bench/baseline-scalar-arm64.dylib \
  --simd target/release/libvs_imageseqs.dylib
IMGSEQS_PLUGIN=$PWD/target/release/libvs_imageseqs.dylib \
  .venv/bin/python target/bench/png-write-yuv.py --limit 6 --passes 5
```

`tests/pngwrite.vpy` is the independent gate: its yuv section compares every
sample of a converted frame with `core.resize.Bilinear`'s own, and reports
"worst 0" on the 4:2:0, 4:2:2 and 4:4:4 fixtures.
