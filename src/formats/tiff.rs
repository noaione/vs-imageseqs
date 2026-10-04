//! TIFF, read through the `tiff` crate rather than through `image`.
//!
//! The plan's row for this format promotes `tiff 0.11.3` from a dependency of
//! `image` to a direct one, so this module is a translation layer rather than a
//! port: the crate reads the container and this file decides what the samples
//! mean and how they reach a frame.
//!
//! Five things are the rule, and the first four are places the obvious reading
//! is wrong:
//!
//! - **The decompressors are named in `Cargo.toml`, not inherited.** The crate's
//!   defaults are `deflate`, `fax`, `jpeg` and `lzw`; this tree takes the crate
//!   without its defaults and names all four plus `zstd`, which is not a default.
//!   Naming them makes what this reader can read visible in one place, and the
//!   one feature left out is `webp`, because a tiff is never a webp.
//! - **A four channel file maps to the format of its own depth.** `Rgba8` is
//!   `Rgb8` and `Rgba16` is `Rgb16`; only `Rgba32F` is a float frame. The alpha
//!   itself lands on its own clip, so the colour frame is three channels
//!   whatever the file held.
//! - **`Decoder::read_image` must not be used.** It calls
//!   `result_extent_for_planes(0..1)` -- literally one plane -- so a planar file
//!   comes back as its first sample's plane alone. That has the right shape and
//!   the wrong colours.
//! - **`read_image_to_buffer` is necessary and not sufficient.** It reads every
//!   plane, and it leaves them **laid end to end**: the buffer is
//!   `[plane 0][plane 1][plane 2]`, not `rgb rgb rgb`. The
//!   [`BufferLayoutPreference`] it returns is what says so, through `planes` and
//!   `plane_stride`, and [`interleave`] is what turns that back into a frame.
//!   Reading all the planes and writing them out as if they were already
//!   interleaved gives a picture of the right size with the wrong colours, which
//!   is the failure this reader must not have.
//! - **A palette page is refused by name**, which is what the reader this
//!   replaces did. The refusal happens at *identify*, so the probe cannot
//!   promise a frame the decode would refuse to produce.
//!
//! The sample count is checked against the header before anything is handed out.
//! That is a guard rather than a formality: `read_image_to_buffer` falls back to
//! one plane when the file's own size exceeds the decoder's buffer limit.
//!
//! [`BufferLayoutPreference`]: tiff::decoder::BufferLayoutPreference

use std::path::Path;

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// What a header's colour type says the samples are.
struct Layout {
    /// Channels one pixel holds in the buffer the decoder fills, which is not
    /// the frame's own count when the samples are separated inks.
    channels: usize,
    /// Bytes one sample occupies.
    sample_bytes: usize,
    /// The layout a frame is written from.
    color_type: ColorType,
    /// The label property's value.
    source: SourceColorType,
    /// Whether the samples are floats rather than integers.
    float: bool,
    /// Whether the samples are separated inks rather than channels, which a
    /// frame holds as rgb and, when there is a fifth sample, as alpha.
    separated: bool,
    /// The width of one index, when the samples are indices into a colour table
    /// rather than colours. The pinned decoder refuses this photometric before it
    /// will name a colour type, so the reader expands the table itself.
    palette: Option<u8>,
}

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    // Content first: a file whose bytes say it is something else is that
    // something else however it is named, and the extension is the hint a
    // format with no signature of its own has to fall back on. See
    // [`identify::owns`](crate::formats::identify::owns).
    crate::formats::identify::owns(crate::formats::identify::Format::Tiff, path)
}

/// Reads the layout a tiff colour type stands for.
///
/// # Errors
///
/// Returns [`ImgSeqError`] for a colour type this reader does not take, named
/// so that an unported subtype is an error rather than a wrong picture.
fn layout(kind: tiff::ColorType) -> Result<Layout> {
    use tiff::ColorType as Tiff;
    // The sample width is the number the colour type carries, and it is the only
    // thing that decides how wide a sample is.
    let (bits, channels, color_type, source, separated) = match kind {
        Tiff::Gray(bits @ (8 | 16 | 32)) => {
            let (color, source) = match bits {
                8 => (ColorType::L8, SourceColorType::L8),
                _ => (ColorType::L16, SourceColorType::L16),
            };
            (bits, 1, color, source, false)
        }
        Tiff::GrayA(bits @ (8 | 16)) => {
            let (color, source) = if bits == 8 {
                (ColorType::La8, SourceColorType::La8)
            } else {
                (ColorType::La16, SourceColorType::La16)
            };
            (bits, 2, color, source, false)
        }
        Tiff::RGB(bits @ (8 | 16 | 32)) => {
            let (color, source) = match bits {
                8 => (ColorType::Rgb8, SourceColorType::Rgb8),
                16 => (ColorType::Rgb16, SourceColorType::Rgb16),
                _ => (ColorType::Rgb32F, SourceColorType::Rgb32F),
            };
            (bits, 3, color, source, false)
        }
        Tiff::RGBA(bits @ (8 | 16 | 32)) => {
            let (color, source) = match bits {
                8 => (ColorType::Rgba8, SourceColorType::Rgba8),
                16 => (ColorType::Rgba16, SourceColorType::Rgba16),
                _ => (ColorType::Rgba32F, SourceColorType::Rgba32F),
            };
            (bits, 4, color, source, false)
        }
        // Separated inks: c, m, y and k, which a frame holds as three channels.
        // The label is the ink model either way, because that is the encoding the
        // file holds; the alpha of a five sample file is a plane of its own and
        // not part of the label.
        Tiff::CMYK(bits @ (8 | 16)) => {
            let (color, source) = if bits == 8 {
                (ColorType::Rgb8, SourceColorType::Cmyk8)
            } else {
                (ColorType::Rgb16, SourceColorType::Cmyk16)
            };
            (bits, 4, color, source, true)
        }
        Tiff::CMYKA(bits @ (8 | 16)) => {
            let (color, source) = if bits == 8 {
                (ColorType::Rgba8, SourceColorType::Cmyk8)
            } else {
                (ColorType::Rgba16, SourceColorType::Cmyk16)
            };
            (bits, 5, color, source, true)
        }
        Tiff::Palette(_) => {
            return Err(ImgSeqError::new(
                "a palette tiff is not a subtype this reader takes",
            ));
        }
        other => {
            return Err(ImgSeqError::new(format!(
                "the tiff colour type {other:?} is not one this reader takes"
            )));
        }
    };
    Ok(Layout {
        channels,
        sample_bytes: if bits == 32 { 4 } else { usize::from(bits / 8) },
        color_type,
        source,
        float: bits == 32,
        separated,
        palette: None,
    })
}

/// The layout a directory states, whether or not the crate will name it.
///
/// A palette is the one photometric the pinned decoder refuses before it names a
/// colour type, and its whole chunk reader goes through that name, so it is
/// recognised from the tag and described here instead.
///
/// # Errors
///
/// Returns [`ImgSeqError`] for a directory this reader cannot describe.
fn layout_of<R: std::io::Read + std::io::Seek>(
    decoder: &mut tiff::decoder::Decoder<R>,
    path: &Path,
    action: &str,
) -> Result<Layout> {
    // 3 is `RGBPalette`; see `tiff::tags::PhotometricInterpretation`.
    let palette = decoder
        .get_tag_unsigned::<u16>(tiff::tags::Tag::PhotometricInterpretation)
        .is_ok_and(|code| code == 3);
    if palette {
        return layout_palette(decoder, path, action);
    }
    layout(
        decoder
            .colortype()
            .map_err(|error| image_error(action, path, error))?,
    )
    .map_err(|error| image_error(action, path, error))
}

/// The layout of a palette page, from the two tags that describe it.
///
/// `BitsPerSample` is the width of one index and `ColorMap` holds three tables
/// of `2**bits` sixteen-bit entries, red first, so the table has to be exactly
/// three times the entry count. A table of another length states an index that
/// has no colour, which is refused rather than read as though the entries a file
/// left out were black.
fn layout_palette<R: std::io::Read + std::io::Seek>(
    decoder: &mut tiff::decoder::Decoder<R>,
    path: &Path,
    action: &str,
) -> Result<Layout> {
    let bits = decoder
        .get_tag_unsigned::<u16>(tiff::tags::Tag::BitsPerSample)
        .map_err(|error| image_error(action, path, error))?;
    let bits = u8::try_from(bits).unwrap_or(0);
    if !palette_supported(bits) {
        return Err(image_error(
            action,
            path,
            format!("a palette of {bits} bit indices is not a width this reader takes"),
        ));
    }
    let entries = 1usize << bits;
    let colour_map = decoder
        .get_tag_u16_vec(tiff::tags::Tag::ColorMap)
        .map_err(|error| image_error(action, path, error))?;
    if colour_map.len() != 3 * entries {
        return Err(image_error(
            action,
            path,
            format!(
                "a palette of {entries} entries states {} colour values",
                colour_map.len()
            ),
        ));
    }
    let depth = palette_depth(bits);
    // The label is the layout the samples decode to rather than the indices the
    // file holds, which is what a palette png already reports: the png reader this
    // tree replaced did not override the type its samples expanded to either.
    let (color_type, source) = if depth == 8 {
        (ColorType::Rgb8, SourceColorType::Rgb8)
    } else {
        (ColorType::Rgb16, SourceColorType::Rgb16)
    };
    Ok(Layout {
        // One index a pixel, which this reader expands itself.
        channels: 1,
        sample_bytes: 1,
        color_type,
        source,
        float: false,
        separated: false,
        palette: Some(bits),
    })
}

/// The widths of one index this reader takes.
///
/// One, two, four and eight are the widths a byte divides into and the widths
/// libtiff reads a palette at, and every writer to hand produces one of them:
/// `ImageMagick` writes one bit for two colours, two for four, four for sixteen
/// and eight for anything larger. Three, five, six and seven are widths a byte
/// does not divide into and no writer here produces.
///
/// Sixteen is a legal width and is refused too. No tool on hand writes one, and
/// the one written by hand was refused by libtiff, so the byte order of a
/// sixteen-bit index stream has no reference to be checked against and a wrong
/// answer there would still look like a picture. The same goes for nine to
/// fifteen. Widening this is what makes a wider palette readable, and
/// [`palette_depth`] already names the depth each of those would be handed out
/// at.
fn palette_supported(bits: u8) -> bool {
    matches!(bits, 1 | 2 | 4 | 8)
}

/// The depth a palette of `bits`-wide indices is handed out at.
///
/// The colormap holds sixteen-bit values whatever the width of an index is, so
/// a frame is written at the narrowest depth that names the entries: eight for
/// indices of eight bits or fewer, then ten, twelve and sixteen for the widths
/// between. Only the first is reachable while [`palette_supported`] admits the
/// widths a byte divides into, which is the point of keeping the answer here:
/// admitting a width needs no other change.
fn palette_depth(bits: u8) -> u32 {
    match bits {
        0..=8 => 8,
        9..=10 => 10,
        11..=12 => 12,
        _ => 16,
    }
}

/// One tag's values as numbers, at whichever width the file wrote them.
///
/// A tiff may state an offset or a count as a short or as a long, so the value
/// is flattened rather than asked for at one width.
fn tag_numbers<R: std::io::Read + std::io::Seek>(
    decoder: &mut tiff::decoder::Decoder<R>,
    tag: tiff::tags::Tag,
    path: &Path,
    action: &str,
) -> Result<Vec<u64>> {
    let value = decoder
        .get_tag(tag)
        .map_err(|error| image_error(action, path, error))?;
    let mut numbers = Vec::new();
    flatten(&value, &mut numbers);
    Ok(numbers)
}

/// Every number inside one tag value, in the order the file wrote them.
fn flatten(value: &tiff::decoder::ifd::Value, out: &mut Vec<u64>) {
    use tiff::decoder::ifd::Value;
    match value {
        Value::Byte(number) => out.push(u64::from(*number)),
        Value::Short(number) => out.push(u64::from(*number)),
        Value::Unsigned(number) => out.push(u64::from(*number)),
        Value::UnsignedBig(number) => out.push(*number),
        Value::List(items) => items.iter().for_each(|item| flatten(item, out)),
        // A rational or a float in a tag this reads is a directory that does not
        // describe a raster, and the caller sees the value it did not get.
        _ => {}
    }
}

/// The format a frame is written from.
///
/// A four channel source is separated into a three channel frame and an alpha
/// clip, so the alpha's existence does not make the colour frame a wider one.
fn format(layout: &Layout) -> PixelFormat {
    let base = match (layout.color_type, layout.sample_bytes) {
        (ColorType::L8 | ColorType::La8, _) => PixelFormat::Gray8,
        (ColorType::L16 | ColorType::La16, 4) => PixelFormat::Gray32F,
        (ColorType::L16 | ColorType::La16, _) => PixelFormat::Gray16,
        (ColorType::Rgb8 | ColorType::Rgba8, _) => PixelFormat::Rgb8,
        (ColorType::Rgb16 | ColorType::Rgba16, _) => PixelFormat::Rgb16,
        _ => PixelFormat::Rgb32F,
    };
    // A palette's indices are their own width whatever the table holds, so the
    // depth a frame is written at comes from the table rather than the sample.
    match layout.palette {
        Some(bits) => base.at_depth(palette_depth(bits)),
        None => base,
    }
}

/// Opens a tiff and reports what it states.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info(path: &Path, apply_rotation: bool) -> Result<Option<ImageInfo>> {
    if !owns(path) {
        return Ok(None);
    }
    // The file itself, not a header buffer: a tiff's IFD can sit anywhere in the
    // file and the crate seeks to it, so a truncated head reads as "failed to
    // fill whole buffer". Handing it the file means the crate reads only the
    // directory and the tags it needs, which is less work than a whole-file read
    // and less than this module could work out for itself.
    let mut file = std::fs::File::open(path).map_err(|error| image_error("open", path, error))?;
    let mut signature = [0u8; 4];
    {
        use std::io::Read;
        let _ = file.read(&mut signature);
    }
    use std::io::Seek;
    file.rewind()
        .map_err(|error| image_error("open", path, error))?;
    // The signature is what decides, not the extension, so a `.tiff` that is
    // not one is declined rather than refused.
    // The central table, not a second copy of it: this gate accepted only the
    // classic signature, so a BigTIFF -- which the pinned crate decodes -- was
    // declined here however `identify` had routed it. A separate table is a
    // separate answer, which is how a probe and a decode drift apart.
    if crate::formats::identify::identify(&signature)
        != Some(crate::formats::identify::Format::Tiff)
    {
        return Ok(None);
    }

    let mut decoder = tiff::decoder::Decoder::new(std::io::BufReader::new(file))
        .map_err(|error| image_error("identify", path, error))?;
    let (width, height) = decoder
        .dimensions()
        .map_err(|error| image_error("identify", path, error))?;
    let layout = layout_of(&mut decoder, path, "identify")?;
    if width == 0 || height == 0 {
        return Err(ImgSeqError::new("the header states no pixels"));
    }

    // The orientation tag and the ICC profile are already in the directory the
    // crate read for the dimensions, so neither costs a second pass. Both were
    // being dropped: the plan records a file that states orientation 6 and
    // carries a profile coming out as orientation 1 with no profile.
    let orientation = decoder
        .get_tag_u32(tiff::tags::Tag::Orientation)
        .ok()
        .and_then(|code| u8::try_from(code).ok())
        .and_then(Orientation::from_exif)
        .unwrap_or(Orientation::NoTransforms);
    // 34675 is `InterColorProfile`, which the tag enum does not name.
    // The crate hands a byte array back as a list of single bytes, and a
    // one-byte profile as the byte itself.
    let icc_profile: Option<std::sync::Arc<[u8]>> = decoder
        .get_tag(tiff::tags::Tag::Unknown(34675))
        .ok()
        .and_then(|value| match value {
            tiff::decoder::ifd::Value::List(items) => items
                .into_iter()
                .map(|item| match item {
                    tiff::decoder::ifd::Value::Byte(byte) => Some(byte),
                    _ => None,
                })
                .collect::<Option<Vec<u8>>>(),
            tiff::decoder::ifd::Value::Byte(byte) => Some(vec![byte]),
            _ => None,
        })
        .filter(|bytes| !bytes.is_empty())
        .map(std::sync::Arc::from);

    Ok(Some(ImageInfo {
        path: path.to_path_buf(),
        width,
        height,
        color_type: layout.color_type,
        original_color_type: layout.source,
        has_icc_profile: icc_profile.is_some(),
        icc_profile,
        cicp: None,
        chroma_location: None,
        orientation,
        transform: if apply_rotation {
            Transform::from_orientation(orientation)
        } else {
            Transform::IDENTITY
        },
        format: format(&layout),
    }))
}

/// Decodes a tiff into one interleaved buffer.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or is malformed.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = std::time::Instant::now();
    let data = std::fs::read(&info.path).map_err(|error| image_error("open", &info.path, error))?;
    let open = open_started.elapsed();

    let mut decoder = tiff::decoder::Decoder::new(std::io::Cursor::new(&data))
        .map_err(|error| image_error("decode", &info.path, error))?;
    let (width, height) = decoder
        .dimensions()
        .map_err(|error| image_error("decode", &info.path, error))?;
    let layout = layout_of(&mut decoder, &info.path, "decode")?;
    if (width, height) != (info.width, info.height) {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {}x{}, now {}x{})",
            info.path.display(),
            info.width,
            info.height,
            width,
            height,
        )));
    }

    let read_started = std::time::Instant::now();
    let mut buffer = if let Some(bits) = layout.palette {
        // A palette is read here rather than by the decoder, which refuses the
        // photometric before it can describe a chunk.
        palette_buffer(&mut decoder, &data, &info.path, width, height, bits)?
    } else {
        // Every plane, not the first one: see the note at the head of the file.
        let mut result = match (layout.sample_bytes, layout.float) {
            (1, _) => tiff::decoder::DecodingResult::U8(Vec::new()),
            (2, _) => tiff::decoder::DecodingResult::U16(Vec::new()),
            _ => tiff::decoder::DecodingResult::F32(Vec::new()),
        };
        let preference = decoder
            .read_image_to_buffer(&mut result)
            .map_err(|error| image_error("decode", &info.path, error))?;
        let expected = (width as usize) * (height as usize) * layout.channels;
        let mut buffer = to_bytes(result, &layout, expected)
            .map_err(|error| image_error("decode", &info.path, error))?;
        // The planes arrive one after another, and a frame is interleaved. This
        // is the step that separates a correct picture from one of the right
        // size.
        if preference.planes > 1 {
            let stride = preference
                .plane_stride
                .map_or(0, std::num::NonZeroUsize::get);
            buffer = interleave(&buffer, &layout, stride)
                .map_err(|error| image_error("decode", &info.path, error))?;
        }
        buffer
    };
    // The separated inks are four samples that a frame holds as three channels,
    // or five as three plus the alpha plane. This conversion is the reader's
    // rather than the container's, so it happens once over the whole picture and
    // its cost belongs to the read.
    if layout.separated {
        buffer = inks_to_channels(&buffer, &layout)
            .map_err(|error| image_error("decode", &info.path, error))?;
    }
    let read = read_started.elapsed();

    Ok(DecodedImage {
        width,
        height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Interleaved {
            color_type: layout.color_type,
            buffer,
        },
        timings: DecodeTimings {
            open,
            metadata: std::time::Duration::ZERO,
            buffer: std::time::Duration::ZERO,
            read,
        },
    })
}

/// Turns the crate's samples into the native-endian bytes a frame is written
/// from.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the samples are a type or a count the header did
/// not describe, which means the reader fell back to one plane.
fn to_bytes(
    result: tiff::decoder::DecodingResult,
    layout: &Layout,
    expected: usize,
) -> Result<Vec<u8>> {
    use tiff::decoder::DecodingResult as D;
    let counted = match &result {
        D::U8(samples) => samples.len(),
        D::U16(samples) => samples.len(),
        D::F32(samples) => samples.len(),
        _ => 0,
    };
    if counted != expected {
        return Err(ImgSeqError::new(format!(
            "the raster holds {counted} samples where {expected} belong, so it was read as one plane"
        )));
    }
    match (result, layout.sample_bytes, layout.float) {
        (D::U8(samples), 1, false) => Ok(samples),
        (D::U16(samples), 2, false) => Ok(samples
            .into_iter()
            .flat_map(u16::to_ne_bytes)
            .collect::<Vec<u8>>()),
        (D::F32(samples), 4, true) => Ok(samples
            .into_iter()
            .flat_map(f32::to_ne_bytes)
            .collect::<Vec<u8>>()),
        (other, _, _) => Err(ImgSeqError::new(format!(
            "the raster holds samples the header did not describe: {other:?}"
        ))),
    }
}

/// Reorders plane-major samples into the interleaved order a frame is written
/// from.
///
/// The buffer holds one plane after another, each `stride` bytes of samples in
/// row-major order, and a frame holds the channels of one pixel together. For a
/// three channel picture the first reads `rrr...ggg...bbb...` and the second
/// wants `rgb rgb rgb ...`, which is a transpose of the sample grid rather than
/// a copy.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when a plane does not hold a whole picture, which
/// means the stride and the header disagree.
fn interleave(buffer: &[u8], layout: &Layout, stride: usize) -> Result<Vec<u8>> {
    let channels = layout.channels;
    let width = layout.sample_bytes;
    let samples = buffer.len() / (channels * width);
    if stride < samples * width || stride * channels > buffer.len() {
        return Err(ImgSeqError::new(
            "a plane does not hold a whole picture, so the stride and the header disagree",
        ));
    }
    let mut out = vec![0u8; buffer.len()];
    for channel in 0..channels {
        let plane = &buffer[channel * stride..channel * stride + samples * width];
        for sample in 0..samples {
            let from = sample * width;
            let to = (sample * channels + channel) * width;
            out[to..to + width].copy_from_slice(&plane[from..from + width]);
        }
    }
    Ok(out)
}

/// Expands a palette page's indices into the channels a frame holds.
///
/// The indices come from the strips here because the decoder refuses this
/// photometric before it can describe a chunk. A row of them is padded to a byte
/// boundary, and the colormap's entries are sixteen-bit values, so a frame at
/// eight bits takes the high byte of each -- which is what libtiff hands out and
/// what Pillow reads from the same file -- while a frame at a wider depth keeps
/// the whole value for the frame writer to move down.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the raster cannot be read or the table does not
/// describe every entry.
fn palette_buffer<R: std::io::Read + std::io::Seek>(
    decoder: &mut tiff::decoder::Decoder<R>,
    data: &[u8],
    path: &Path,
    width: u32,
    height: u32,
    bits: u8,
) -> Result<Vec<u8>> {
    let indices = palette_indices(decoder, data, path, width, height, bits)?;
    let colour_map = decoder
        .get_tag_u16_vec(tiff::tags::Tag::ColorMap)
        .map_err(|error| image_error("decode", path, error))?;
    let entries = 1usize << bits;
    if colour_map.len() != 3 * entries {
        return Err(image_error(
            "decode",
            path,
            "the colour table is not three times the entries the palette states",
        ));
    }
    let depth = palette_depth(bits);
    let sample_bytes = if depth == 8 { 1 } else { 2 };
    // An entry is sixteen bits and the frame names the depth it is written at,
    // so the bits at or above that depth are the ones a frame holds.
    let shift = 16 - depth;
    let mut out = Vec::with_capacity(indices.len() * 3 * sample_bytes);
    for index in &indices {
        // An index is `bits` wide and the table is three tables of `entries`,
        // so both reads below are inside it.
        let index = usize::from(*index);
        for channel in 0..3 {
            let value = colour_map[channel * entries + index] >> shift;
            if sample_bytes == 1 {
                out.push(u8::try_from(value).expect("eight bits of a sixteen bit entry"));
            } else {
                out.extend_from_slice(&value.to_ne_bytes());
            }
        }
    }
    Ok(out)
}

/// One index a pixel, in row order, from the strips of a palette page.
///
/// The raster is read here rather than by the decoder, which refuses this
/// photometric, so only an uncompressed strip is read: a compression this
/// reader does not decode is refused rather than read as though it were not
/// compressed. The strips are walked in the order the directory lists them and
/// the last one is not required to be full, which is what a file whose height
/// does not divide by its rows a strip states looks like.
///
/// # Errors
///
/// Returns [`ImgSeqError`] for a compressed or tiled palette, a strip that runs
/// past the end of the file, and a raster with fewer rows than the image states.
fn palette_indices<R: std::io::Read + std::io::Seek>(
    decoder: &mut tiff::decoder::Decoder<R>,
    data: &[u8],
    path: &Path,
    width: u32,
    height: u32,
    bits: u8,
) -> Result<Vec<u16>> {
    let compression = decoder
        .get_tag_unsigned::<u16>(tiff::tags::Tag::Compression)
        .unwrap_or(1);
    if compression != 1 {
        return Err(image_error(
            "decode",
            path,
            format!("a palette compressed with {compression} is not one this reader decodes"),
        ));
    }
    let offsets = tag_numbers(decoder, tiff::tags::Tag::StripOffsets, path, "decode")?;
    let counts = tag_numbers(decoder, tiff::tags::Tag::StripByteCounts, path, "decode")?;
    if offsets.is_empty() || offsets.len() != counts.len() {
        return Err(image_error(
            "decode",
            path,
            "the strip table does not hold one count for every offset",
        ));
    }
    let rows_per_strip = decoder
        .get_tag_unsigned::<u32>(tiff::tags::Tag::RowsPerStrip)
        .unwrap_or(height)
        .max(1) as usize;
    // A row of indices is padded to a byte boundary, however narrow an index is.
    let row_bits = (width as usize)
        .checked_mul(usize::from(bits))
        .ok_or_else(|| {
            image_error(
                "decode",
                path,
                "a row of indices is wider than this platform can count",
            )
        })?;
    let row_bytes = row_bits.div_ceil(8);
    let mut indices = Vec::new();
    let mut row = 0usize;
    for (offset, count) in offsets.iter().zip(&counts) {
        if row >= height as usize {
            break;
        }
        let offset = usize::try_from(*offset).unwrap_or(usize::MAX);
        let count = usize::try_from(*count).unwrap_or(usize::MAX);
        let strip = data
            .get(offset..offset.saturating_add(count))
            .ok_or_else(|| image_error("decode", path, "a strip runs past the end of the file"))?;
        let rows = rows_per_strip.min(height as usize - row);
        let needed = rows.checked_mul(row_bytes).ok_or_else(|| {
            image_error(
                "decode",
                path,
                "a strip is larger than this platform can count",
            )
        })?;
        if strip.len() < needed {
            return Err(image_error(
                "decode",
                path,
                "a strip holds fewer rows than the directory says it covers",
            ));
        }
        for inside in 0..rows {
            let start = inside * row_bytes;
            unpack_row(
                &strip[start..start + row_bytes],
                width as usize,
                bits,
                &mut indices,
            );
            row += 1;
        }
    }
    if row < height as usize {
        return Err(image_error(
            "decode",
            path,
            "the strips hold fewer rows than the image states",
        ));
    }
    Ok(indices)
}

/// Appends one row of packed indices, most significant bit first.
///
/// An index narrower than a byte is packed into the bits above the one before
/// it and the row is padded to a byte boundary, which is the order the
/// specification gives a one bit fax. The caller sized the row to hold every
/// index, so the reads below are inside it and a row that was shorter would read
/// as zero rather than panicking.
fn unpack_row(row: &[u8], width: usize, bits: u8, out: &mut Vec<u16>) {
    let bits = u32::from(bits);
    let mut at = 0usize;
    for _ in 0..width {
        let mut value = 0u16;
        for _ in 0..bits {
            let byte = row.get(at / 8).copied().unwrap_or(0);
            let shift = 7 - (at % 8);
            value = (value << 1) | u16::from((byte >> shift) & 1);
            at += 1;
        }
        out.push(value);
    }
}
/// Writes separated ink samples as the channels a frame holds.
///
/// This is the conversion the reader this tree replaced made, in the same `f32`
/// arithmetic and with the same truncation, so a cmyk tiff that was read before
/// that reader went reads the same now: `channel = (maximum - ink) * (maximum -
/// k) / maximum`, where zero is paper and the maximum is all of the ink. A fifth
/// sample is the alpha channel, which is a plane of its own and is copied rather
/// than converted.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the buffer does not hold whole pixels.
fn inks_to_channels(buffer: &[u8], layout: &Layout) -> Result<Vec<u8>> {
    let inks = layout.channels;
    let alpha = inks == 5;
    let width = layout.sample_bytes;
    let pixel = inks * width;
    if !buffer.len().is_multiple_of(pixel) {
        return Err(ImgSeqError::new(
            "the separated samples do not hold whole pixels",
        ));
    }
    let channels = if alpha { 4 } else { 3 };
    let mut out = Vec::with_capacity(buffer.len() / pixel * channels * width);
    for pixel in buffer.chunks_exact(pixel) {
        // The samples are c, m, y, k and, when there is one, alpha.
        let black = &pixel[3 * width..4 * width];
        for channel in 0..3 {
            let ink = &pixel[channel * width..(channel + 1) * width];
            if width == 1 {
                out.push(ink_to_byte(ink[0], black[0]));
            } else {
                let ink = u16::from_ne_bytes([ink[0], ink[1]]);
                let black = u16::from_ne_bytes([black[0], black[1]]);
                out.extend_from_slice(&ink_to_word(ink, black).to_ne_bytes());
            }
        }
        if alpha {
            out.extend_from_slice(&pixel[4 * width..5 * width]);
        }
    }
    Ok(out)
}

/// `(255 - ink) * (255 - k) / 255`, truncated, which is what the reader this
/// replaces wrote.
fn ink_to_byte(ink: u8, black: u8) -> u8 {
    let factor = 1. - f32::from(black) / 255.;
    ((255. - f32::from(ink)) * factor) as u8
}

/// The same at sixteen bits, where the maximum is 65535.
fn ink_to_word(ink: u16, black: u16) -> u16 {
    let factor = 1. - f32::from(black) / 65535.;
    ((65535. - f32::from(ink)) * factor) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn read(name: &str) -> Vec<u8> {
        let info = image_info(&fixture(name), true)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
            .unwrap_or_else(|| panic!("{name} is taken over"));
        match decode(&info)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
            .pixels
        {
            Pixels::Interleaved { buffer, .. } => buffer,
            _ => panic!("{name} hands out one interleaved buffer"),
        }
    }

    /// Every fixture, and the layout its header asks for. A four channel file
    /// reports four channels and a three channel frame, which is the rule.
    #[test]
    fn the_fixtures_state_the_layouts_the_baseline_recorded() {
        for (name, width, height, color_type, source) in [
            (
                "tiff-gray8.tiff",
                37,
                23,
                ColorType::L8,
                SourceColorType::L8,
            ),
            (
                "tiff-gray16.tiff",
                37,
                23,
                ColorType::L16,
                SourceColorType::L16,
            ),
            (
                "tiff-rgb8.tiff",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            (
                "tiff-rgb16.tiff",
                37,
                23,
                ColorType::Rgb16,
                SourceColorType::Rgb16,
            ),
            // Four channels in the buffer, and the writer splits off the alpha.
            (
                "tiff-rgba8.tiff",
                37,
                23,
                ColorType::Rgba8,
                SourceColorType::Rgba8,
            ),
            (
                "alpha-rgba32f.tiff",
                3,
                2,
                ColorType::Rgba32F,
                SourceColorType::Rgba32F,
            ),
            // Every compression the fixtures carry, which must not change the
            // header's meaning. These are the three that `default-features =
            // false` silently drops.
            (
                "tiff-none.tiff",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            (
                "tiff-lzw.tiff",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            (
                "tiff-deflate.tiff",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            (
                "tiff-packbits.tiff",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            // Tiled and planar storage are the container's business, not the
            // frame's, so they are the same layout as the interleaved one.
            (
                "tiff-tiled.tiff",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            (
                "tiff-planar.tiff",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            // The separated inks, which a frame holds as rgb whatever the
            // file's own sample count is. A five sample file keeps its alpha
            // plane, so its frame layout carries one.
            (
                "tiff-cmyk8.tiff",
                4,
                2,
                ColorType::Rgb8,
                SourceColorType::Cmyk8,
            ),
            (
                "tiff-cmyk16.tiff",
                4,
                2,
                ColorType::Rgb16,
                SourceColorType::Cmyk16,
            ),
            (
                "tiff-cmyka8.tiff",
                4,
                2,
                ColorType::Rgba8,
                SourceColorType::Cmyk8,
            ),
            (
                "tiff-cmyk8-planar.tiff",
                4,
                2,
                ColorType::Rgb8,
                SourceColorType::Cmyk8,
            ),
        ] {
            let info = image_info(&fixture(name), true)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .unwrap_or_else(|| panic!("{name} is taken over"));
            assert_eq!((info.width, info.height), (width, height), "{name}");
            assert_eq!(info.color_type, color_type, "{name}");
            assert_eq!(info.original_color_type, source, "{name}");
            assert_eq!(info.transform, Transform::IDENTITY, "{name}");
            let buffer = read(name);
            // The buffer holds the channels the *source* states at the width its
            // samples are, whatever the frame is built from.
            let expected = match color_type {
                ColorType::L8 => (width * height) as usize,
                ColorType::L16 => (width * height) as usize * 2,
                ColorType::Rgb8 => (width * height) as usize * 3,
                ColorType::Rgb16 => (width * height) as usize * 6,
                ColorType::Rgba8 => (width * height) as usize * 4,
                _ => (width * height) as usize * 16,
            };
            assert_eq!(buffer.len(), expected, "{name}");
        }
    }

    /// A compressed file reads to the same picture as an uncompressed one,
    /// which is what says the decompressor ran rather than that the header was
    /// parsed. Zstd is the one whose feature is opted into by hand.
    #[test]
    fn every_compression_reads_to_the_same_picture() {
        let plain = read("tiff-none.tiff");
        for name in [
            "tiff-lzw.tiff",
            "tiff-deflate.tiff",
            "tiff-packbits.tiff",
            "tiff-tiled.tiff",
            "tiff-zstd.tiff",
        ] {
            assert_eq!(read(name), plain, "{name}");
        }
    }

    /// The planar file reads to the same picture as the interleaved one. This is
    /// the assertion that fails when the planes are not reordered: the samples
    /// are all there, so only the *order* distinguishes the two, and a picture
    /// of the right size with the wrong colours passes every other check here.
    #[test]
    fn a_planar_file_is_reordered_into_a_frame() {
        assert_eq!(
            read("tiff-planar.tiff"),
            read("tiff-rgb8.tiff"),
            "the planar and interleaved spellings of one picture are one picture"
        );
    }

    /// The separated inks become the colour the file states, by the arithmetic
    /// the reader this tree replaced used, which is `f32` and truncates.
    ///
    /// The inks are the ones `tests/make-tiff-cmyk-fixtures.py` writes, and the
    /// expected bytes are that arithmetic worked out by hand. One pixel is worth
    /// reading twice: its `k` is 128 and every other ink is zero, so the colour
    /// is `255 * (1 - 128 / 255)`, which is exactly 127, and in `f32` the factor
    /// lands a hair below it and truncates to 126. An implementation using exact
    /// rational arithmetic would answer 127 there and pass everything else here.
    #[test]
    fn the_separated_inks_become_the_colour_they_state() {
        assert_eq!(
            read("tiff-cmyk8.tiff"),
            [
                255, 255, 255, // no ink at all is paper
                0, 0, 0, // all of it, and black alone, are both black
                0, 0, 0, //
                127, 255, 255, // cyan alone
                126, 126, 126, // black alone, and the truncation above
                119, 179, 209, // four different inks
                0, 255, 255, // cyan and magenta
                206, 198, 189, // and a little of everything
            ]
        );
    }

    /// The same inks at sixteen bits, so the conversion scales with the sample
    /// rather than reusing the byte one.
    #[test]
    fn the_sixteen_bit_inks_scale_with_the_sample() {
        let bytes = read("tiff-cmyk16.tiff");
        let (samples, rest) = bytes.as_chunks::<2>();
        assert!(rest.is_empty(), "a sixteen bit sample is two bytes");
        let words: Vec<u16> = samples
            .iter()
            .map(|word| u16::from_ne_bytes(*word))
            .collect();
        assert_eq!(
            words,
            [
                65535, 65535, 65535, //
                0, 0, 0, //
                0, 0, 0, //
                32639, 65535, 65535, //
                32638, 32638, 32638, // the same truncation, one word up
                30591, 46007, 53715, //
                0, 65535, 65535, //
                53088, 50921, 48754,
            ]
        );
    }

    /// A planar separated file is the same picture as the chunky one. This is
    /// the assertion that fails when four links are not reordered: every sample
    /// is there either way, so only the order tells the two apart, and a picture
    /// of the right size with the wrong colours passes every other check.
    #[test]
    fn a_planar_separated_file_reorders_to_the_same_picture() {
        assert_eq!(read("tiff-cmyk8-planar.tiff"), read("tiff-cmyk8.tiff"));
    }

    /// A fifth sample is alpha rather than a fifth ink: the colour of every
    /// pixel is the four ink file's, so the sample never reached the conversion.
    #[test]
    fn a_fifth_sample_is_alpha_and_not_an_ink() {
        let four = read("tiff-cmyk8.tiff");
        let five = read("tiff-cmyka8.tiff");
        let (rgb, half) = four.as_chunks::<3>();
        let (rgba, quarter) = five.as_chunks::<4>();
        assert!(
            half.is_empty() && quarter.is_empty(),
            "both files hold whole pixels"
        );
        assert_eq!(
            rgb.len(),
            rgba.len(),
            "one channel more, not one pixel more"
        );
        for (pixel, (three, four)) in rgb.iter().zip(rgba).enumerate() {
            assert_eq!(&four[..3], three, "pixel {pixel}");
        }
    }

    /// A buffer that is not whole pixels is refused rather than read past its
    /// end, which is what a header disagreeing with its strips would ask for.
    #[test]
    fn separated_samples_that_are_not_whole_pixels_are_refused() {
        let layout = Layout {
            channels: 4,
            sample_bytes: 1,
            color_type: ColorType::Rgb8,
            source: SourceColorType::Cmyk8,
            float: false,
            separated: true,
            palette: None,
        };
        assert!(inks_to_channels(&[0; 7], &layout).is_err());
        assert!(
            inks_to_channels(&[], &layout).is_ok(),
            "no pixels is not half a pixel"
        );
    }

    /// The reorder is a transpose of the sample grid, checked on a buffer whose
    /// answer can be written out by hand.
    #[test]
    fn the_reorder_is_a_transpose_and_not_a_copy() {
        let layout = Layout {
            channels: 3,
            sample_bytes: 1,
            color_type: ColorType::Rgb8,
            source: SourceColorType::Rgb8,
            float: false,
            separated: false,
            palette: None,
        };
        // Two pixels, three channels, held as three planes of two samples.
        let planes = [10u8, 11, 20, 21, 30, 31];
        let interleaved = interleave(&planes, &layout, 2).expect("a whole picture");
        assert_eq!(interleaved, [10, 20, 30, 11, 21, 31]);
        // A stride that cannot hold a picture is refused rather than read.
        assert!(interleave(&planes, &layout, 1).is_err());
        assert!(interleave(&planes, &layout, 4).is_err());
    }

    /// A palette page is expanded through its colour table, and the four widths a
    /// byte divides into read the same way.
    ///
    /// The indices come from the strips here because the crate refuses this
    /// photometric before it can describe a chunk, so the expansion is this
    /// reader's own: a row of indices is padded to a byte boundary and an index
    /// narrower than a byte is packed into the bits above the one before it. The
    /// expected colours are what libtiff hands Pillow for the same files at the
    /// same positions, which is what says the bit order and the padding are the
    /// ones the format means rather than ones that happen to look plausible.
    #[test]
    fn a_palette_page_is_expanded_through_its_colour_table() {
        // (x, y, r, g, b), for each width, read out of the same file by Pillow.
        for (name, samples) in [
            (
                "tiff-palette-1bit.tiff",
                [(0, 0, 197, 0, 58), (1, 5, 197, 0, 58), (18, 11, 64, 0, 191)],
            ),
            (
                "tiff-palette-2bit.tiff",
                [
                    (0, 0, 197, 0, 58),
                    (1, 5, 197, 0, 58),
                    (18, 11, 128, 0, 128),
                ],
            ),
            (
                "tiff-palette.tiff",
                [
                    (0, 0, 244, 0, 11),
                    (1, 5, 209, 0, 46),
                    (18, 11, 110, 0, 145),
                ],
            ),
            (
                "tiff-palette-8bit.tiff",
                [(0, 0, 255, 0, 0), (1, 5, 197, 0, 58), (18, 11, 128, 0, 128)],
            ),
        ] {
            let info = image_info(&fixture(name), true)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .unwrap_or_else(|| panic!("{name} is taken over"));
            assert_eq!((info.width, info.height), (37, 23), "{name}");
            assert_eq!(info.color_type, ColorType::Rgb8, "{name}");
            // The label is the layout the samples expand to rather than the
            // indices the file holds, which is what a palette png reports.
            assert_eq!(info.original_color_type, SourceColorType::Rgb8, "{name}");
            assert_eq!(info.format, PixelFormat::Rgb8, "{name}");
            // Every one of these is 37 samples wide and 23 rows tall.
            let width = 37usize;
            let buffer = read(name);
            for (x, y, r, g, b) in samples {
                let at = (y * width + x) * 3;
                assert_eq!(&buffer[at..at + 3], [r, g, b], "{name} at ({x}, {y})");
            }
        }
    }

    /// A page whose indices are not a width this reader takes is refused by the
    /// *probe*, so the probe cannot promise a frame the decode would refuse.
    ///
    /// Three bits an index is a legal width and one a byte does not divide
    /// into. Nothing here writes one and libtiff will not read the sixteen bit
    /// page that was written by hand to find out, so the widths that are read
    /// are the ones a reference implementation reads too.
    #[test]
    fn a_palette_width_that_is_not_read_is_refused_at_identify() {
        let error = image_info(&fixture("tiff-palette-3bit.tiff"), true)
            .expect_err("a three bit palette is refused");
        assert!(error.to_string().contains("3 bit"), "{error}");
    }

    /// A file that is not a tiff is declined rather than refused, and a path with
    /// no file behind it falls back on the extension, which is the hint a format
    /// with a signature can do without.
    #[test]
    fn a_file_that_is_not_a_tiff_is_declined() {
        assert!(!owns(Path::new("a.png")));
        assert!(owns(Path::new("a.tif")));
        assert!(owns(Path::new("a.TIFF")));
        assert!(
            image_info(&fixture("cicp-rgb8.png"), true)
                .expect("a png is not ours")
                .is_none()
        );
    }

    /// A file that states orientation 6 and carries a profile keeps both.
    ///
    /// Plan 34 records the probe reporting "orientation 1, no ICC" for such a
    /// file: the adapter built its `ImageInfo` with both hardcoded away, so a
    /// picture that should be turned was handed out stored. The directory the
    /// crate reads for the dimensions holds both tags, so neither costs a pass.
    #[test]
    fn the_orientation_and_the_profile_are_read_from_the_directory() {
        let info = image_info(&fixture("tiff-orient6-icc.tiff"), true)
            .expect("a tiff is ours")
            .expect("the fixture is taken over");
        assert_eq!(info.orientation, Orientation::Rotate90, "orientation 6");
        assert!(info.has_icc_profile, "the file carries a profile");
        let profile = info.icc_profile.expect("the bytes are kept");
        assert_eq!(profile.len(), 588, "the same profile the fixture embeds");
        // The probe reports the size the file stores and the transform carries the
        // turn; the writer is what hands the stored 2x3 out as 3x2.
        assert_eq!((info.width, info.height), (2, 3));
        assert_ne!(
            info.transform,
            Transform::IDENTITY,
            "a quarter turn is applied when rotation is asked for"
        );
    }
}
