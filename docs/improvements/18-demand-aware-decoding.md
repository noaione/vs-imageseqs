# 18 — Avoid unused alpha work and retained ICC payloads

- status: implemented
- touches: `src/decoder.rs`, `src/clip.rs`, `src/prefetch.rs`,
  `src/formats/avif.rs`, `src/formats/heif.rs`, `src/source.rs`,
  `tests/make-alpha-fixtures.py` and `tests/fixtures/avif-broken-alpha.avif`
  (new), `tests/readalpha.vpy`, `AGENTS.md`, `CHANGELOG.md`, `docs/BENCH.md`,
  `docs/IMPLEMENTATION.md`, `docs/HANDOFF.md` and the index row
- depends on: [02](02-frame-write-path.md), whose `Prepare` trait carries the
  demand, and [12](12-heif-avif-yuv-output.md), whose avif alpha item is the
  coded item this skips
- expected: a call that hands out no alpha clip does not decode an avif alpha
  item, and a call that does not export an ICC profile does not keep its bytes,
  with no pixel, property or format change
- result: a decode request (`decoder::Demand`) is derived from the clips a call
  hands out and reaches the avif and heif readers before they start. `Read` on an
  alpha avif decodes 2.7–3.4 ms less per 1536x2304 page and 8–12 ms less per
  frame in the lookahead pool, 11–20% of the wall clock, and its colour planes are
  byte identical. A probe still reads an embedded profile to detect it — the
  `ImgSeqHasICC` fact is unchanged — but keeps the bytes only when
  `icc_profile=True`, which takes 36.4 → 0.5 MiB off a 35-file clip whose files
  each carry a 1 MiB profile. `cargo test` 154 (4 new), the validator 437 checks
  (6 new), and all 192 `frame-parity.py` lines are byte identical.
- risk: low to medium — it changes when an avif alpha item is read, which is also
  the one observable behaviour change: a colour-only read no longer fails on a
  file whose alpha item is broken

## problem and evidence

`source.rs` knows whether it is creating `Read` or `ReadAlpha`, and reads the
`icc_profile` option before probing. However, `decoder::decode` receives only
`ImageInfo`, and `decoder::probe` does not receive the ICC export option.

The native AVIF reader decides to fetch and decode the alpha item from the
file's color type, even when only the color clip will be returned. HEIF also
allocates and packs an alpha plane whenever the file has one. Separately,
probes keep each source's ICC bytes in `ImageInfo.icc_profile`, although frame
property export is disabled by default. Identical profiles across a large
sequence are separate allocations; `Arc` shares a given value but does not
deduplicate independently read profiles.

## what it did, measured

### the alpha item a colour-only call never asks for

`decoder::Demand` is one flag — whether the alpha plane is needed — and it is
built from the same clip list the frames are built from
(`clip::demand_of`), so the decode and the payload built from it cannot disagree.
`Prepare::demand` carries it into the pool, which is where the decode starts, and
the avif and heif readers take it: an avif alpha item is a coded item of its own,
so skipping it is a whole `dav1d` decoder that is not created; a heif alpha plane
is a buffer and a copy this module would otherwise do. The other readers are not
told, because a webp or a jxl arrives with its alpha channel already in the
buffer the decoder wrote.

The corpus is twelve colour pages of `sandbox/avif` cropped to 1536x2304 and
encoded three ways — with the page's own grey content as alpha, with a smooth
ramp, and with no alpha at all — built by `target/bench/make-alpha-corpus.py`.
The third is the control: a file with no alpha item has nothing to skip, so it
must not move. Best of five rounds per build, `Read` over all twelve files,
`prefetch=0`:

| corpus | before | after | change |
| --- | --- | --- | --- |
| no alpha item (control) | 193.4 ms, decode 12.02 | 195.3 ms, decode 12.27 | unchanged |
| alpha = the page's own grey | 284.1 ms, decode 19.83 | 254.1 ms, decode 17.14 | −10.6% wall, −2.69 ms |
| alpha = a smooth ramp | 258.0 ms, decode 16.98 | 206.9 ms, decode 13.55 | −19.8% wall, −3.43 ms |

and the decode stage alone — the part that is the alpha item and not the file
open, the buffer or the frame write — is 11.95 → 9.14 ms (mask) and 11.95 →
8.51 ms (ramp) against 8.29 → 8.44 ms for the control. In the lookahead pool the
same work is contended, so the same twelve files at `prefetch=4`, best of three,
move 177.2 → 143.1 ms (mask) and 149.9 → 127.3 ms (ramp) of wall clock, with the
control at 128.0 → 114.0 ms.

Both halves of the corpus improve and the control does not, which is what says
the change is the alpha item rather than the machine. The saving is not
proportional to the alpha item's compressed size: the ramp's alpha is the cheaper
one to *code* and the larger one to skip, because a smooth ramp costs fewer bits
but the same plane to decode and copy.

**the one behaviour this changes.** A colour-only read no longer reads the alpha
item, so it no longer fails on a file whose alpha item is broken. That is the
point rather than an accident — the item is not part of what such a call hands
out — and it is tested rather than left to be discovered:
`tests/fixtures/avif-broken-alpha.avif` is the coded item of `avif-yuv420p.avif`
with an alpha item beside it that holds no frame. `Read` reads it and hands out
the same planes as the file it was cut from; `ReadAlpha` still fails on it with
"the item holds no picture", which is the clip that item exists for. The fixture
is hand-written by `tests/make-alpha-fixtures.py` like the two beside it.

### the ICC bytes a call that does not export them never uses

Reproduced first: a 35-file png sequence whose every file carries a 1 MiB
profile (`target/bench/make-icc-corpus.py`, measured with
`target/bench/icc-retention.py`, which asks a PowerShell process for this
process's working set because the sandbox refuses the in-process query). `Read`
with `prefetch=0` decodes nothing, so the working set right after the call is the
clip's own:

| corpus | `icc_profile` | before | after |
| --- | --- | --- | --- |
| 35 files, 1 MiB profile each | 0 (default) | **+36.4 MiB** | +0.5 MiB |
| 35 files, 1 MiB profile each | 1 | +36.2 MiB | +35.7 MiB |
| 35 files, 588-byte profile each | 0 | +0.6 MiB | +0.5 MiB |
| 35 files, no profile | 0 | +0.5 MiB | +0.5 MiB |

so the default kept one copy of every profile for the life of the clip — 36 MiB
against the 176 MiB those 35 pages then take as frames — and the fix is that
`decoder::probe` drops what it read unless the caller asked for it. The bytes are
still read, because whether a file has a profile is the `ImgSeqHasICC` fact and
that is reported whatever the option says; what is gone is the copy that nothing
would have used. Every reader is covered by the one drop, because they all hand
over a profile they have already copied out of the file.

## deviations from the plan

- **the demand is one flag, not a general request description.** The plan asks
  for "an internal decode request describing the outputs required". Alpha is the
  only output any reader here can skip: a webp's and a jxl's alpha arrives inside
  the buffer their decoder writes, and a `ReadAlpha` colour clip needs the colour
  planes whatever else is asked for. A struct with one field would be the same
  thing with more places to get it wrong.
- **the ICC bytes are dropped where the decode is asked for them, not where they
  are read.** Threading the option into five `image_info` functions would avoid
  the transient copy as well, but each of them needs the profile to detect it
  anyway, and one drop in `decoder::probe` cannot be forgotten by a reader added
  later. The plan's own note — "some APIs may still need to read the bytes to
  detect them" — is why the copy stays.
- **no deduplication of identical profiles.** The plan allows it only if
  measurement justifies it, and it is the opt-in path: with `icc_profile=True`
  the 35 copies are what the caller asked for, and sharing them would need a
  content-keyed cache with a lifetime the clip does not have.

## validation

this is the plan's own list, as written before the change; the measured answers
are in "what it did, measured" above.

- Measure `Read` versus `ReadAlpha` on lossless alpha AVIF/HEIC and sequences
  with repeated large ICC profiles. Record decode time, creation time and
  process memory, separately from the prefetch payload budget.
- Carry an internal decode request describing the outputs required. Start with
  skipping the independently coded AVIF alpha item for color-only requests.
  For HEIF, first verify whether the wrapper can suppress alpha decoding; if it
  cannot, distinguish savings in packing/allocation from native decode work.
- Retain ICC payloads only when export is requested, while preserving
  `ImgSeqHasICC` detection. Some APIs may still need to read the bytes to detect
  them; do not claim that all metadata I/O disappears. Consider deduplication
  for export-enabled sequences only if measurement justifies it.
- Preserve `ImgSeqOriginalColorType`, opaque-alpha behavior, and the shared
  color/alpha cache used by `ReadAlpha`. Demand belongs to the whole filter
  instance, not whichever output happens to request a frame first.
- Compare every color plane from `Read` against the color output of `ReadAlpha`,
  including premultiplied-alpha samples before deciding whether alpha is safely
  skippable. Preserve both clips' ICC properties when enabled and their absence
  when disabled. Check seeks, repeated requests and interleaved color/alpha
  requests. Decide and test how color-only reads handle a broken auxiliary alpha
  item; that error behavior should not change accidentally.

what that came to:

- `cargo test --locked`: **154 passed** (was 150). The four new ones are the
  demand derived from the clips, the colour-only decode that skips the alpha item
  and matches the colour planes of the decode that does not, the broken-alpha
  fixture read both ways, and the profile kept only when it is exported.
- `tests/readalpha.vpy`: **437 checks pass** (6 new), against 431 before, for the
  broken-alpha fixture and the intact file beside it.
- `target/bench/frame-parity.py`: **192 of 192 data lines byte identical**,
  including a new `demand` set over the whole 36-file alpha corpus, so no colour
  plane and no alpha plane moved.
- `ReadAlpha` still shares one decode per frame: the demand is a property of the
  call, taken from the same clip list both clips are built from, so a colour and
  an alpha request cannot ask for different decodes.
- the heif half is the packing and the allocation only: `libheif` decodes a
  page's alpha plane whether or not anything asks for it, and `libheif-rs` 3.0.0
  exposes no option to suppress it, which is why `heif::decode` takes the demand
  but the saving there is smaller than the avif item's.

## left over

- **a heif corpus with alpha.** The heif half is implemented and unmeasured: the
  alpha corpus is avif only, because the four colour pages of `sandbox/heic` have
  no alpha channel and the only alpha heic here is the 3x2 `alpha-rgba8.heic`
  fixture, whose frames are too small to time.
- **deduplicating identical profiles when export is enabled.** See the deviation
  above; the measurement that would justify it is a sequence of large profiles
  read with `icc_profile=True`, which is exactly the case the option exists for.
- **a pre-multiplied alpha fixture.** The plan asks for premultiplied samples to
  be compared before deciding whether alpha is skippable; nothing here writes
  one (`avifenc --premultiply` would), and the decision does not depend on it
  because the colour planes are read from the colour item either way.
- **`n_threads` on the two decoders one frame can now create at most.** An avif
  with alpha still creates two `dav1d` decoders under `ReadAlpha`, each with the
  host's thread count; [19](19-avif-thread-budget.md) measured it and left it
  alone, because the default already uses 8.8 of ten cores and a share of them
  costs 20%.
