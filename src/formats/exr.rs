//! OpenEXR stills, read straight from the format's own crate.
//!
//! The plan's row for this format is the one that says the candidate is already
//! in the lock file behind `image`, and that this crate's channel selection is
//! what the adapter was reaching for; see
//! `docs/improvements/27-direct-still-decoders.md`. What replaces the adapter is
//! this file: the `exr` crate parses the header and decompresses the blocks, and
//! the picture is laid out here.
//!
//! Three behaviours are the rule rather than an incidental detail:
//!
//! - **A frame is always float and native-endian.** Every sample type the format
//!   defines -- half, float and unsigned integer -- is handed over as `f32`, so
//!   the same picture stored as half and as float holds the same numbers.
//! - **The channels are chosen by name, not by position.** `R`, `G` and `B` are
//!   read wherever the file keeps them, so a file that stores them in another
//!   order still decodes to the picture it holds, and `A` becomes the alpha clip
//!   only when the file states one.
//! - **The probe reads the header alone.** `MetaData::read_from_file` parses the
//!   attributes and the block table and stops there, so describing a file never
//!   decompresses a block.
//!
//! A layer is handed out at its own data window, which is the pixels the file
//! stores. A file may place that layer inside a larger display window; the
//! canvas around it, which the reader this replaces filled with transparent
//! black, is not reconstructed here.

use std::{io::Read, path::Path};

use exr::{
    meta::MetaData,
    prelude::{FlatSamples, read_first_flat_layer_from_file},
};

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// File extensions that hold an openexr picture.
const EXTENSIONS: [&str; 1] = ["exr"];

/// The four bytes every openexr picture starts with, which is the magic number
/// 20000630 written little-endian.
const MAGIC: [u8; 4] = [0x76, 0x2f, 0x31, 0x01];

/// The colour channels a frame is built from, in the order a frame holds them.
const COLOR_CHANNELS: [&[u8]; 3] = [b"R", b"G", b"B"];

/// The channel the alpha clip is built from, when the file states one.
const ALPHA_CHANNEL: &[u8] = b"A";

/// What one layer's header states, and nothing else.
///
/// This is the whole of what a probe needs: the size of the layer, and whether
/// it carries an alpha channel. Every other attribute the format defines is the
/// file owner's and is not reported.
#[derive(Clone, Copy, Debug)]
struct Header {
    width: u32,
    height: u32,
    /// Whether the layer states an alpha channel.
    alpha: bool,
}

impl Header {
    /// The layout a frame is written from.
    #[must_use]
    const fn color_type(&self) -> ColorType {
        if self.alpha {
            ColorType::Rgba32F
        } else {
            ColorType::Rgb32F
        }
    }

    /// The label property's value.
    #[must_use]
    const fn source(&self) -> SourceColorType {
        if self.alpha {
            SourceColorType::Rgba32F
        } else {
            SourceColorType::Rgb32F
        }
    }

    /// The format a frame is written from.
    ///
    /// An alpha channel is not part of the colour frame even when the file
    /// holds one: it is the gray clip `ReadAlpha` hands out, so the colour
    /// frame is three floats wide either way.
    #[must_use]
    const fn format(&self) -> PixelFormat {
        PixelFormat::Rgb32F
    }
}

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// Whether the file starts with the format's magic number.
///
/// A file too short to hold the four bytes is not one either, so it is declined
/// rather than refused at this step: the magic is what decides whether this
/// module owns a `.exr`, and the extension only decides whether it is asked.
fn is_exr(path: &Path) -> Result<bool> {
    let mut file = std::fs::File::open(path).map_err(|error| image_error("open", path, error))?;
    let mut magic = [0u8; MAGIC.len()];
    match file.read_exact(&mut magic) {
        Ok(()) => Ok(magic == MAGIC),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(image_error("open", path, error)),
    }
}

/// Whether a layer is one this reader can hand out: a flat r,g,b one.
///
/// A deep layer has no flat samples at all, and a layer without all three of
/// `R`, `G` and `B` is a picture in a colour space this reader does not convert.
/// Both are declined rather than described wrongly.
fn flat_rgb(header: &exr::meta::header::Header) -> bool {
    !header.deep
        && COLOR_CHANNELS.iter().all(|name| {
            header
                .channels
                .list
                .iter()
                .any(|channel| channel.name.bytes() == *name)
        })
}

/// Whether the layer's channel list holds the alpha channel.
fn states_alpha(header: &exr::meta::header::Header) -> bool {
    header
        .channels
        .list
        .iter()
        .any(|channel| channel.name.bytes() == ALPHA_CHANNEL)
}

/// A size, checked against the type a frame's dimensions are held in.
///
/// The format states its windows in pointer-width numbers and a frame states
/// its dimensions in thirty-two bit ones, so this is where the two meet.
fn frame_size(width: usize, height: usize) -> Result<(u32, u32)> {
    if width == 0 || height == 0 {
        return Err(ImgSeqError::new("the header states no pixels"));
    }
    let width = u32::try_from(width)
        .map_err(|_| ImgSeqError::new("the header states a width a frame cannot hold"))?;
    let height = u32::try_from(height)
        .map_err(|_| ImgSeqError::new("the header states a height a frame cannot hold"))?;
    Ok((width, height))
}

/// Reads the header of a file this module owns.
///
/// Returns `Ok(None)` for a file that is not an openexr picture, and for one
/// whose layers are every one of them either deep or not r,g,b, so that another
/// reader may take it.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or its header cannot be
/// parsed.
fn header(path: &Path) -> Result<Option<Header>> {
    if !is_exr(path)? {
        return Ok(None);
    }
    let metadata = MetaData::read_from_file(path, false)
        .map_err(|error| image_error("identify", path, error))?;
    let Some(layer) = metadata.headers.iter().find(|header| flat_rgb(header)) else {
        return Ok(None);
    };
    // The layer's own data window: see the note at the head of this file.
    let (width, height) = frame_size(layer.layer_size.width(), layer.layer_size.height())
        .map_err(|error| image_error("identify", path, error))?;
    Ok(Some(Header {
        width,
        height,
        alpha: states_alpha(layer),
    }))
}

/// Writes one channel into its slot of an interleaved frame buffer.
///
/// A channel's samples are stored plane by plane, so this is the step that
/// separates a correct picture from a transposed or a shuffled one. A channel
/// whose samples do not cover the layer is refused rather than read past its
/// own end: a release build aborts on a panic, and a file that states a
/// sampling other than one sample a pixel is a shape this reader does not take.
fn interleave(
    samples: &FlatSamples,
    slot: usize,
    channels: usize,
    pixels: usize,
    buffer: &mut [u8],
) -> Result<()> {
    if samples.len() != pixels {
        return Err(ImgSeqError::new(format!(
            "a channel holds {} samples where the layer states {pixels} pixels",
            samples.len()
        )));
    }
    for (index, value) in samples.values_as_f32().enumerate() {
        let at = (index * channels + slot) * size_of::<f32>();
        buffer[at..at + size_of::<f32>()].copy_from_slice(&value.to_ne_bytes());
    }
    Ok(())
}

/// What a picture states, when this module reads the file.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info(path: &Path, _apply_rotation: bool) -> Result<Option<ImageInfo>> {
    if !owns(path) {
        return Ok(None);
    }
    let Some(header) = header(path)? else {
        return Ok(None);
    };
    Ok(Some(ImageInfo {
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type: header.color_type(),
        original_color_type: header.source(),
        has_icc_profile: false,
        icc_profile: None,
        cicp: None,
        chroma_location: None,
        // An openexr states no orientation: a layer is placed inside a canvas
        // rather than turned by a tag, and what is handed out is the layer's own
        // pixels.
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: header.format(),
    }))
}

/// Decodes a picture into one interleaved buffer of native-endian floats.
///
/// The two halves of the file's work -- opening it and reading its samples --
/// are one call into the crate, so the open time is where all of it lands and
/// what is left to measure separately is the laying out of the picture.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or is malformed.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = std::time::Instant::now();
    let image = read_first_flat_layer_from_file(&info.path)
        .map_err(|error| image_error("decode", &info.path, error))?;
    let open = open_started.elapsed();

    let layer = &image.layer_data;
    let (width, height) = frame_size(layer.size.width(), layer.size.height())
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

    // The channels are found by name, so a file that stores them in another
    // order reads the same picture as one that does not.
    let find = |name: &[u8]| {
        layer
            .channel_data
            .list
            .iter()
            .find(|channel| channel.name.bytes() == name)
    };
    let alpha = find(ALPHA_CHANNEL).is_some();
    if alpha != info.color_type.has_alpha() {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing: its alpha channel is {}",
            info.path.display(),
            if alpha { "there" } else { "gone" },
        )));
    }

    let pixels = layer
        .size
        .width()
        .checked_mul(layer.size.height())
        .ok_or_else(|| ImgSeqError::new("the layer states more pixels than a frame holds"))?;
    let channels = if alpha { 4 } else { 3 };
    let bytes = pixels
        .checked_mul(channels)
        .and_then(|count| count.checked_mul(size_of::<f32>()))
        .ok_or_else(|| ImgSeqError::new("the layer is too large for a frame"))?;

    let read_started = std::time::Instant::now();
    let mut buffer = vec![0u8; bytes];
    for (slot, name) in COLOR_CHANNELS.iter().enumerate() {
        let channel = find(name).ok_or_else(|| {
            ImgSeqError::new(format!(
                "the file states no {:?} channel",
                String::from_utf8_lossy(name)
            ))
        })?;
        interleave(&channel.sample_data, slot, channels, pixels, &mut buffer)
            .map_err(|error| image_error("decode", &info.path, error))?;
    }
    if alpha {
        let channel =
            find(ALPHA_CHANNEL).ok_or_else(|| ImgSeqError::new("the file states no A channel"))?;
        interleave(&channel.sample_data, 3, channels, pixels, &mut buffer)
            .map_err(|error| image_error("decode", &info.path, error))?;
    }
    let read = read_started.elapsed();

    Ok(DecodedImage {
        width,
        height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Interleaved {
            color_type: if alpha {
                ColorType::Rgba32F
            } else {
                ColorType::Rgb32F
            },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    /// One fixture's colour buffer, from the probe to the decoded pixels.
    fn read(name: &str) -> (ImageInfo, ColorType, Vec<u8>) {
        let info = image_info(&fixture(name), true)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
            .unwrap_or_else(|| panic!("{name} is taken over"));
        let decoded = decode(&info).unwrap_or_else(|error| panic!("{name}: {error}"));
        match decoded.pixels {
            Pixels::Interleaved { color_type, buffer } => (info, color_type, buffer),
            _ => panic!("{name} hands out one interleaved buffer"),
        }
    }

    /// Every committed fixture, and what its header states. A file without an
    /// alpha channel is described as `Rgb32F` however its samples are stored,
    /// and a file with one is not made wider than the frame it is written into.
    #[test]
    fn the_fixtures_state_the_layout_they_hold() {
        for (name, color_type, source) in [
            (
                "exr-half-rgb.exr",
                ColorType::Rgb32F,
                SourceColorType::Rgb32F,
            ),
            (
                "exr-half-rgba.exr",
                ColorType::Rgba32F,
                SourceColorType::Rgba32F,
            ),
            (
                "exr-float-rgba.exr",
                ColorType::Rgba32F,
                SourceColorType::Rgba32F,
            ),
            ("exr-none.exr", ColorType::Rgb32F, SourceColorType::Rgb32F),
            ("exr-rle.exr", ColorType::Rgb32F, SourceColorType::Rgb32F),
            ("exr-zip.exr", ColorType::Rgb32F, SourceColorType::Rgb32F),
            ("exr-zips.exr", ColorType::Rgb32F, SourceColorType::Rgb32F),
            ("exr-piz.exr", ColorType::Rgb32F, SourceColorType::Rgb32F),
        ] {
            let info = image_info(&fixture(name), true)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .unwrap_or_else(|| panic!("{name} is taken over"));
            assert_eq!((info.width, info.height), (37, 23), "{name}");
            assert_eq!(info.color_type, color_type, "{name}");
            assert_eq!(info.original_color_type, source, "{name}");
            assert_eq!(info.format, PixelFormat::Rgb32F, "{name}");
            assert_eq!(info.orientation, Orientation::NoTransforms, "{name}");
            assert_eq!(info.transform, Transform::IDENTITY, "{name}");
            assert!(!info.has_icc_profile, "{name}");
            assert!(info.icc_profile.is_none(), "{name}");
            assert!(info.cicp.is_none(), "{name}");
            assert!(info.chroma_location.is_none(), "{name}");

            let (_, decoded, buffer) = read(name);
            assert_eq!(decoded, color_type, "{name}");
            let channels = info.color_type.channels();
            assert_eq!(
                buffer.len(),
                37 * 23 * channels * size_of::<f32>(),
                "{name}"
            );
        }
    }

    /// The five compressions the fixtures cover, which are the whole of what a
    /// header can ask for short of the lossy ones: a reader that takes one and
    /// not the others reads most real files wrongly. They hold the same picture,
    /// so they decode to the same bytes.
    #[test]
    fn every_compression_decodes_to_the_same_picture() {
        let (_, _, expected) = read("exr-none.exr");
        for name in ["exr-rle.exr", "exr-zip.exr", "exr-zips.exr", "exr-piz.exr"] {
            let (_, _, got) = read(name);
            assert_eq!(got.len(), expected.len(), "{name}");
            assert_eq!(got, expected, "{name} is the same picture as exr-none.exr");
        }
    }

    /// Half is the format's native sample and float is the other, and the same
    /// picture stored either way holds the same numbers. The alpha file's first
    /// three channels are the colour file's, pixel for pixel.
    #[test]
    fn a_half_picture_and_a_float_one_agree_on_colour() {
        let (_, _, rgb) = read("exr-half-rgb.exr");
        for name in ["exr-half-rgba.exr", "exr-float-rgba.exr"] {
            let (_, _, rgba) = read(name);
            assert_eq!(rgba.len(), rgb.len() / 3 * 4, "{name}");
            for (pixel, colour) in rgba.as_chunks::<16>().0.iter().zip(rgb.as_chunks::<12>().0) {
                assert_eq!(&pixel[..12], colour, "{name}");
            }
        }
    }

    /// A file of another format is declined however it is named, and the magic
    /// is what decides: the extension only decides whether this module is asked.
    #[test]
    fn a_file_of_another_format_is_declined() {
        assert!(!owns(Path::new("a.png")));
        assert!(owns(Path::new("a.exr")));
        assert!(owns(Path::new("a.EXR")));
        assert!(
            image_info(&fixture("cicp-rgb8.png"), true)
                .expect("a png is not ours")
                .is_none()
        );
        assert!(
            !is_exr(&fixture("cicp-rgb8.png")).expect("the png's first bytes are read"),
            "a png does not start with the openexr magic"
        );
    }
}
