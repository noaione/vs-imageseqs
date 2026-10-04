//! Farbfeld, read here rather than through a crate.
//!
//! Farbfeld is the smallest image format there is: the eight bytes `farbfeld`,
//! the width and the height as big-endian `u32`, then one big-endian `u16` per
//! channel per pixel, red first and alpha last. There is no compression, no
//! palette and no metadata, which is why the whole reader is [`decode`] and a
//! header reader, and why a crate for it was rejected: the one candidate's
//! `decode_into` is not usable, and the format is less code than the wrapper
//! around it would be.
//!
//! Two things are worth naming, because they are the only places this can go
//! wrong:
//!
//! - **The samples are big-endian on disk and native in memory.** A frame is
//!   written from a `u16` cast, so the bytes have to be swapped as they are
//!   read. `tests/fixtures/alpha-rgba16.ff` holds `1000, 2000, 3000, 100`
//!   followed by `4000, 5000, 6000, 200`, which no byte order could read back
//!   as anything else by accident.
//! - **Every farbfeld has four channels.** The format has no three-channel
//!   spelling and no way to say "no alpha", so every file is `Rgba16` and every
//!   file has an alpha plane, whatever its samples contain.
//!
//! The provenance is [27](../../../docs/improvements/27-direct-still-decoders.md)'s
//! row, which decided this one would be written here.

use std::path::Path;

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error, image_head},
    error::{ImgSeqError, Result},
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// The eight bytes every farbfeld starts with.
const MAGIC: &[u8; 8] = b"farbfeld";

/// Bytes of the magic and the two dimensions.
const HEADER: usize = 16;

/// Channels per pixel, which the format fixes at four.
const CHANNELS: usize = 4;

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    // Content first: a file whose bytes say it is something else is that
    // something else however it is named, and the extension is the hint a
    // format with no signature of its own has to fall back on. See
    // [`identify::owns`](crate::formats::identify::owns).
    crate::formats::identify::owns(crate::formats::identify::Format::Farbfeld, path)
}

/// The size a farbfeld states, when it states one.
///
/// `None` means "this is not a farbfeld", which is a different answer from
/// "this is a broken one": a file that does not start with the magic is left to
/// whatever else can read it, and one that does is this module's to refuse.
fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < HEADER || &data[..8] != MAGIC {
        return None;
    }
    let width = u32::from_be_bytes(data[8..12].try_into().ok()?);
    let height = u32::from_be_bytes(data[12..16].try_into().ok()?);
    Some((width, height))
}

/// How many bytes the pixels of a `width` by `height` farbfeld occupy.
fn pixel_bytes(width: u32, height: u32) -> Result<usize> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(CHANNELS * 2))
        .ok_or_else(|| ImgSeqError::new("the farbfeld is too large to read"))
}

/// The pixel bytes of a farbfeld, checked against the size its header states.
///
/// Kept apart from [`decode`] so the bounds can be checked without a file on
/// disk: a header is a claim about a length, and the claim is what has to be
/// verified before anything is allocated from it.
fn pixel_data<'a>(data: &'a [u8], width: u32, height: u32, path: &Path) -> Result<&'a [u8]> {
    let expected = pixel_bytes(width, height)?;
    let end = HEADER
        .checked_add(expected)
        .ok_or_else(|| ImgSeqError::new("the farbfeld is too large to read"))?;
    data.get(HEADER..end).ok_or_else(|| {
        image_error(
            "decode",
            path,
            format!(
                "the file holds {} bytes of pixels where the header states {expected}",
                data.len().saturating_sub(HEADER)
            ),
        )
    })
}

/// What a farbfeld states, when this module reads the file.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info(
    path: &Path,
    _apply_rotation: bool,
    route: Option<crate::formats::identify::Format>,
) -> Result<Option<ImageInfo>> {
    if !route.map_or_else(
        || owns(path),
        |saved| saved == crate::formats::identify::Format::Farbfeld,
    ) {
        return Ok(None);
    }
    let data = image_head(path).map_err(|error| image_error("open", path, error))?;
    let Some((width, height)) = dimensions(&data) else {
        return Ok(None);
    };
    if width == 0 || height == 0 {
        return Err(image_error("identify", path, "the header states no pixels"));
    }
    Ok(Some(ImageInfo {
        route: None,
        path: path.to_path_buf(),
        width,
        height,
        // Every farbfeld is four channels of sixteen bits. The format has no
        // other spelling, so the decoded layout and the label are the same.
        color_type: ColorType::Rgba16,
        original_color_type: SourceColorType::Rgba16,
        // A farbfeld states no profile, no colour description and no
        // orientation: its header is a magic and a size and nothing else.
        has_icc_profile: false,
        icc_profile: None,
        cicp: None,
        chroma_location: None,
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: PixelFormat::Rgb16,
    }))
}

/// Decodes a farbfeld into one interleaved buffer.
///
/// The alpha plane is the fourth channel of every pixel, so a call that hands
/// out no alpha clip needs no second pass.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read, when it is not a
/// farbfeld, or when it is too short to hold the pixels it states.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = std::time::Instant::now();
    let data = std::fs::read(&info.path).map_err(|error| image_error("open", &info.path, error))?;
    let open = open_started.elapsed();

    let (width, height) =
        dimensions(&data).ok_or_else(|| image_error("decode", &info.path, "not a farbfeld"))?;
    if (width, height) != (info.width, info.height) {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {}x{}, now {width}x{height})",
            info.path.display(),
            info.width,
            info.height,
        )));
    }
    let pixels_in = pixel_data(&data, width, height, &info.path)?;

    let buffer_started = std::time::Instant::now();
    let mut pixels = vec![0u8; pixels_in.len()];
    // Big-endian on disk, native in memory: a frame is written from a `u16`
    // cast, so the swap happens here rather than in the writer.
    for (sample, target) in pixels_in
        .as_chunks::<2>()
        .0
        .iter()
        .zip(pixels.as_chunks_mut::<2>().0.iter_mut())
    {
        *target = u16::from_be_bytes(*sample).to_ne_bytes();
    }
    let buffer = buffer_started.elapsed();

    Ok(DecodedImage {
        width,
        height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Interleaved {
            color_type: ColorType::Rgba16,
            buffer: pixels,
        },
        timings: DecodeTimings {
            open,
            metadata: std::time::Duration::ZERO,
            buffer,
            read: std::time::Duration::ZERO,
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

    /// The committed fixture: two by two, four channels of sixteen bits.
    #[test]
    fn the_header_is_read_as_the_file_states_it() {
        let info = image_info(&fixture("alpha-rgba16.ff"), true, None)
            .expect("the header is read")
            .expect("a farbfeld is taken over");
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(info.format, PixelFormat::Rgb16);
        assert_eq!(info.color_type, ColorType::Rgba16);
        assert_eq!(info.original_color_type, SourceColorType::Rgba16);
        assert_eq!(info.transform, Transform::IDENTITY);
        assert!(!info.has_icc_profile);
    }

    /// Every channel of every pixel, in the order the format stores them and in
    /// the byte order a frame needs: `R, G, B, A` per pixel, native-endian
    /// `u16`. Wrong byte order would read these back as 6144 or 13312 rather
    /// than 1000.
    #[test]
    fn the_samples_are_big_endian_on_disk_and_native_in_memory() {
        let info = image_info(&fixture("alpha-rgba16.ff"), true, None)
            .expect("the header is read")
            .expect("taken over");
        let decoded = decode(&info).expect("the file decodes");
        let Pixels::Interleaved {
            color_type, buffer, ..
        } = decoded.pixels
        else {
            panic!("a farbfeld hands out one interleaved buffer");
        };
        assert_eq!(color_type, ColorType::Rgba16);
        // Two by two pixels, four channels, two bytes a sample.
        assert_eq!(buffer.len(), 2 * 2 * 4 * 2);
        let samples: Vec<u16> = buffer
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_ne_bytes(*pair))
            .collect();
        assert_eq!(
            samples,
            vec![
                1000, 2000, 3000, 100, //
                4000, 5000, 6000, 200, //
                7000, 8000, 9000, 300, //
                10000, 11000, 12000, 400,
            ]
        );
    }

    /// A file that stops before the pixels it states is refused rather than
    /// handed out short.
    #[test]
    fn a_truncated_file_is_refused() {
        let path = fixture("alpha-rgba16.ff");
        let data = std::fs::read(&path).expect("the fixture is read");
        // The whole file reads, and every shorter prefix is refused: a header
        // that states four pixels is a claim about forty eight bytes.
        assert_eq!(data.len(), 48);
        assert_eq!(
            pixel_data(&data, 2, 2, &path)
                .expect("the whole file")
                .len(),
            32
        );
        for missing in 1..=32 {
            let cut = &data[..data.len() - missing];
            let error = pixel_data(cut, 2, 2, &path)
                .expect_err("a short file is refused, not handed out short");
            assert!(
                error.to_string().contains("where the header states"),
                "{error}"
            );
        }
        // And a header that states more than any file holds is refused before
        // anything is allocated from it.
        assert!(pixel_data(&data, u32::MAX, u32::MAX, &path).is_err());
    }

    /// A header that states no pixels, and one that is not a farbfeld at all.
    #[test]
    fn a_broken_header_is_an_error_and_a_foreign_file_is_declined() {
        // The magic, then a zero height.
        let mut data = MAGIC.to_vec();
        data.extend_from_slice(&2u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        assert_eq!(dimensions(&data), Some((2, 0)));

        // Not the magic: declined rather than refused, so the probe can describe
        // it as whatever it really is.
        let mut foreign = b"notfarbf".to_vec();
        foreign.extend_from_slice(&2u32.to_be_bytes());
        foreign.extend_from_slice(&2u32.to_be_bytes());
        assert_eq!(dimensions(&foreign), None);

        // Too short even for a header.
        assert_eq!(dimensions(b"farbfeld"), None);
    }

    /// Only a `.ff` is taken over.
    #[test]
    fn only_a_farbfeld_is_taken_over() {
        assert!(owns(Path::new("a.ff")));
        assert!(owns(Path::new("a.FF")));
        assert!(!owns(Path::new("a.png")));
        assert!(!owns(Path::new("a.ffv")));
    }

    /// A file whose size changed after probing is refused.
    #[test]
    fn a_file_that_changed_after_probing_is_refused() {
        let mut info = image_info(&fixture("alpha-rgba16.ff"), true, None)
            .expect("the header is read")
            .expect("taken over");
        info.height += 1;
        let error = decode(&info).expect_err("the sizes disagree");
        assert!(
            error.to_string().contains("changed after probing"),
            "{error}"
        );
    }
}
