# 13 — expose embedded icc profiles

- status: design only
- touches: `src/source.rs`, `src/decoder.rs`, `src/formats/`, `src/color.rs`,
  fixtures, `tests/readalpha.vpy`, `README.md`, and `AGENTS.md`
- expected: `icc_profile=False` keeps the current frame properties;
  `icc_profile=True` attaches the source's embedded ICC bytes as the binary
  `ICCProfile` frame property
- risk: medium — it adds profile extraction to every reader, but does not
  change decoded pixels, formats, planes, or color metadata

## decision

The source plugin should not apply ICC transforms itself. It should expose the
embedded profile to the graph and let a color-management filter decide how to
use it.

`VapourSynth-ICCConvert` already follows this model: its `Convert` filter can
read an `ICCProfile` frame property, and its `Tag` filter writes one. The
plugin can therefore interoperate with an existing VapourSynth workflow without
adding LittleCMS or another ICC engine to this project.

The proposed source option is a boolean, not a target color-space selector:

```python
clip = core.imgseqs.Read(
    files=files,
    icc_profile=True,
)

managed = iccc.Convert(
    clip,
    display_icc="srgb",
    prefer_props=True,
)
```

`icc_profile=False` is the default and keeps the current property contract.
When enabled, a file that carries an ICC profile gets the profile bytes on every
frame decoded from that file. A file without an embedded profile gets no
`ICCProfile` property. `ImgSeqHasICC` remains available and keeps its existing
meaning regardless of the option.

This plan deliberately does not add `apply_icc="srgb"` or another target-name
API. Applying a profile requires a target, rendering intent, black-point
compensation, and possibly proofing settings. Those are color-management
filter policy, not source-reader policy. If the project later needs a built-in
conversion path, it should be a separate plan or filter with its own explicit
contract.

## why a frame property

VapourSynth frames have a property map that can carry arbitrary binary data.
`ICCProfile` is the interoperability name used by ICCConvert, and the profile
must be stored as raw ICC bytes rather than as a path or a string. This keeps
the frame self-contained and works when the source file is moved or deleted
after the clip is created.

The property is per frame because an image sequence can contain different
profiles in different files. A downstream filter can therefore choose a
transform for each frame, cache transforms by profile hash, or reject a
sequence whose profiles do not match.

The profile property is metadata only:

- no pixels are modified;
- RGB, gray, and native YUV formats remain unchanged;
- alpha is unaffected because no pixel transform occurs;
- `_Primaries`, `_Transfer`, `_Matrix`, `_Range`, and `_ChromaLocation` keep the
  existing CICP/container rules;
- `ImgSeqHasICC` continues to report whether the source carries a profile;
- `ICCProfile` is absent when extraction is disabled or the source has no ICC.

## option and clip rules

- `icc_profile` is optional and defaults to `False`;
- only `False`/`0` and `True`/nonzero integer values are accepted;
- enabling it never changes the clip's video format or dimensions;
- `ReadAlpha` exposes the same `ICCProfile` property on its color frames and
  its alpha frames, while the alpha pixels remain untouched;
- the property is copied into each output frame, including prefetched frames;
- the source must preserve the profile bytes exactly, without converting them
  into CICP values or rewriting the profile;
- a profile that can be extracted but is not understood by the source is still
  exposed as bytes; interpretation belongs to the downstream color filter;
- extraction failures for a profile that is structurally unavailable should be
  reported during probing rather than silently setting `ImgSeqHasICC` alone.

The option can remain an integer in the existing VapourSynth argument string,
unlike the earlier target-name proposal. This keeps the filter API consistent
with `apply_rotation`, `debug`, and `mismatch`.

## profile extraction

`ImageInfo` currently stores only `has_icc_profile`. It should gain an optional
shared byte buffer, such as `Arc<[u8]>`, so the profile is read once during
probing and can be attached to frames without reopening the source file or
copying the bytes into every internal image record.

| reader | current state | required change |
| --- | --- | --- |
| `image` formats | `icc_profile()` is queried during probe and discarded | retain the returned profile bytes |
| HEIF/HEIC | libheif exposes raw profile data and presence | copy the selected image's raw ICC profile |
| AVIF | the box walk recognizes `prof`/`rICC` | retain the primary item's profile payload |
| JPEG 2000 | the JP2 box walk recognizes ICC `colr` | retain the profile bytes, not only the method flag |
| JPEG XL | `embedded_color_profile()` distinguishes ICC | retain the ICC variant instead of only setting a boolean |

The format modules must preserve the exact profile payload expected by ICC
consumers. Container headers and profile-box type markers are not part of the
ICC profile itself unless the consumer convention explicitly requires them.
The extraction tests should compare the emitted property bytes with the source
profile bytes, not only check that the property exists.

## frame-property details

The source should set the property as binary data with the exact key
`ICCProfile`. It should not use a path, base64 encoding, or a custom property
name that ICCConvert cannot discover.

`ImgSeqHasICC` and `ICCProfile` have different purposes:

```text
ImgSeqHasICC = 1   profile exists in the source, whether export is enabled or not
ICCProfile        raw profile bytes, only when export is enabled
```

The profile should be attached before the existing color properties so a
downstream filter sees one complete metadata map. No `ImgSeqICCApplied`
property is needed because this source never performs the transform.

## validation

The test set needs small images with embedded profiles and matching unprofiled
controls. The first batch should cover:

- `icc_profile=False` preserving the current frame-property output;
- `icc_profile=True` producing an exact binary `ICCProfile` value;
- a file without ICC having `ImgSeqHasICC=0` and no `ICCProfile` property;
- the same profile being present on all frames decoded from one file;
- different profiles producing different per-frame property bytes;
- extraction from every reader that claims support;
- RGB, gray, YUV, alpha, 8-bit, 16-bit, and float sources remaining pixel- and
  format-identical to the disabled path;
- `ReadAlpha` exposing the property without changing the alpha plane;
- profile bytes remaining available after the source path is no longer used by
  the frame request;
- downstream ICCConvert consuming the property with `prefer_props=True`.

The property test should compare bytes against an independent container
extraction tool or the profile file used to create the fixture. The disabled
and enabled paths should use frame parity for pixels and formats; the only
expected difference is the optional binary property.

No ICC transform benchmark is needed for this plan. The relevant measurement
is probe overhead and the additional frame-property memory, especially when a
sequence contains many distinct profiles.

## open decisions

- whether the default should remain `False` permanently or become `True` after
  the property is proven interoperable;
- the exact raw-profile accessors and container payload offsets for HEIF, AVIF,
  and JPEG 2000;
- whether an oversized or malformed profile should be rejected, capped, or
  passed through unchanged;
- whether profile bytes should be deduplicated across `ImageInfo` values by
  hash in addition to using `Arc`;
- whether profile extraction should be exposed in `ReadAlpha`'s alpha frames
  if that clip is ever split into independently sourced metadata.

## reference

- [VapourSynth-ICCConvert](https://github.com/YomikoR/VapourSynth-ICCConvert)
  documents the `ICCProfile` property and the `prefer_props` behavior.
- VapourSynth frame properties support arbitrary binary data, which is the
  transport required for an embedded ICC profile.
