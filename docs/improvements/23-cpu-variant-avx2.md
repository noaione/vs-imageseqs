# 23 - ship an `x86-64-v3` build as a `avx2` variant

status: measured, not implemented. the build itself is done and checked (see
[22](22-png-decode-path.md) for the benchmark it was built for); what is
missing is the packaging that would let a wheel carry more than one build.

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

so the baseline is `x86-64-v2`, not `x86-64`: v2 is sse4.2 and runs from
Nehalem onward, which is a floor a wheel can take for free, and the `avx2`
variant is the one for everything newer. its own CI builds both levels in
`.github/workflows/rust-tests.yml`, and its `docs/HANDOFF-nmanga.md` describes
the installed tree as `vs_nimages.dll` plus `vs_nimages.avx2.dll`.

the variant this plan adds is the `avx2` one, over the same `x86-64-v2`
baseline `vs-nimages` uses, so a wheel built here is installable on the same
machines one built there is. moving this repository's baseline off plain
`x86-64` is part of this plan rather than a separate one: the measurement above
puts the two within noise of each other on this machine (1.217 against 1.206 s),
so the reason to move is the shared floor and not a number.

## the intended edit

* `hatch_build.py`: a `Variant` record holding a suffix and a `target-cpu`
  value, a `variants(environment)` that answers one baseline build off x86-64
  and two on it, `build_plugin` taking the variant and appending
  `-C target-cpu=...` to `RUSTFLAGS`, and `initialize` staging every variant
  into `vapoursynth/plugins/imageseqs/` beside one `manifest.vs`. The manifest
  keeps naming the stem, which it already does.
* `tools/stage-native.py` and the two wheel checkers
  (`tools/check-linux-wheel.py`, `tools/package-linux-wheel.py`) stage and
  check the plugin tree, so each has to know a variant set rather than one
  filename.
* `.github/workflows`: the x86_64 rows build and check both variants, the way
  `vs-nimages`' `rust-tests.yml` does.

## how to check

* `python -c "import vapoursynth as vs; print(vs.core.imgseqs.plugin_path)"`
  against an installed wheel on a machine with avx2, which must report the
  `.avx2` file.
* `tests/readalpha.vpy` against each build through `IMGSEQS_PLUGIN`, which
  must produce byte identical logs.
* `tools/check-linux-wheel.py`, and a zip listing of the wheel: both files,
  their manifest, and no variant named in `manifest.vs`.
* `target/bench/decode/png-decode.py` before and after, which is where the
  6% to 11% should show up.

## what this does not do

it does not add `#[target_feature]` kernels. The gain here is the compiler
using wider registers on loops that already existed, and the decoder at the
bottom of the png path (`fdeflate`) is scalar and stays scalar whatever the
baseline is.
