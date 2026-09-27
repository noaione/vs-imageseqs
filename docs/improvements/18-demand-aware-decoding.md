# 18 — Avoid unused alpha work and retained ICC payloads

Status: proposed measurement and design work; code paths confirmed on
2026-09-27. Priority: medium. No speed or memory saving has been measured yet.

## evidence

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

## intended investigation and change

- Measure `Read` versus `ReadAlpha` on lossless alpha AVIF/HEIC and sequences
  with repeated large ICC profiles. Record decode time, creation time and
  process memory, separately from the prefetch payload budget.
- Carry an internal decode request describing the outputs required. Start with
  skipping the independently coded AVIF alpha item for color-only requests.
  For HEIF, first verify whether the wrapper can suppress alpha decoding; if
  it cannot, distinguish savings in packing/allocation from native decode work.
- Retain ICC payloads only when export is requested, while preserving
  `ImgSeqHasICC` detection. Some APIs may still need to read the bytes to detect
  them; do not claim that all metadata I/O disappears. Consider deduplication
  for export-enabled sequences only if measurement justifies it.
- Preserve `ImgSeqOriginalColorType`, opaque-alpha behavior, and the shared
  color/alpha cache used by `ReadAlpha`. Demand belongs to the whole filter
  instance, not whichever output happens to request a frame first.

## validation

Compare every color plane from `Read` against the color output of `ReadAlpha`,
including premultiplied-alpha samples before deciding whether alpha is safely
skippable. Preserve both clips' ICC properties when enabled and their absence
when disabled. Check seeks, repeated requests and interleaved color/alpha
requests. Decide and test how color-only reads handle a broken auxiliary alpha
item; that error behavior should not change accidentally.

Keep the existing default API. Land optimizations only with before/after
measurements and no pixel/property regression.
