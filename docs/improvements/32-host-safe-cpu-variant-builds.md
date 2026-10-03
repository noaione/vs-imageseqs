# 32 - keep CPU variant flags out of host build tools

status: implemented, 2026-10-03. Local packaging and Cargo isolation checks,
a complete Windows source/wheel build, and all three packaged libraries pass
validation. Remote Linux and Windows CI have not been rerun in this session.

## failure and cause

Linux and Windows CI fail while building the `x86-64-v4` plugin. Linux reports
`num-traits`'s `build-script-build` exiting with `SIGILL`, before the plugin
can be linked.

The hook appended `-C target-cpu=x86-64-v4` to `RUSTFLAGS` and invoked
`cargo build --release --locked` without an explicit target. Cargo then used
those flags for both the plugin's dependencies and host executables. A build
script or procedural macro compiled for AVX512 cannot run on a CI machine
that lacks those instructions. The same problem applies to the AVX2 variant
on a host without the `x86-64-v3` feature set.

[Cargo's documentation](https://doc.rust-lang.org/cargo/reference/config.html#buildrustflags)
specifies that passing `--target`, even with the native host triple, limits
these flags to target compilation and separates host build scripts and
procedural macros. The optimized plugin can therefore be built on a machine
that cannot execute it. Runtime selection still belongs to VapourSynth's
manifest mechanism, as described in [23](23-cpu-variant-avx2.md) and
[24](24-cpu-variant-avx512.md).

## fix

`hatch_build.py` now passes `--target <triple>` for both CPU variants. It
respects an explicit `CARGO_BUILD_TARGET`; otherwise it reads `host:` from
the selected `RUSTC`'s verbose version output. A missing or failed host query
ends the build with an error before Cargo can compile an unsafe host tool.
The baseline invocation remains a plain release build.

The CPU flag still applies to the entire target dependency graph. Existing
caller flags are preserved, including `CARGO_ENCODED_RUSTFLAGS`, which Cargo
gives precedence over `RUSTFLAGS`. The caller's environment is copied rather
than modified.

An explicit target moves the variant artifact into
`<CARGO_TARGET_DIR>/<triple>/release/`; the hook now looks there using the
actual build environment. Staged filenames, wheel tags, the manifest and
legal files keep their existing layout. When the caller specifies a target,
the baseline is restored to its shared release directory after staging all
variants, so downstream `readelf` checks still examine the baseline.

This changes build tooling without changing wheel contents or native linkage.
The repository's changelog rule records that work here and in the index.

## validation

Before edits, the existing 74 packaging checks passed and the native validator
passed all 580 checks, both before and after a release build.

The packaging suite now passes 125 checks using only Python's standard
library. Its 51 new checks cover native target discovery, explicit Windows
and Linux targets, all three CPU levels, custom Cargo/Rust executables,
relative output directories, preserved caller flags, encoded flag precedence,
artifact lookup and unchanged caller environments. Running this coverage
against the old hook detects 19 failures.

A standalone, offline Cargo fixture under ignored
`target/avx512-host-validation/` refuses AVX2/AVX512 in its build script and
procedural macro, while requiring AVX512 in its target library. The old hook
fails both host guards. The fixed hook builds and executes the host helpers,
then produces the AVX512 target library. A second build confirms the same
behavior with encoded caller flags. This verifies host isolation even on the
local machine, which can execute AVX512 and would otherwise conceal `SIGILL`.

`C:\Python314\python.exe -m build` successfully built the source archive and
then built its Windows wheel in an isolated environment. All three release
builds completed: baseline in 2m 23s, AVX2 in 2m 09s, AVX512 in 1m 46s.
Inspection of the `py3-none-win_amd64` wheel confirms exactly three plugin
libraries beside the unchanged manifest, every required legal file, and no
Python package or module. `tools/stage-native.py` stages it successfully.

The native validator passed all 580 checks against each of the three staged
wheel libraries. Those logs are byte-identical to the before-change baseline,
with no warnings or failed checks. A plain repository release build also
passes after the change, and its DLL still has the before-change SHA-256:
`74f9f2c5322621e125d5ee56d239a30d767239f74b22c724a93df6ace5b1e031`.
The freshly built wheel has its own baseline binary, which was validated and
benchmarked directly.

The [BENCH.md](../BENCH.md) harness read all 35 `sandbox/png` files with
`--imgseqs-only --reps 3`, at the default prefetch setting:

| configuration | frame totals (s) | best (s) |
| --- | --- | ---: |
| before, repository release baseline | 0.451, 0.404, 0.398 | 0.398 |
| after, newly built wheel baseline | 0.430, 0.393, 0.444 | 0.393 |

The best totals differ by about 1.3%, with overlapping repetition ranges.
These measurements show no substantial throughput change on this corpus;
they do not establish a decoder improvement or measure peak memory. Python
AST checks and `git diff HEAD --check` also pass. Logs and the reproduction
script live under ignored `target/avx512-host-validation/`.

This repair makes no decoder performance claim. Benchmark against the
baseline before and after any decoder change, including the proposed removal
of `image-rs`, and investigate significant speed or memory regressions before
calling it an improvement. See [BENCH.md](../BENCH.md) and
[26](26-remove-image-rs.md).
