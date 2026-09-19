# 06 — jpeg 2000 backend

- status: implemented
- touches: `Cargo.toml`, `README.md`, `THIRD_PARTY_NOTICES`, `LICENSES/`,
  `src/formats/jp2.rs` (new), `src/formats/mod.rs`, `src/decoder.rs`, fixtures,
  `tests/make-alpha-fixtures.py`, `tests/readalpha.vpy`
- expected: a `.jp2`/`.j2k` file reads as gray or RGB at its stated depth instead
  of failing the probe, and the probe costs a header walk rather than a decode
- risk: medium — a vendored native decoder and a format path with no sandbox set
  behind it

## problem

`docs/IMPLEMENTATION.md` names jpeg 2000 in two places. The backend plan has it
as one of the decoders that do not go through `image-rs`:

```text
JPEG 2000
└── jpeg2k
    └── OpenJPEG
```

and `# Initial Implementation Scope` lists the formats the first version should
read. The build has neither: `Cargo.toml` names no jpeg 2000 crate,
`src/formats/` is `heif.rs`, `webp.rs` and `mod.rs`, and `README.md`'s input
list does not mention the format. `image` has no jpeg 2000 codec at all, so no
feature flag can add one — this is the only backend the plan names that has no
route through `image`, and therefore the only one where the doc describes a
decoder that cannot be switched on.

A `.jp2` file fails the probe today as a file whose format cannot be
determined, and there is no set under `sandbox/` to measure against. The cost of
the change is the one number this plan cannot quote before it is written.

## change, as planned

**the dependency.** `jpeg2k` 0.10.1 (MIT/Apache-2.0 for the wrapper) uses
`openjpeg-sys` 1.0.12, which compiles the OpenJPEG sources it vendors. No
vcpkg or pkg-config entry is needed; the plugin contains that static decoder.
`THIRD_PARTY_NOTICES` and `LICENSES/` carry the vendored OpenJPEG BSD-2-Clause
text.

**the module.** `src/formats/jp2.rs`, picked by extension (`jp2`, `j2k`, `jpf`,
`jpx`, `j2c`) and by the signature the file starts with: a `jP  ` box for a
container, `FF 4F FF 51` for a bare codestream. It exposes the same two entry
points as `heif.rs` and `webp.rs`:

- `handles(info)` — the extension selects the module, and the probe validates
  the JP2 or codestream magic before accepting the file.
- `image_info(path)` — the probe, from the header. A container states the size
  and the component types in `ihdr` (height, width, component count, bit depth)
  and the colour space in `colr` (16 `sRGB`, 17 `greyscale`, 18 `sYCC`); a bare
  codestream states them in its `SIZ` marker, which is also where `dx`/`dy` per
  component live. This is the [05](05-monochrome-heif.md) lesson applied on
  purpose: the probe never builds a decoder for a file whose header already
  answers it, and `decoder::decode` keeps comparing width, height and colour
  type against the probe, so a header that lies fails the frame instead of
  producing a wrong clip.
- `decode(info)` — through the crate, into `Pixels::Interleaved` for a file the
  `colr` box calls rgb or grey, and into the planar `Pixels` of
  [03](03-webp-yuv-output.md) for a file whose sYCC header names a supported
  sampling layout and depth.

**the formats.** `prec <= 8` is `Gray8`/`RGB24`; deeper components use the
nominal `Gray9`–`Gray16` or `RGB27`–`RGB48` format, with the decoder's wider
word shifted down by the existing pixel writer. Jpeg 2000 has no alpha channel, so
`ReadAlpha` answers the opaque gray plane it answers for every other source
without one, and a component count above three (cmyk, or an alpha-less
`colr`-less codestream with four components) fails loudly rather than being
guessed.

**what it replaces.** Nothing: the format is unreachable today, so there is no
regression surface. The frame path, the lookahead pool and the property writer
are untouched.

## validation

- fixtures, encoded once by hand and committed, the way the avif and heic ones
  are: 8-bit grey, 8-bit rgb, 16-bit rgb (reversible 5/3), one lossy 9/7, and
  one bare `.j2k` codestream. `opj_compress` writes all five; the commands are
  recorded in `tests/make-alpha-fixtures.py`.
- `tests/readalpha.vpy` gains a table row per fixture: the four container files
  in one `mismatch=True` clip with the format each of them must land on, and the
  bare codestream beside them. That is the same shape the monochrome container
  table has now.
- `cargo test --locked`, `cargo fmt`, `cargo clippy --workspace --all-targets
  --locked -- -D warnings`.
- a wheel build, then `dumpbin /dependents` on `vs_imageseqs.dll` (and the CI's
  `otool -L`/`readelf -d`) to confirm what OpenJPEG cost: an import, or nothing.
- the per stage table in [BENCH.md](../BENCH.md) is where the decode cost would
  land. It cannot be filled until a set exists; a hundred-file jp2 set, or a
  `.jp2` copy of an existing sandbox set, is what makes this plan measurable
  rather than merely correct.

## left over

- **`sYCC` is not automatically `YUV420P8`.** the yuv path covers 4:2:0 and
  4:2:2 at 8/10 bits, and 4:4:4 at 8/10/12 bits; unsupported sYCC sampling or
  depth is converted to RGB.
- **`jpx`/`jp2` metadata that is not read**: `pclr` palettes, `cdef` channel
  definitions, `res`/`resc` resolutions and the `uuid` boxes. A file that needs
  one of them to be interpreted correctly is out of scope; the colour space in
  `colr` and the component types in `ihdr` are in.
