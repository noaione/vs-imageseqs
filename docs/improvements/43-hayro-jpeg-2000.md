# 43 - hayro jpeg 2000 backend

status: implemented. `hayro-jpeg2000` 0.4.1 replaced `jpeg2k` 0.10.1 and the
OpenJPEG sources `openjpeg-sys` 1.0.12 vendors on 2026-10-25. every jpeg 2000
frame this tree produced before it is produced byte for byte by the new decoder,
and the build no longer needs a C toolchain on any platform.

## why the swap

`openjpeg-sys` compiles the OpenJPEG C sources with `cc` on every platform, which
made it the one dependency in this tree that needs a C toolchain, a build
directory and minutes of a cold build, and it is the one that failed without a
diagnostic often enough to be a maintenance problem: the maintainer's own words
are that it "sometimes would just randomly fails". That failure surface is
entirely inside the build, not the decode, and every wheel this project ships
pays for it three times (baseline, `.avx2`, `.avx512`).

`hayro-jpeg2000` is a pure-Rust decoder: no `build.rs`, no C sources, no
toolchain beyond `cargo`, `#![forbid(unsafe_code)]`, MSRV 1.92 against this
workspace's 1.94.0, and Apache-2.0 OR MIT. `default-features = false, features =
["std", "simd"]` pulls in `fearless_simd` and nothing else — the `image` feature
(which would add the `image` crate and `moxcms`) and `log` stay off, which matters
because this tree spent a whole plan ([26](26-remove-image-rs.md)) removing
`image` from the decode paths.

## what the crate gives

the public surface the adapter needed, from 0.4.1's own source:

| need | 0.4.1 |
| --- | --- |
| open from bytes | `Image::new(&bytes, &DecodeSettings)` |
| decode | `image.decode(&mut DecoderContext) -> DecodedImage` |
| per-component samples | `DecodedImage::components() -> &[ComponentData]` |
| sample values | `ComponentData::samples() -> &[f32]`, DC-shifted raw values in `[0, 2^precision-1]` |
| component depth | `ComponentData::bit_depth() -> u8` |
| alpha present | `Image::has_alpha()`, or the probe's own `cdef` walk |
| palette indices | `DecodeSettings { resolve_palette_indices: false, .. }` |

`samples()` is the important one: `data_u8()`/`store_u8_into()` scale to eight
bits, but the components themselves are the file's own precision, which is what
[10](10-nominal-bit-depth.md) and the current `L16`/`RGB48` output need. For a
component of `depth` bits the writer wants the sample in a sixteen bit word that
it shifts down by `16 - depth` ([`src/pixel.rs`](../../src/pixel.rs)'s `shift`),
so the adapter stores `v << (16 - depth)`. That is not the same word `jpeg2k`
handed over — that one scaled by `p * 65535 / (2^d - 1)` — but both shift back
down to the same sample, which is why the frame bytes are identical and the
decoder buffer is not. Every comparison below hashes frames, not decoder buffers.

## the four edges, and what was done about each

| edge | what the crate does | what this module does |
| --- | --- | --- |
| sYCC planes | `decode()` converts sYCC to r,g,b in place and no setting turns it off, so the y,cb,cr planes the reader hands out would become rgb | the `colr` box's four byte enumerated colour space is rewritten 18 (sYCC) → 16 (sRGB) in the one buffer the crate reads, so the crate sees a file that needs no conversion; nothing moves, and the probe has already read the original value |
| premultiplied or late `cdef` | a channel type of 2 is not a channel it knows, and it drops the whole box; it also decides "has alpha" from the box's *last* entry, so a box that names opacity elsewhere loses the clip | the `cdef` box is rewritten in place: type 2 becomes type 1 (this reader never unpremultiplies either way) and the opacity entries move to the end. entries keep their length and component indices |
| palette pages | `resolve_palette_indices: false` returns the codestream's index components, at the index's own precision — not scaled into a word as OpenJPEG returned it | `palette_buffer` looks the index up directly and the old `* (2^bits - 1) / 65535` scale is gone |
| a size nothing can hold | allocates one `f32` a sample a component before it checks anything (upstream #1549: a 97 byte codestream claiming 100000x100000 reaches tens of gigabytes); PR #1558 is unmerged | `parse_siz` refuses a file whose `width * height * components` is past 2^28 samples, and one whose tile grid is past 65,536 tiles, with a path-qualified error — a release build aborts on a failed allocation, so this is the probe's job |

Two behaviours changed as a consequence, and both are named in the changelog:

- a file whose components **all** state one subsampling factor is now refused.
  the crate decodes such a file at that factor's resolution rather than at the
  size its `SIZ` states, so a frame of the stated size would hold a picture that
  does not exist. nothing else in this tree read those files either.
- sYCC output is preserved but only for the layouts `sycc_format` names; the
  rest still go through the module's own ycbcr→rgb conversion, which now reads
  the decoder's replicated component buffers.

## the edit

**`Cargo.toml`.** `jpeg2k` out, `hayro-jpeg2000` 0.4.1 in with `std` and `simd`
only.

**`src/formats/jp2.rs`.** the probe is unchanged. `decode` reads the file once
(it already did), patches the `colr` and `cdef` boxes, opens the crate, checks
what came back against the `SIZ` header, and then builds the frame from the
components:

- `sample_word` holds the whole word contract: `round().clamp(0, 2^depth - 1)`,
  then `<< (16 - depth)` for a wide layout or a byte for an 8-bit one.
- `decode_interleaved` interleaves the components in the order the crate leaves
  them, which is association-sorted with opacity last once the box is normalised.
- `decode_yuv` reads each plane's pixel from the decoder's replicated component
  buffer at `x * dx, y * dy`, clamped because the last block is clipped rather
  than padded (`ComponentHeader::sample_index`, which has its own unit test).
- `decode_sycc_rgb` keeps the old conversion for a layout no yuv format names.
- `palette_buffer` expands the indices itself, and `check_decoded_header`
  compares the component count and each component's depth against the probe.

**`tests/`.** a new fixture, `sycc-jp2-444.jp2`: the existing `alpha-jp2-rgb8.jp2`
with its `colr` box relabelled sYCC, so the components are that page's three
planes and the check is that the reader hands them out as `YUV444P8`, labelled
`_Matrix=6`/`_Range=1`, unchanged. this is the only end-to-end check of the
`colr` rewrite, and it is identical between the two builds. a 4:2:0 or 4:2:2
sYCC page needs per-component sampling that `opj_compress` cannot write, so
those layouts have a unit test of the index arithmetic and no fixture — they had
none before this change either.

**legal and docs.** `LICENSES/openjpeg-COPYING.txt` and its
`LICENSES/README.md` entry are gone, `THIRD_PARTY_NOTICES` loses the OpenJPEG
block, its dependency-list line and its URL, `tests/check-packaging-tools.py`
loses the assertion that names the text, and `docs/IMPLEMENTATION.md` and
`AGENTS.md` name `hayro-jpeg2000`. `hayro-jpeg2000` itself is a pure Rust crate,
so the native notice bundle needs no entry for it — the same rule that keeps the
other Rust crates out of it.

## the comparison

measured on 2026-10-25 on the maintainer's macOS arm64 machine, release build,
with `target/bench/ab-jp2.py` and the corpus `target/bench/research/corpus-jp2.md`
describes: the 35 `sandbox/png` pages converted losslessly with `opj_compress`
into `sandbox/jp2` (260.9 MB, 31 gray pages of 18.5 MiB and 4 colour ones), which
is the shape `docs/BENCH.md`'s jpeg 2000 section describes. the baseline binary
is preserved at `target/bench/baseline/libvs_imageseqs.dylib`, so the comparison
does not depend on this tree still building OpenJPEG.

**frames are identical.** the corpus digest (35 frames, `Read` and `ReadAlpha`)
and the fixture digest (9 decoded, 1 refused by design, the same refusal) are
byte for byte the same over both builds, properties included, and so is the
`sandbox/webp` control and the new `sycc-jp2-444.jp2`. `tests/readalpha.vpy`
passes on both, and the build under test passes its 1034 checks.

**speed**, alternating rounds of two timed passes each; the base and the new
build swap places every round, and the medians are the reading because the
machine's own speed drifted about 5% over the session:

| reading the set | `jpeg2k` | `hayro-jpeg2000` | ratio |
| --- | --- | --- | --- |
| 35 pages, default `prefetch=4` (5 rounds) | 8789 ms | 8108 ms | 0.922 |
| 35 pages, `prefetch=16` (5 rounds) | 5848 ms | 5742 ms | 0.982 |
| 35 pages, `prefetch=0` (2 rounds) | 33149 ms | 30833 ms | 0.930 |
| clip creation only | 0.80 ms | 0.95 ms | 1.18 |

so the swap is **7.8% faster** at the default window and **7.0%** serial, and
level at the deepest one. that is not the decoder: it came from the module's
own copy, and the two measurements before it are worth recording because they
say where the difference was. with the per-sample copy this plan first
shipped, the same protocol read 1.010 at the default and 1.124 at
`prefetch=16`; the copy costs 19.8 ms a frame for a gray page and 48.7 ms for
a colour one, which four workers hide and sixteen expose. replacing it with
the crate's own `data_u8` (1.7 ms and 3.9 ms) removed that cpu and every row
improved at once.

**where the decode time goes.** measured with
`target/bench/hayro-bench`, a standalone crate that links `hayro-jpeg2000`
alone, splits one file into parse / decode / copy, and can be sampled with
`sample`: 

| stage | 3672x5274 gray | 3672x5274 r,g,b |
| --- | --- | --- |
| `Image::new` (boxes and codestream header) | 0.01 ms | 0.01 ms |
| `Image::decode` | 908 ms | 1222 ms |
| this module's interleave, per sample (before) | 19.8 ms | 48.7 ms |
| this module's interleave, `data_u8` (now) | 1.7 ms | 3.9 ms |

and `sample` over a decode loop, share of samples:

| function | gray | colour |
| --- | --- | --- |
| `j2c::bitplane::decode_inner` (significance and refinement, MQ-coded) | 77% | 56% |
| `j2c::bitplane::fast_path::cleanup_pass` (MQ cleanup, table-driven) | 18% | 34% |
| `j2c::idwt::filter_2d` (inverse wavelet) | 1.9% | 4.2% |
| component buffers (`bzero`/`memmove`/`memset`) | 0.7% | 1.4% |

about 95% of a frame is the bit-plane arithmetic decoder and 2% to 4% the
inverse wavelet, so this workload is entropy-bound. `simd` cannot help it: it
reaches the IDWT, the colour transform and the interleave, which together are
the small end of the table, and the cleanup fast path is already the
OpenJPEG-derived one the crate's own comment credits.

**memory**, `maximum resident set size` of one pass over the corpus, both
builds measured by the same script:

| row | `jpeg2k` | `hayro-jpeg2000` | ratio |
| --- | --- | --- | --- |
| `prefetch=0` | 715 MB | 1105 MB | 1.55 |
| `prefetch=16` | 2139 MB | 3271 MB | 1.53 |

this is the decoder's own scratch, not the lookahead pool's payload: the crate
holds one `f32` a sample a component where OpenJPEG held a word, so an
in-flight decode costs twice the component bytes, and at sixteen workers that
is over a gigabyte. unlike the time, no plugin-side change removes it — it is
the representation the decoder chose — and the payload budget
(`prefetch_memory`) does not count it, and cannot: it is not the pool's memory.

## what was not done

- **the deep-window memory cost.** it is the decoder's representation, not the
  pool's accounting, so the fix is upstream: a 16-bit integer path instead of
  `f32` components, or the buffer reuse its own `TODO` names. PR #1558
  (`max_decoded_samples`) bounds a hostile file, not the 1.5x a real one costs.
  a caller who is memory sensitive can lower `prefetch`, which is what the
  argument is for. a plugin-side follow-up could count in-flight decoder
  scratch in the budget the way it counts payload, which would hand out
  fewer concurrent decodes at a deep window rather than more memory.
- **forking the decoder for speed.** the profile above says the win would have
  to come from the MQ/bit-plane decoder (95%), which is the hardest part to
  change and the part already using OpenJPEG's fast-path idea; the callers'
  three remaining per-sample loops (16-bit, sYCC and planar yuv) are worth
  writing but cannot pay for a fork. upstream `main` was measured instead: at
  `7acf4212` (160 commits past 0.4.1, the IDWT and bounds-check work of #1539,
  #1540 and #1542) a gray page decodes in 899.7 ms against 908.3 and a colour
  one in 1216.5 against 1222.2, which is 0.5% to 0.9% — the IDWT is 2% of this
  corpus. `main` also carries unreleased ICC and subsampled-fill changes, so
  the gain does not justify the jump.
- **another crate or a published fork.** none is usable: `image` has no jpeg
  2000 codec; `openjp2` is a C-shaped pure-Rust port last released in July
  2025 with no real-world correctness claim; the 2026 crop (`j2k`,
  `oxideav-jpeg2000`) is pre-1.0 with no independent dependents; and every
  hayro fork on crates.io is *behind* 0.4.1 (an older snapshot, feature
  additions, or stale). 98 GitHub forks were scanned and none carries unmerged
  performance work. the survey is `target/bench/research/jp2-alternatives.md`.
- **a 4:2:0 or 4:2:2 sYCC fixture.** no tool here writes per-component sampling
  from an rgb source, so the index arithmetic is held by a unit test instead.
- **`docs/BENCH.md`'s jpeg 2000 table.** its numbers are from the maintainer's
  Windows machine and were not re-measured; the section points at this page for
  the comparison that was.

## found while measuring

`Prefetcher::drop` had a lost-wakeup race that predates this plan: it stored
`stop` and called `signal.notify_all()` **without holding `state`**, and a worker
that had just tested `stop` inside `worker()` could call `signal.wait()` after the
notification had been delivered, then sleep until the process exited. the first
run of `tests/readalpha.vpy` on this machine hung exactly there — main thread in
`Sequence`'s destructor, `pthread_join` on `imgseqs-prefetch`, worker parked on
the condvar — and the same binary passed on a re-run, which is what a race looks
like from outside. the benchmark harness hit the same hang independently. the
store and the notification are made under the pool's lock now, which closes it;
that is a fix of its own in the changelog, not part of the decoder swap.
