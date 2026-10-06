# 37 - cache manylinux and musllinux build inputs

status: implemented, 2026-10-06. Local packaging and shell-path checks pass.
Container builds, cache transfers and build-time savings remain to be measured
in GitHub Actions.

## repeated work

The Linux wheel jobs took approximately seven minutes for manylinux and twelve
minutes for musllinux, as reported by the maintainer. Both started with empty
Cargo outputs and rebuilt dav1d, libde265 and libwebp. Musllinux also installed
Rust every time. These totals are the CI baseline, not measured stage timings.

## implementation

Both jobs run `tools/setup-linux-build.sh` before restoring compiled inputs.
The builders source the same script, so Cargo sees the same target, flags,
toolchain and native search paths at cache restore and during compilation.
Search paths and the musl `-crt-static` flag are only added once. Direct script
invocations still install the prerequisites themselves.

All wheel jobs pin Rust to 1.99.0 through the workflow's top-level
`IMGSEQS_RUST_TOOLCHAIN`, beside `CARGO_TERM_COLOR`. NASM is installed for both
Linux builds through their package managers and the Windows wheel through
Chocolatey. Its Windows install directory is added to the job's PATH and
`nasm -v` verifies the install. macOS ARM64 skips NASM.
Musllinux caches its rustup toolchain and Cargo shims. Both jobs also cache
Python package downloads; installations still run to select compatible tools.

Each platform has two independent compiled caches:

- The installed codec `prefix/` and source `archives/`, with an exact key only.
- Cargo dependencies in `target/<platform>/cargo`, through `Swatinem/rust-cache`.
  Its additional key is the native cache fingerprint; its normal key also
  includes Rust, Cargo dependencies and compiler environment settings.

`tools/linux-native-cache.py` fingerprints the platform, workflow (including
the pinned container), native build recipe, setup script, CMake toolchain,
compiler flags, search paths and installed compiler/assembler/build-tool
versions. Hashing the whole workflow is conservative: unrelated workflow edits
can also invalidate the cache. Pixel-source edits do not invalidate the codec
prefix. Changes to Cargo.lock invalidate the Rust dependency cache normally.

Native compilation is skipped only on an exact Actions hit with a matching
completion stamp and the required headers, libraries and pkg-config files.
A partial install is rebuilt. Source archives are still SHA-512 checked on a
warm run, and their extraction is skipped when the prefix is reused. Keeping
the archives is necessary for the relinking source bundle.

The caches exclude intermediate/final wheels, standalone bundles and generated
relinking trees. Wheel assembly, auditwheel repair, dependency/layout checks,
staging and source-bundle generation run every time. Distribution validators
still use fresh containers with no build cache. New helpers are included in
the source distribution and relinking bundle; native linkage and wheel contents
are unchanged, so this work gets no changelog entry.

## validation

Before edits, the native validator passed both before and after a release
build. A three-pass benchmark of ten sandbox WebP pages recorded 0.937 seconds
for default lookahead and 3.130 seconds for `prefetch=0` (best passes).
After edits, the release build and all 824 validator checks pass again, with no
unexpected warnings or errors. The first benchmark round recorded 1.000 and
3.208 seconds. Repeating it with the identical plugin hash recorded 0.878 and
2.961 seconds, which puts the variation on the machine rather than a consistent
runtime regression. This build-tool change does not alter the Windows runtime.
Logs are under the ignored `target/ci-cache-*.log` paths.

All 148 packaging checks pass. The suite covers incomplete/unstamped prefixes, exact key matches,
missing static libraries, platform/container/recipe/compiler/flag invalidation
and exporting the CI environment. A WSL harness runs both actual shell builders
with fixture archives and simulated build tools: cold and warm builds, a broken
prefix, a corrupt archive, and repeated setup. Warm runs skip native compilation
but still assemble the wheel and regenerate the source bundle. A broken prefix
rebuilds, and a corrupt archive stops before wheel assembly. This checks shell
control flow, not native compilation or auditwheel behavior. Shell syntax,
Python syntax, TOML/YAML parsing and the source distribution's new build inputs
pass. Actionlint passes with only its unknown-label diagnostics suppressed: its
runner catalog does not yet recognize the existing `ubuntu-26.04` label.
Docker was not running locally, so native Linux compilation, cache transfers
and repaired-wheel validation remain CI checks.

## CI follow-up

Run the same revision twice: the first seeds the caches and the second measures
a warm build. Then change only a Rust source and confirm the codec prefix is
reused while the plugin is rebuilt. Record job and stage durations, cache sizes
and transfer times. Require both clean-container distribution validators to
pass. No build-time speedup is claimed until these runs complete.
