# 07 — dds and farbfeld

- status: implemented
- touches: `Cargo.toml`, `README.md`, `docs/IMPLEMENTATION.md` (only if the
  write-up moves), fixtures, `tests/readalpha.vpy`
- expected: `.dds` and `.ff` files stop failing the probe, for two feature flags
  and no native code
- risk: low

## problem

`### image-rs Configuration` in `docs/IMPLEMENTATION.md` prints the `image`
feature list the plugin is supposed to carry, `dds` and `ff` among the format
features:

```toml
    "bmp",
    "dds",
    "exr",
    "ff",
```

and the backend plan tree directly above it lists `DDS` and `Farbfeld` beside
PNG and JPEG. `Cargo.toml` enables neither, so both extensions fail the probe
the way any unregistered format does, and the doc's tree describes a decoder set
the build does not have. That is the whole gap: no module, no native
dependency, no correction of a decoder's output — the two codecs are already in
the `image` crate this plugin compiles against.

## change, as planned

- add `"dds"` and `"ff"` to the `image` feature list in `Cargo.toml`. Confirm
  the two names against `image` 0.25.10 first: they are the doc's claim and not
  the manifest's, and a feature name that moved between versions fails the build
  rather than silently dropping the codec.
- add `dds` and `farbfeld` to the input list in `README.md`.
- nothing else. `decoder::probe` keeps its shape: `ImageReader` sniffs the
  signature, the decoder reports dimensions and colour type,
  `PixelFormat::from_color_type` decides the format, and `src/formats/` is only
  involved if a probe shows `image` reports something wrong for one of the two —
  which is what [05](05-monochrome-heif.md) turned out to be for monochrome
  heif, and is not expected here.
- the formats land where the colour type says. Farbfeld is 16-bit rgba by
  definition, so a `.ff` file becomes `RGB48` plus a separate alpha clip, or
  `La16`/`Gray16` where the alpha is opaque everywhere and the source reads as
  grey. Dds is whatever its fourcc and pixel masks mean; `image` reports a
  colour type for it and `PixelFormat::from_color_type` decides from there. The
  first probe decides whether that is the end of it or whether the colour type
  it reports is one the writer does not accept (see the left over below).
- the two codecs are compiled into the plugin's one `image` build, so the dll
  grows by their code. No new native library, no change to `THIRD_PARTY_NOTICES`
  or `LICENSES/`: `image` and its codecs are already covered there.

## validation

- fixtures written by hand, in the spirit of `tests/make-alpha-fixtures.py`:
  farbfeld is the magic `farbfeld`, a big-endian width and height, then
  16-bit big-endian rgba rows — about thirty lines of python with no encoder
  involved. An uncompressed 32-bit dds is a 128-byte header and the rows, which
  the same script can write with `struct.pack` (fourcc 0, the rgb and alpha
  pixel flags, a 32-bit bgra mask). Use a 64×64 crop of an existing fixture so
  both files stay a few hundred bytes.
- `tests/readalpha.vpy` gains a row per fixture, the way the monochrome
  container table reads now: the format each file must land on, and the alpha
  clip's type.
- `cargo test --locked`, `cargo fmt`, `cargo clippy --workspace --all-targets
  --locked -- -D warnings`.
- `cargo tree -e features -i image` to confirm both codecs are compiled in
  rather than only declared, and a wheel build plus `dumpbin /dependents` (or
  the CI's `otool -L`/`readelf -d` assertions) to confirm no new native import
  appeared.

## left over

- neither format has a folder in `sandbox/`, so a real-world file — a compressed
  dds from a game, a farbfeld dump from `png2ff` — is untested until one is
  added. Two hand-written files prove the routing and the format mapping, not
  the codecs, which is the same thing the png and jpeg fixtures prove for their
  formats.
- dds is a container for far more than images (mipmap chains, cubemaps, volume
  textures). `image` reads the top mip of the first face; that is the only
  interpretation a one-frame-per-file reader can use, and it is worth one line
  in `README.md`'s input list when the format is claimed.
- **a colour type the writer does not accept.** `PixelFormat::from_color_type`
  takes the ten types the frame writer can fill, and a dds built from pixel
  masks (rather than a dxt fourcc) is the kind of file that makes a decoder
  report something else — `Bgra8` is the candidate, since the format stores its
  channels in that order. `decoder::probe` fails loudly on an unsupported colour
  type today, so the failure would be visible rather than wrong; the fix would be
  a format correction in `src/formats/` the way
  [05](05-monochrome-heif.md) corrected a monochrome avif, not a new
  `PixelFormat`. Write the fixture with masks if the goal is to find out.

The `image` 0.25.10 DDS backend currently accepts DXT1, DXT3 and DXT5 files;
it does not accept uncompressed mask-based DDS files. The fixture uses one DXT5
block so the supported path is covered without adding a native dependency.
