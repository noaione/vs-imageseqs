# 24 - ship an `x86-64-v4` build as an `avx512` variant

status: implemented with [23](23-cpu-variant-avx2.md). this page is the row that
plan's `variants()` carries for `x86-64-v4`, and the measurements below are the
evidence that it is worth a third build.

[32](32-host-safe-cpu-variant-builds.md) fixes building this variant on CI
hosts without AVX512. An explicit Cargo target isolates host build scripts
and procedural macros from the variant's CPU flags.

## the suffix

the file is `vs_imageseqs.avx512.dll` on Windows (`libvs_imageseqs.avx512.so`
and `.dylib` on unix, from the same `plugin_stem` rule). `avx512` is the suffix
VapourSynth's core knows for this level and, like `avx2`, it is appended by the
core rather than named in `manifest.vs`; the manifest keeps naming the stem.

that was checked in this checkout's `.venv`: with `vs_imageseqs.dll`,
`vs_imageseqs.avx2.dll` and `vs_imageseqs.avx512.dll` beside one manifest,
`core.imgseqs.plugin_path` reports the `.avx512` file. dropping the `.avx512`
file makes it report the `.avx2` one, and dropping that makes it report the
plain one, so the core ranks `avx512` above `avx2` above the baseline exactly as
the manifest rules describe.

## what was measured

`-C target-cpu=x86-64-v4` against the same source, `prefetch=0`, best of the
rounds, the same harness as [23](23-cpu-variant-avx2.md):

| configuration | `sandbox/png` | posterize-check 49 | level-check 129 |
| --- | ---: | ---: | ---: |
| default (`x86-64`) | 1.217 s | 2.550 s | 2.088 s |
| `x86-64-v3` (avx2) | **1.148 s** | 2.356 s | 1.877 s |
| `x86-64-v4` (avx512) | 1.204 s | **2.278 s** | **1.791 s** |

so it is the better of the two on the posterize and jpeg sets and the worse one
on `sandbox/png`, by 5% there and 3% the other way elsewhere. the three sets are
not enough to call one level better than the other, and both are clearly better
than the default. the same `convert` table as
[23](23-cpu-variant-avx2.md) applies: `x86-64-v4` lands at 7.29 / 12.95 / 2.91 ms
against the default's 8.06 / 16.59 / 4.36.

the build is also the smallest of the four, 8,234,496 bytes against the
default's 8,902,656, because 512 bit encodings need fewer instructions for the
same loops.

behaviour is unchanged: `tests/readalpha.vpy` through `IMGSEQS_PLUGIN` gives
byte identical logs for the default, the v3 and the v4 build, 527 `ok` checks
each, so nothing on this path depends on the width of the registers.

## what it needs

`x86-64-v4` is avx512f, avx512bw, avx512cd, avx512dq and avx512vl together, so
a host has to have the whole set for the core to choose this file. the machine
these numbers come from reports avx2, avx512f and avx512bw, and the build runs
there; a machine with only part of the set falls back to the `avx2` or the
baseline file, which is the whole point of a variant.

## what landed

one row in the list [23](23-cpu-variant-avx2.md) introduces:
`Variant(".avx512", "x86-64-v4")`, which makes the same `hatch_build.py` build
and stage three files per x86-64 wheel instead of one. the tools that read the
plugin tree were changed once, in that plan, and see the third file the same
way they see the second.

## what it costs

three builds per x86_64 wheel. at these sizes the plugin files alone are 8.90 +
8.66 + 8.23 MiB uncompressed, against 8.90 for one, and each variant is a full
target dependency rebuild because `RUSTFLAGS` is part of cargo's fingerprint: about two
and a half minutes each on this machine. the release workflow pays that once per
platform; a source build pays it on the user's machine.

## how to check

* `core.imgseqs.plugin_path` on a host with avx512, which must report the
  `.avx512` file, and on a host without it, which must not.
* `tests/readalpha.vpy` against each build through `IMGSEQS_PLUGIN`, which must
  produce byte identical logs.
* `target/bench/decode/png-decode.py` on `sandbox/posterize-check` and
  `sandbox/level-check`, which is where the 8% to 14% should show up.
