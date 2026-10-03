# 23 - ship an `x86-64-v3` build as a `avx2` variant

status: implemented. `hatch_build.py` builds and stages one library per x86-64
level, `manifest.vs` names the stem, and the tools that stage or check a wheel
know a variant set rather than one filename. the measurements below are the
evidence it was worth doing; the wheel it produces was built and inspected,
and `tests/check-packaging-tools.py` covers the wheels that would get it
wrong.

## what was measured

the same source built with `-C target-cpu=x86-64-v3` is 6% to 11% faster than
the shipped default on every png and jpeg set tried, and the gain is almost all
in the plane write:

| configuration | `sandbox/png` | posterize-check 49 | level-check 129 |
| --- | ---: | ---: | ---: |
| default (`x86-64`) | 1.217 s | 2.550 s | 2.088 s |
| `x86-64-v2` | 1.206 s | | |
| `x86-64-v3` (avx2) | **1.148 s** | **2.356 s** | **1.877 s** |

per stage, `convert` is where it lands: 8.06 -> 7.18 ms on `sandbox/png`,
16.35 -> 12.25 ms on the 48 png of posterize-check, 4.36 -> 2.97 ms on the
jpeg set. `read` moves far less, 24.54 -> 23.53 ms and 32.99 -> 30.81 ms, which
is a copy loop the compiler vectorized to 256 bits rather than a faster
decoder: `fdeflate`, the decoder on those pages, has no architecture specific
code at all.

the builds were also checked for behaviour: `tests/readalpha.vpy` run against
the default and the v3 build through `IMGSEQS_PLUGIN` produces byte identical
logs, 527 `ok` checks each.

## the mechanism

VapourSynth's manifest already carries this. `manifest.vs` names the plugin's
*stem*, and the core appends a variant suffix of its own when the host CPU
supports one, looking for `<stem>.<variant><extension>`. a manifest that names
`vs_imageseqs` therefore loads `vs_imageseqs.avx2.dll` on a machine with avx2
and `vs_imageseqs.dll` on one without, with no Python and no loader script.

that was checked rather than assumed: with `vs_imageseqs.dll`,
`vs_imageseqs.avx2.dll` and `vs_imageseqs.avx512.dll` all present beside one
manifest in this checkout's `.venv`, `core.imgseqs.plugin_path` reports
`vs_imageseqs.avx512.dll`; with the `.avx512` file removed it reports
`vs_imageseqs.avx2.dll`; with both removed it reports `vs_imageseqs.dll`.

`vs-nimages` in the sibling checkout is the precedent for the naming and the
baseline, in its `hatch_build.py`:

```python
X86_64_V2 = "x86-64-v2"
X86_64_V3 = "x86-64-v3"

def variants(environment):
    if not is_x86_64(environment):
        return [Variant("", None)]
    return [Variant("", X86_64_V2), Variant(".avx2", X86_64_V3)]

def plugin_filename(environment, variant):
    return f"{plugin_stem(environment)}{variant.suffix}{plugin_extension(environment)}"

def manifest(environment):
    return MANIFEST_HEADER + f"{plugin_stem(environment)}\n"
```

`vs-nimages` puts its baseline at `x86-64-v2`, which is sse4.2 and runs from
Nehalem onward, and its `avx2` variant above it. this repository keeps its
baseline a plain build with no `-C target-cpu` at all, which is a lower floor
still and a strictly additive change: nothing that loaded the plugin before
can stop loading it. the measurement above puts plain `x86-64` and
`x86-64-v2` within noise of each other on this machine (1.217 against
1.206 s), so moving the baseline buys no number, and the convention that does
matter — the stem in the manifest and the `.avx2` suffix beside it — is taken
from that checkout.

## what landed

* `hatch_build.py`: a `Variant` record holding a suffix and a `target-cpu`
  value, a `variants(environment)` that answers the baseline plus `.avx2` and
  `.avx512` on x86-64 and the baseline alone anywhere else, `build_plugin`
  taking the variant and appending `-C target-cpu=...` to `RUSTFLAGS`, and
  `initialize` building and staging every variant into
  `vapoursynth/plugins/imageseqs/` beside one `manifest.vs`. the manifest names
  the stem, which is what the core appends a variant suffix to.
* cargo writes the baseline's file name whichever variant it just built, so
  `initialize` copies the baseline back into the release directory afterwards.
  a plain `cargo build`, the benchmarks and the Linux build's own `readelf`
  check all read that path.
* `tools/stage-native.py` checks the shape rather than a count: the library the
  manifest names has to be there, and every other library in the plugin's
  directory has to be `<stem>.<variant><extension>`. `tools/package-linux-wheel.py`
  sets `$ORIGIN/lib` on every variant, not only the baseline, and
  `tools/check-linux-wheel.py` accepts the suffixed libraries as part of the
  plugin rather than as unexpected content.
* `tests/check-packaging-tools.py` has a `check_variants` case: a variant wheel
  stages all three libraries, an unexpected library is refused by name, and a
  manifest naming a stem the wheel does not hold is refused.

the CI workflows need no change: `python -m build` is what drives the hook, so
the x86_64 jobs produce the variants where they already build. macOS is arm64
only — `tools/package-macos-wheel.py` refuses any binary without an arm64
slice — so that wheel gets the single baseline library and nothing there
changed.

## how to check

* `C:\Python314\python.exe tests\check-packaging-tools.py`, which is the
  offline one and covers the staging path the release uses.
* a zip listing of the built wheel: the three libraries, their manifest, and
  no variant named inside `manifest.vs`.
* `python -c "import vapoursynth as vs; print(vs.core.imgseqs.plugin_path)"`
  against that wheel installed, which must report the file whose suffix this
  host's CPU supports.
* `tests/readalpha.vpy` against each library through `IMGSEQS_PLUGIN`, which
  must produce byte identical logs.
* `tools/check-linux-wheel.py` on the repaired Linux wheel, which now has to
  accept three `libvs_imageseqs*.so` and reject anything else.
* `target/bench/decode/png-decode.py` before and after, which is where the
  6% to 11% should show up.

## what this does not do

it does not add `#[target_feature]` kernels. The gain here is the compiler
using wider registers on loops that already existed, and the decoder at the
bottom of the png path (`fdeflate`) is scalar and stays scalar whatever the
baseline is.
