//! heif and heic decoding through `libheif` itself.
//!
//! `decoder` registers `libheif_rs::integration::image`, which reads the
//! decoded image through `planes.interleaved`. A monochrome image decodes into
//! a single `Y` plane, so the integration rejects it with:
//!
//! ```text
//! Format error decoding `heif`: Image is not interleaved.
//! ```
//!
//! Monochrome pages are common in scanned material, so those files are decoded
//! here instead: `libheif` is asked for a monochrome image, the planes are
//! packed into the interleaved buffer the frame writer expects (`L8`, `La8`,
//! `L16` or `La16`), and the row padding `libheif` adds is dropped. samples are
//! copied as `libheif` returns them, so a 10-bit page keeps its 10-bit values in
//! the low bits of each 16-bit sample instead of being stretched to the range of
//! `Gray16`.

use std::{path::Path, time::Instant};

use image::ColorType;
use libheif_rs::{ColorSpace, HeifContext, LibHeif, Plane};

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
};

/// File extensions that hold a heif container.
const EXTENSIONS: [&str; 4] = ["heic", "heics", "heif", "hif"];

/// Whether this module decodes `info`.
///
/// Only the images the `image` integration cannot represent are taken over, so
/// colour heif files keep going through the hook and the colour conversion it
/// performs.
pub fn handles(info: &ImageInfo) -> bool {
    has_heif_extension(&info.path) && !integration_can_decode(info.color_type)
}

/// Whether the `image` integration can return this color type.
///
/// It only ever reads `planes.interleaved`, which exists for the colour spaces
/// the hook converts to; the four monochrome color types decode into a single
/// luma plane and have no interleaved one.
const fn integration_can_decode(color_type: ColorType) -> bool {
    !matches!(
        color_type,
        ColorType::L8 | ColorType::La8 | ColorType::L16 | ColorType::La16
    )
}

fn has_heif_extension(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
}

/// Decodes one monochrome heif image into an interleaved buffer.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = Instant::now();
    let path = info.path.to_str().ok_or_else(|| {
        ImgSeqError::new(format!(
            "image path '{}' is not valid utf-8",
            info.path.display()
        ))
    })?;
    let context = HeifContext::read_from_file(path)
        .map_err(|error| image_error("open", &info.path, error))?;
    let handle = context.primary_image_handle().map_err(|error| {
        ImgSeqError::new(format!(
            "failed to read the primary image of '{}': {error}",
            info.path.display()
        ))
    })?;
    let open = open_started.elapsed();

    let metadata_started = Instant::now();
    if (handle.width(), handle.height()) != (info.width, info.height) {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {}x{}, now {}x{})",
            info.path.display(),
            info.width,
            info.height,
            handle.width(),
            handle.height(),
        )));
    }
    let expects_alpha = crate::pixel::alpha_channel(info.color_type).is_some();
    let channels = if expects_alpha { 2 } else { 1 };
    let width = usize::try_from(info.width)
        .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
    let height = usize::try_from(info.height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    let row_bytes = row_bytes(width, info.format.bytes_per_sample(), channels)?;
    let size = row_bytes
        .checked_mul(height)
        .ok_or_else(|| ImgSeqError::new("image is too large"))?;
    let metadata = metadata_started.elapsed();

    let buffer_started = Instant::now();
    let mut pixels = vec![0; size];
    let buffer = buffer_started.elapsed();

    let read_started = Instant::now();
    let image = LibHeif::new()
        .decode(&handle, ColorSpace::Monochrome, None)
        .map_err(|error| image_error("decode", &info.path, error))?;
    let planes = image.planes();
    let luma = planes.y.as_ref().ok_or_else(|| {
        ImgSeqError::new(format!(
            "decoded image '{}' has no luma plane",
            info.path.display()
        ))
    })?;
    // An alpha plane is only packed when the probe reported alpha, so a plane
    // libheif did not decode cannot end up as an alpha channel of zeros or as
    // a half filled buffer the frame writer would misread.
    let alpha = match (expects_alpha, planes.a.as_ref()) {
        (true, Some(plane)) => Some(plane),
        (true, None) => {
            return Err(ImgSeqError::new(format!(
                "decoded image '{}' has no alpha plane, but it was probed as {}",
                info.path.display(),
                info.format.name(),
            )));
        }
        (false, _) => None,
    };
    pack_planes(info, luma, alpha, &mut pixels)?;
    let read = read_started.elapsed();

    Ok(DecodedImage {
        width: info.width,
        height: info.height,
        format: info.format,
        pixels: Pixels::Interleaved {
            color_type: info.color_type,
            buffer: pixels,
        },
        timings: DecodeTimings {
            open,
            metadata,
            buffer,
            read,
        },
    })
}

/// Row size of a packed image row.
fn row_bytes(width: usize, sample_bytes: usize, channels: usize) -> Result<usize> {
    width
        .checked_mul(channels)
        .and_then(|value| value.checked_mul(sample_bytes))
        .ok_or_else(|| ImgSeqError::new("image row is too large"))
}

/// Copies the luma plane, and the alpha plane when the color type has one,
/// into the packed buffer.
///
/// Rows are copied one at a time because `libheif` pads every row to the
/// plane's stride, and `La8`/`La16` interleave the two planes into pixel pairs.
fn pack_planes(
    info: &ImageInfo,
    luma: &Plane<&[u8]>,
    alpha: Option<&Plane<&[u8]>>,
    pixels: &mut [u8],
) -> Result<()> {
    let sample_bytes = info.format.bytes_per_sample();
    let luma_row = plane_row_bytes(info, luma, "luma", sample_bytes)?;
    let height = usize::try_from(luma.height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    let alpha_row = match alpha {
        Some(plane) => Some(plane_row_bytes(info, plane, "alpha", sample_bytes)?),
        None => None,
    };
    let packed_row = luma_row
        .checked_mul(if alpha_row.is_some() { 2 } else { 1 })
        .ok_or_else(|| ImgSeqError::new("image row is too large"))?;

    for y in 0..height {
        let start = y * packed_row;
        let luma_source = plane_row(info, luma, luma_row, y, "luma")?;
        let destination = pixels
            .get_mut(start..start + packed_row)
            .ok_or_else(|| ImgSeqError::new("packed image is smaller than its own rows"))?;
        match (alpha, alpha_row) {
            (Some(alpha), Some(alpha_row)) => {
                let alpha_source = plane_row(info, alpha, alpha_row, y, "alpha")?;
                for (index, pixel) in destination.chunks_exact_mut(2 * sample_bytes).enumerate() {
                    let sample = index * sample_bytes;
                    pixel[..sample_bytes]
                        .copy_from_slice(&luma_source[sample..sample + sample_bytes]);
                    pixel[sample_bytes..]
                        .copy_from_slice(&alpha_source[sample..sample + sample_bytes]);
                }
            }
            _ => destination.copy_from_slice(luma_source),
        }
    }
    Ok(())
}

/// Bytes one row of `plane` occupies, checked against the probed format.
fn plane_row_bytes(
    info: &ImageInfo,
    plane: &Plane<&[u8]>,
    name: &str,
    sample_bytes: usize,
) -> Result<usize> {
    if (plane.width, plane.height) != (info.width, info.height) {
        return Err(ImgSeqError::new(format!(
            "{name} plane of image '{}' is {}x{}, expected {}x{}",
            info.path.display(),
            plane.width,
            plane.height,
            info.width,
            info.height,
        )));
    }
    let stored = usize::from(plane.storage_bits_per_pixel) / 8;
    if stored != 0 && stored != sample_bytes {
        return Err(ImgSeqError::new(format!(
            "{name} plane of image '{}' stores {stored} bytes per sample, but the probe reported a format of {sample_bytes}",
            info.path.display(),
        )));
    }
    let row = usize::try_from(plane.width)
        .ok()
        .and_then(|width| width.checked_mul(sample_bytes))
        .filter(|row| *row <= plane.stride)
        .ok_or_else(|| {
            ImgSeqError::new(format!(
                "{name} plane of image '{}' does not fit its stride of {}",
                info.path.display(),
                plane.stride,
            ))
        })?;
    Ok(row)
}

/// One row of `plane` of `length` bytes, without the padding.
fn plane_row<'a>(
    info: &ImageInfo,
    plane: &'a Plane<&'a [u8]>,
    length: usize,
    y: usize,
    name: &str,
) -> Result<&'a [u8]> {
    plane
        .data
        .get(y * plane.stride..y * plane.stride + length)
        .ok_or_else(|| {
            ImgSeqError::new(format!(
                "{name} plane of image '{}' is shorter than its {} rows of stride {}",
                info.path.display(),
                plane.height,
                plane.stride,
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ExtendedColorType, metadata::Orientation};
    use std::path::PathBuf;

    use crate::pixel::PixelFormat;

    fn info(path: &str, color_type: ColorType) -> ImageInfo {
        sized_info(path, color_type, 3, 2)
    }

    fn sized_info(path: &str, color_type: ColorType, width: u32, height: u32) -> ImageInfo {
        ImageInfo {
            path: PathBuf::from(path),
            width,
            height,
            color_type,
            original_color_type: ExtendedColorType::L8,
            has_icc_profile: false,
            orientation: Orientation::NoTransforms,
            format: PixelFormat::from_color_type(color_type).expect("a supported color type"),
        }
    }

    fn plane(data: &[u8], width: u32, height: u32, stride: usize, storage: u8) -> Plane<&[u8]> {
        Plane {
            data,
            width,
            height,
            stride,
            bits_per_pixel: storage,
            storage_bits_per_pixel: storage,
        }
    }

    #[test]
    fn only_heif_extensions_are_taken_over() {
        for path in ["a.heic", "b.heics", "c.heif", "d.HIF"] {
            assert!(has_heif_extension(Path::new(path)), "{path}");
        }
        for path in ["a.jpg", "b.png", "c.avif", "d"] {
            assert!(!has_heif_extension(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn monochrome_is_the_case_the_integration_cannot_read() {
        assert!(!integration_can_decode(ColorType::L8));
        assert!(!integration_can_decode(ColorType::La16));
        assert!(integration_can_decode(ColorType::Rgb8));
        assert!(integration_can_decode(ColorType::Rgba16));
    }

    #[test]
    fn handles_needs_both_the_container_and_a_monochrome_probe() {
        assert!(handles(&info("page.heic", ColorType::L8)));
        assert!(!handles(&info("page.heic", ColorType::Rgb8)));
        assert!(!handles(&info("page.png", ColorType::L8)));
    }

    #[test]
    fn padded_rows_are_packed_without_the_padding() {
        let buffer = [1, 2, 3, 9, 9, 4, 5, 6, 9, 9];
        let mut pixels = vec![0; 6];
        let luma = plane(&buffer, 3, 2, 5, 8);
        pack_planes(&info("a.heic", ColorType::L8), &luma, None, &mut pixels).unwrap();
        assert_eq!(pixels, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn alpha_is_interleaved_with_the_luma() {
        let luma_buffer = [10, 11, 0, 0, 20, 21, 0, 0];
        let alpha_buffer = [30, 31, 0, 40, 41, 0];
        let luma = plane(&luma_buffer, 2, 2, 4, 8);
        let alpha = plane(&alpha_buffer, 2, 2, 3, 8);
        let mut pixels = vec![0; 8];
        let info = sized_info("a.heic", ColorType::La8, 2, 2);
        pack_planes(&info, &luma, Some(&alpha), &mut pixels).unwrap();
        assert_eq!(pixels, [10, 30, 11, 31, 20, 40, 21, 41]);
    }

    #[test]
    fn sixteen_bit_samples_are_copied_byte_for_byte() {
        let buffer = [0x34, 0x12, 0x78, 0x56, 0xff, 0xff];
        let mut pixels = vec![0; 4];
        let luma = plane(&buffer, 2, 1, 6, 16);
        let info = sized_info("a.heif", ColorType::L16, 2, 1);
        pack_planes(&info, &luma, None, &mut pixels).unwrap();
        assert_eq!(pixels, [0x34, 0x12, 0x78, 0x56]);
    }

    #[test]
    fn a_plane_that_does_not_fit_its_stride_is_rejected() {
        let buffer = [1, 2, 3, 4, 5, 6];
        let mut pixels = vec![0; 6];
        let luma = plane(&buffer, 3, 2, 2, 8);
        let error = pack_planes(&info("a.heic", ColorType::L8), &luma, None, &mut pixels)
            .expect_err("a row wider than the stride cannot be packed");
        assert!(error.to_string().contains("stride"), "{error}");
    }

    #[test]
    fn a_plane_with_the_wrong_size_is_rejected() {
        let buffer = [1, 2, 3, 4, 5, 6];
        let mut pixels = vec![0; 6];
        let luma = plane(&buffer, 2, 2, 4, 8);
        let error = pack_planes(&info("a.heic", ColorType::L8), &luma, None, &mut pixels)
            .expect_err("the probe reported a different size");
        assert!(error.to_string().contains("expected 3x2"), "{error}");
    }
}
