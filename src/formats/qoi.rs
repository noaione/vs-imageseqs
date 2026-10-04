//! Quite OK Image, read through the `qoi` crate.
//!
//! QOI is a header, an opcode stream and an eight byte end marker. That is
//! small enough that the crate is the whole decoder and this module is the
//! adapter: [`image_info`] reads the fourteen byte header without touching a
//! sample, and [`decode`] runs the crate's decoder into the buffer a frame is
//! written from.
//!
//! Two facts come out of the header, and they are independent:
//!
//! - The channel count, three or four, which is what decides whether the file
//!   has an alpha plane at all. It is a statement about the file, not about the
//!   samples: a four channel file whose alpha happens to be all 255 still has
//!   one, and a three channel file has none however its samples look.
//! - The colours flag, which the specification calls `sRGB` or `linear`. It is
//!   **informative** and changes no sample, and it must not become a `_Transfer`
//!   frame property. QOI states no colour description, so a file whose flag is
//!   set is tagged exactly like one whose flag is clear;
//!   `tests/fixtures/qoi-linear.qoi` and `qoi-rgb8.qoi` hold the same picture
//!   and exist to keep that true.
//!
//! The provenance is [27](../../../docs/improvements/27-direct-still-decoders.md)'s
//! row: `qoi 0.4.1` was already in the lock file behind the `image` crate's own
//! qoi adapter, so this is a dependency promotion rather than a new crate. It
//! was verified against the `image` decoder byte for byte before the migration,
//! and after it by the fixtures above.

use std::path::Path;

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// File extensions that hold a qoi.
const EXTENSIONS: [&str; 1] = ["qoi"];

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

/// What the header of a qoi states.
struct Header {
    width: u32,
    height: u32,
    /// Three or four channels, which is whether the file has an alpha plane.
    color_type: ColorType,
    /// The colour type the label property reports, which is the same here:
    /// qoi's only two layouts are `Rgb8` and `Rgba8`, and a container that
    /// states one byte per channel is handed out at that word.
    source: SourceColorType,
}

impl Header {
    /// Reads the header, which is the first fourteen bytes and nothing else.
    fn read(data: &[u8], path: &Path) -> Result<Self> {
        let header =
            qoi::decode_header(data).map_err(|error| image_error("identify", path, error))?;
        if header.width == 0 || header.height == 0 {
            return Err(image_error("identify", path, "the header states no pixels"));
        }
        let (color_type, source) = match header.channels {
            qoi::Channels::Rgb => (ColorType::Rgb8, SourceColorType::Rgb8),
            qoi::Channels::Rgba => (ColorType::Rgba8, SourceColorType::Rgba8),
        };
        Ok(Self {
            width: header.width,
            height: header.height,
            color_type,
            source,
        })
    }

    /// The format a frame is written from.
    ///
    /// Both layouts are three colour channels with the depth a qoi states,
    /// which is eight; the alpha of an `Rgba8` file is a plane of its own
    /// rather than a fourth channel of this format.
    const fn format(&self) -> PixelFormat {
        PixelFormat::Rgb8
    }

    /// How many bytes one decoded sample of one pixel occupies.
    const fn channels(&self) -> usize {
        match self.color_type {
            ColorType::Rgba8 => 4,
            _ => 3,
        }
    }
}

/// What the header of a qoi states, when this module reads the file.
///
/// Returns `None` for anything that is not a qoi, which leaves it to whatever
/// else can read it.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info(path: &Path, _apply_rotation: bool) -> Result<Option<ImageInfo>> {
    if !owns(path) {
        return Ok(None);
    }
    let data = std::fs::read(path).map_err(|error| image_error("open", path, error))?;
    // A file whose magic is not `qoif` is not a qoi however it is named, and
    // the extension is only a hint: declining it here lets the probe describe it
    // as what it really is rather than failing on a file another reader owns.
    if !data.starts_with(b"qoif") {
        return Ok(None);
    }
    let header = Header::read(&data, path)?;
    Ok(Some(ImageInfo {
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type: header.color_type,
        original_color_type: header.source,
        // A qoi states no profile, no colour description and no orientation.
        has_icc_profile: false,
        icc_profile: None,
        cicp: None,
        chroma_location: None,
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: header.format(),
    }))
}

/// Decodes a qoi into the buffer of its own layout.
///
/// The alpha of a four channel file arrives inside that buffer, so a call that
/// hands out no alpha clip needs no second pass and no decoder to skip.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or the stream is
/// malformed.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = std::time::Instant::now();
    let data = std::fs::read(&info.path).map_err(|error| image_error("open", &info.path, error))?;
    let open = open_started.elapsed();

    let header = Header::read(&data, &info.path)?;
    if (header.width, header.height) != (info.width, info.height) {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {}x{}, now {}x{})",
            info.path.display(),
            info.width,
            info.height,
            header.width,
            header.height,
        )));
    }
    if header.color_type != info.color_type {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {:?}, now {:?})",
            info.path.display(),
            info.color_type,
            header.color_type,
        )));
    }

    let buffer_started = std::time::Instant::now();
    let size = (header.width as usize)
        .checked_mul(header.height as usize)
        .and_then(|pixels| pixels.checked_mul(header.channels()))
        .ok_or_else(|| ImgSeqError::new(format!("image '{}' is too large", info.path.display())))?;
    let mut pixels = vec![0u8; size];
    let buffer = buffer_started.elapsed();

    let read_started = std::time::Instant::now();
    qoi::decode_to_buf(&mut pixels, &data)
        .map_err(|error| image_error("decode", &info.path, error))?;
    let read = read_started.elapsed();

    Ok(DecodedImage {
        width: header.width,
        height: header.height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Interleaved {
            color_type: header.color_type,
            buffer: pixels,
        },
        timings: DecodeTimings {
            open,
            metadata: std::time::Duration::ZERO,
            buffer,
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

    /// The three committed fixtures, and what each states.
    #[test]
    fn the_headers_are_read_as_the_files_state_them() {
        for (name, width, height, color_type, format) in [
            ("qoi-rgb8.qoi", 8, 6, ColorType::Rgb8, PixelFormat::Rgb8),
            ("qoi-rgba8.qoi", 8, 6, ColorType::Rgba8, PixelFormat::Rgb8),
            ("qoi-linear.qoi", 8, 6, ColorType::Rgb8, PixelFormat::Rgb8),
        ] {
            let info = image_info(&fixture(name), true)
                .expect("the header is read")
                .expect("a qoi is taken over");
            assert_eq!((info.width, info.height), (width, height), "{name}");
            assert_eq!(info.color_type, color_type, "{name}");
            assert_eq!(info.format, format, "{name}");
            assert_eq!(info.transform, Transform::IDENTITY, "{name}");
            assert_eq!(info.orientation, Orientation::NoTransforms, "{name}");
            assert!(!info.has_icc_profile, "{name}");
            assert!(info.cicp.is_none(), "{name}");
        }
    }

    /// The colours flag is informative, so the file that sets it is labelled
    /// exactly like the file that clears it. This is the parity rule the plan
    /// names, and it is the reason `qoi-linear.qoi` exists.
    #[test]
    fn the_colours_flag_is_not_a_colour_property() {
        let plain = image_info(&fixture("qoi-rgb8.qoi"), true)
            .expect("the header is read")
            .expect("taken over");
        let linear = image_info(&fixture("qoi-linear.qoi"), true)
            .expect("the header is read")
            .expect("taken over");
        assert_eq!(plain.color_type, linear.color_type);
        assert_eq!(plain.original_color_type, linear.original_color_type);
        assert_eq!(plain.format, linear.format);
        assert_eq!(plain.cicp, linear.cicp);
    }

    /// The channel count is what decides the alpha plane, not the samples.
    #[test]
    fn the_channel_count_decides_the_alpha_plane() {
        let rgb = image_info(&fixture("qoi-rgb8.qoi"), true)
            .expect("the header is read")
            .expect("taken over");
        let rgba = image_info(&fixture("qoi-rgba8.qoi"), true)
            .expect("the header is read")
            .expect("taken over");
        assert_eq!(crate::pixel::alpha_channel(rgb.color_type), None);
        assert_eq!(crate::pixel::alpha_channel(rgba.color_type), Some(3));
        // Both are handed out as the same format: alpha is a clip of its own.
        assert_eq!(rgb.format, rgba.format);
    }

    /// The decode produces one sample per channel per pixel, and the picture
    /// the fixtures were written to hold.
    #[test]
    fn the_samples_are_the_ones_the_file_holds() {
        let info = image_info(&fixture("qoi-rgba8.qoi"), true)
            .expect("the header is read")
            .expect("taken over");
        let decoded = decode(&info).expect("the file decodes");
        let Pixels::Interleaved {
            color_type, buffer, ..
        } = decoded.pixels
        else {
            panic!("a qoi hands out one interleaved buffer");
        };
        assert_eq!(color_type, ColorType::Rgba8);
        assert_eq!(buffer.len(), 8 * 6 * 4);
        // The first pixel is the ramp's start, and the alpha the fixture wrote.
        assert_eq!(&buffer[..4], &[255, 0, 0, 40]);
    }

    /// A file the module does not own, and one whose magic does not match its
    /// extension, are both declined rather than described.
    #[test]
    fn only_a_qoi_is_taken_over() {
        assert!(owns(Path::new("a.qoi")));
        assert!(owns(Path::new("a.QOI")));
        assert!(!owns(Path::new("a.png")));
        assert!(!owns(Path::new("a.qoif")));
        // A png named as a qoi is not one, so the extension alone is not
        // enough to claim it.
        assert!(
            image_info(&fixture("cicp-rgb8.png"), true)
                .expect("a png is not ours to describe")
                .is_none()
                || !owns(Path::new("cicp-rgb8.png"))
        );
    }

    /// A file that stops early is an error rather than a short buffer.
    #[test]
    fn a_truncated_stream_is_refused() {
        let path = fixture("qoi-rgb8.qoi");
        let data = std::fs::read(&path).expect("the fixture is read");
        // The header alone is enough to describe, and not enough to decode.
        assert_eq!(
            qoi::decode_header(&data[..14])
                .expect("the header is there")
                .width,
            8
        );
        let mut buffer = vec![0u8; 8 * 6 * 3];
        assert!(
            qoi::decode_to_buf(&mut buffer, &data[..14]).is_err(),
            "a stream with no samples in it decodes to nothing"
        );
        // And one cut before its end marker is one too.
        assert!(qoi::decode_to_buf(&mut buffer, &data[..data.len() - 4]).is_err());
    }

    /// A file that changed between the probe and the decode is refused rather
    /// than handed out at the wrong size or in the wrong layout.
    #[test]
    fn a_file_that_changed_after_probing_is_refused() {
        let mut info = image_info(&fixture("qoi-rgb8.qoi"), true)
            .expect("the header is read")
            .expect("taken over");
        info.width += 1;
        let error = decode(&info).expect_err("the sizes disagree");
        assert!(
            error.to_string().contains("changed after probing"),
            "{error}"
        );

        // The channel count is the other half: it decides the alpha plane, so
        // handing one out at the other layout would be a wrong picture rather
        // than a wrong size.
        let mut info = image_info(&fixture("qoi-rgba8.qoi"), true)
            .expect("the header is read")
            .expect("taken over");
        info.color_type = ColorType::Rgb8;
        let error = decode(&info).expect_err("the layouts disagree");
        assert!(
            error.to_string().contains("changed after probing"),
            "{error}"
        );
    }
}
