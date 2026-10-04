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
    /// Channels one pixel holds.
    channels: usize,
    /// Bytes one sample occupies.
    sample_bytes: usize,
    /// The layout a frame is written from.
    color_type: ColorType,
    /// The label property's value.
    source: SourceColorType,
    /// Whether the samples are floats rather than integers.
    float: bool,
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
    let (bits, channels, color_type, source) = match kind {
        Tiff::Gray(bits @ (8 | 16 | 32)) => {
            let (color, source) = match bits {
                8 => (ColorType::L8, SourceColorType::L8),
                _ => (ColorType::L16, SourceColorType::L16),
            };
            (bits, 1, color, source)
        }
        Tiff::GrayA(bits @ (8 | 16)) => {
            let (color, source) = if bits == 8 {
                (ColorType::La8, SourceColorType::La8)
            } else {
                (ColorType::La16, SourceColorType::La16)
            };
            (bits, 2, color, source)
        }
        Tiff::RGB(bits @ (8 | 16 | 32)) => {
            let (color, source) = match bits {
                8 => (ColorType::Rgb8, SourceColorType::Rgb8),
                16 => (ColorType::Rgb16, SourceColorType::Rgb16),
                _ => (ColorType::Rgb32F, SourceColorType::Rgb32F),
            };
            (bits, 3, color, source)
        }
        Tiff::RGBA(bits @ (8 | 16 | 32)) => {
            let (color, source) = match bits {
                8 => (ColorType::Rgba8, SourceColorType::Rgba8),
                16 => (ColorType::Rgba16, SourceColorType::Rgba16),
                _ => (ColorType::Rgba32F, SourceColorType::Rgba32F),
            };
            (bits, 4, color, source)
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
    })
}

/// The format a frame is written from.
///
/// A four channel source is separated into a three channel frame and an alpha
/// clip, so the alpha's existence does not make the colour frame a wider one.
fn format(layout: &Layout) -> PixelFormat {
    match (layout.color_type, layout.sample_bytes) {
        (ColorType::L8 | ColorType::La8, _) => PixelFormat::Gray8,
        (ColorType::L16 | ColorType::La16, 4) => PixelFormat::Gray32F,
        (ColorType::L16 | ColorType::La16, _) => PixelFormat::Gray16,
        (ColorType::Rgb8 | ColorType::Rgba8, _) => PixelFormat::Rgb8,
        (ColorType::Rgb16 | ColorType::Rgba16, _) => PixelFormat::Rgb16,
        _ => PixelFormat::Rgb32F,
    }
}

/// Opens a tiff and reports what it states.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info(path: &Path, _apply_rotation: bool) -> Result<Option<ImageInfo>> {
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
    let little = signature == [0x49, 0x49, 0x2a, 0x00];
    let big = signature == [0x4d, 0x4d, 0x00, 0x2a];
    if !little && !big {
        return Ok(None);
    }

    let mut decoder = tiff::decoder::Decoder::new(std::io::BufReader::new(file))
        .map_err(|error| image_error("identify", path, error))?;
    let (width, height) = decoder
        .dimensions()
        .map_err(|error| image_error("identify", path, error))?;
    let layout = layout(
        decoder
            .colortype()
            .map_err(|error| image_error("identify", path, error))?,
    )
    .map_err(|error| image_error("identify", path, error))?;
    if width == 0 || height == 0 {
        return Err(ImgSeqError::new("the header states no pixels"));
    }

    Ok(Some(ImageInfo {
        path: path.to_path_buf(),
        width,
        height,
        color_type: layout.color_type,
        original_color_type: layout.source,
        has_icc_profile: false,
        icc_profile: None,
        cicp: None,
        chroma_location: None,
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
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
    let layout = layout(
        decoder
            .colortype()
            .map_err(|error| image_error("decode", &info.path, error))?,
    )
    .map_err(|error| image_error("decode", &info.path, error))?;
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
    // The planes arrive one after another, and a frame is interleaved. This is
    // the step that separates a correct picture from one of the right size.
    if preference.planes > 1 {
        let stride = preference
            .plane_stride
            .map_or(0, std::num::NonZeroUsize::get);
        buffer = interleave(&buffer, &layout, stride)
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
        };
        // Two pixels, three channels, held as three planes of two samples.
        let planes = [10u8, 11, 20, 21, 30, 31];
        let interleaved = interleave(&planes, &layout, 2).expect("a whole picture");
        assert_eq!(interleaved, [10, 20, 30, 11, 21, 31]);
        // A stride that cannot hold a picture is refused rather than read.
        assert!(interleave(&planes, &layout, 1).is_err());
        assert!(interleave(&planes, &layout, 4).is_err());
    }

    /// A palette page is refused at identify, so the probe cannot promise a
    /// frame the decode would refuse, and a file that is not a tiff is declined.
    #[test]
    fn a_palette_page_is_refused_by_name() {
        let error =
            image_info(&fixture("tiff-palette.tiff"), true).expect_err("a palette tiff is refused");
        assert!(error.to_string().contains("palette"), "{error}");

        assert!(!owns(Path::new("a.png")));
        assert!(owns(Path::new("a.tif")));
        assert!(owns(Path::new("a.TIFF")));
        assert!(
            image_info(&fixture("cicp-rgb8.png"), true)
                .expect("a png is not ours")
                .is_none()
        );
    }
}
