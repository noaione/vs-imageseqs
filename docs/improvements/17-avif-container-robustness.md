# 17 — Validate AVIF box boundaries and decode eligibility

Status: proposed; findings from code inspection on 2026-09-27, not a completed
malformed-file test campaign. Priority: high for bounds handling, medium for
additional container arrangements.

## evidence

The custom reader in `src/formats/avif.rs` has several concrete review targets:

- `leading_boxes` reads an extended 16-byte header but calculates the resize
  and payload slice using `header.len()` (eight). This can overwrite the
  extended-size bytes and consume the wrong amount of input.
- `Meta::data_range` checks `base + offset`, then constructs the end with
  unchecked `start + length`. Full decode calls use `usize::MAX` as their
  length limit. `read_range` allocates before verifying that the range fits
  the actual file. Method-1 extents also need bounds against their `idat` box.
- `leading_boxes` stops at the first `mdat`. Unsupported box ordering should
  produce a deliberate fallback or error, rather than an accidental result.
- The probe can describe a native YUV image before `decode` rejects a grid or
  split extents. Comments about falling back to `image` must match the actual
  routing: a successful native probe is not itself a fallback decision.

## intended change

1. Use the actual header size throughout box reads. Validate all additions,
   subtractions, offsets and lengths before indexing or allocation. Check
   declared ranges against the file and their containing box as appropriate.
2. Keep metadata reads bounded. Report malformed recognized containers as
   useful errors, and distinguish them from valid but unsupported layouts.
3. Introduce an explicit native-decode eligibility decision for grids, multiple
   extents and construction methods. A fallback must advertise the format it
   really decodes; it must not retain a YUV probe while returning RGB pixels.
4. Support extra arrangements only with fixtures and a documented benefit.
   Full grid assembly is separate from correcting the current error/fallback
   behavior; it is not required to land the bounds fixes.

## validation

Add synthetic small boxes for normal, extended and zero sizes; partial headers;
overflowing/out-of-file extents; multiple extents; and `idat` bounds. Test
unsupported layouts through both probe and decode. Malformed inputs must return
errors without panic, huge allocation, hangs or process abort. Release uses
`panic = "abort"`, so checked error paths matter even if debug tests panic.

Run a bounded fuzz/property-test campaign over pure parsing helpers, with a
seed corpus of valid containers and recorded regressions. Use subprocess limits
for native-decoder integration. Close [15](15-avif-decoder-progress.md) before
running a broad malformed-file integration campaign.
