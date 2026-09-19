use std::{
    path::{Path, PathBuf},
    sync::Once,
    time::{Duration, Instant},
};

use image::metadata::Orientation;
use image::{ColorType, ExtendedColorType, ImageDecoder, ImageReader};
use vapoursynth4_rs::ffi;

use crate::{
    color::Cicp,
    error::{ImgSeqError, Result},
    formats,
    pixel::{PixelFormat, Transform},
};

static DECODER_HOOKS: Once = Once::new();

fn register_decoder_hooks() {
    DECODER_HOOKS.call_once(|| {
        libheif_rs::integration::image::register_heif_decoding_hook();
        libheif_rs::integration::image::register_heic_decoding_hook();
    });
}

/// Format modules that decode what the registered hooks cannot; see
/// [`crate::formats`].
fn format_decoder(info: &ImageInfo) -> Option<Result<DecodedImage>> {
    if formats::avif::handles(info) {
        return Some(formats::avif::decode(info));
    }
    if formats::heif::handles(info) {
        return Some(formats::heif::decode(info));
    }
    if formats::webp::handles(info) {
        return Some(formats::webp::decode(info));
    }
    if formats::jxl::handles(info) {
        return Some(formats::jxl::decode(info));
    }
    if formats::jp2::handles(info) {
        return Some(formats::jp2::decode(info));
    }
    None
}

/// Format a module decodes this file into, when it is not the one the probed
/// color type suggests; see [`crate::formats`].
fn format_override(path: &Path, color_type: ColorType) -> Option<PixelFormat> {
    formats::webp::output_format(path, color_type)
        .or_else(|| formats::avif::output_format(path, color_type))
}

#[derive(Clone, Debug)]
pub struct ImageInfo {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub color_type: ColorType,
    pub original_color_type: ExtendedColorType,
    pub has_icc_profile: bool,
    /// The colour description the container states about its own samples, which
    /// is `None` for a file that states none and for one whose codes name no
    /// property; see [`crate::color::Cicp`].
    pub cicp: Option<Cicp>,
    /// The position of the chroma samples the file states, which is written as
    /// `_ChromaLocation` for a subsampled frame and is `None` for a file that
    /// names no position; see [`crate::color::chroma_location`].
    pub chroma_location: Option<ffi::VSChromaLocation>,
    /// The exif orientation the file states, which is reported as a property
    /// whatever the caller asked for.
    pub orientation: Orientation,
    /// The rearrangement the pixels are written with, which is the identity
    /// when the file states no orientation or the caller turned rotation off.
    pub transform: Transform,
    pub format: PixelFormat,
}

impl ImageInfo {
    /// Width this image is handed out as, which is its height for an
    /// orientation that transposes the picture.
    #[must_use]
    pub const fn output_width(&self) -> u32 {
        orientation_size(self.transform, self.width, self.height).0
    }

    /// Height this image is handed out as.
    #[must_use]
    pub const fn output_height(&self) -> u32 {
        orientation_size(self.transform, self.width, self.height).1
    }
}

#[derive(Clone, Debug)]
pub struct DecodeTimings {
    pub open: Duration,
    pub metadata: Duration,
    pub buffer: Duration,
    pub read: Duration,
}

/// Pixels of one decoded image, in whichever layout its format decodes to.
#[derive(Debug, Eq, PartialEq)]
pub enum Pixels {
    /// One buffer holding every channel of `color_type`, interleaved.
    Interleaved {
        color_type: ColorType,
        buffer: Vec<u8>,
    },
    /// One tightly packed buffer per plane of the frame format.
    ///
    /// `alpha` is the alpha item of the file, tightly packed and the size of
    /// the image, which is `None` for a file without one. It is a plane of its
    /// own rather than a channel of the last plane because the alpha clip is a
    /// gray frame of the same depth.
    Planar {
        planes: Vec<Vec<u8>>,
        alpha: Option<Vec<u8>>,
    },
}

#[derive(Debug)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// The format the probe recorded for this image, which is the format of the
    /// frame it is written into.
    pub format: PixelFormat,
    /// How the samples are rearranged as they are written, from the probe.
    pub transform: Transform,
    pub pixels: Pixels,
    pub timings: DecodeTimings,
}

impl DecodedImage {
    /// Width of the frame these pixels are written into, which is the stored
    /// height for an orientation that transposes the picture.
    #[must_use]
    pub const fn output_width(&self) -> u32 {
        orientation_size(self.transform, self.width, self.height).0
    }

    /// Height of the frame these pixels are written into.
    #[must_use]
    pub const fn output_height(&self) -> u32 {
        orientation_size(self.transform, self.width, self.height).1
    }
}

/// Size `width`x`height` is handed out as under `transform`.
///
/// The probe and the decode result answer the same question, and the frame the
/// pixels are written into is this size rather than the size the file holds.
#[must_use]
pub const fn orientation_size(transform: Transform, width: u32, height: u32) -> (u32, u32) {
    if transform.transposes() {
        (height, width)
    } else {
        (width, height)
    }
}

fn open_decoder(path: &Path) -> Result<impl ImageDecoder> {
    register_decoder_hooks();
    let reader = ImageReader::open(path)
        .map_err(|error| image_error("open", path, error))?
        .with_guessed_format()
        .map_err(|error| image_error("identify", path, error))?;
    reader
        .into_decoder()
        .map_err(|error| image_error("create decoder for", path, error))
}

/// Builds the error every decoder path reports, so the format modules in
/// [`crate::formats`] word theirs the same way.
pub(crate) fn image_error(action: &str, path: &Path, error: impl std::fmt::Display) -> ImgSeqError {
    ImgSeqError::new(format!(
        "failed to {action} image '{}': {error}",
        path.display()
    ))
}

pub fn probe(path: &Path, apply_rotation: bool) -> Result<ImageInfo> {
    // A file a format module can describe from its container skips the decoder,
    // which for an avif means skipping a decode of the whole picture; see
    // [`crate::formats::avif::image_info`] and [`crate::formats::heif::image_info`].
    if let Some(info) = formats::heif::image_info(path) {
        return Ok(info);
    }
    if let Some(info) = formats::avif::image_info(path) {
        return Ok(info);
    }
    // A jpeg xl is read here whether or not the `image` crate could reach a
    // decoder for one, which it cannot: it has no jpeg xl format of its own, and
    // the hook that taught it one is gone. See [`crate::formats::jxl`].
    if formats::jxl::owns(path) {
        return formats::jxl::image_info(path, apply_rotation);
    }
    if formats::jp2::owns(path) {
        return formats::jp2::image_info(path, apply_rotation);
    }

    let mut decoder = open_decoder(path)?;
    let (width, height) = decoder.dimensions();
    let color_type = decoder.color_type();
    let original_color_type = decoder.original_color_type();
    let has_icc_profile = decoder
        .icc_profile()
        .map_err(|error| image_error("read metadata from", path, error))?
        .is_some();
    // The containers that state a colour description do it somewhere the `image`
    // decoder has no accessor for: a heif item property inside a `libheif`
    // handle, or a chunk beside the data of a png. Both are read from the file
    // rather than from the decoder, and both decline a file of another kind
    // without opening it.
    let cicp = formats::heif::cicp(path).or_else(|| formats::png::cicp(path));
    let orientation = decoder
        .orientation()
        .map_err(|error| image_error("read orientation from", path, error))?;
    let format = format_override(path, color_type)
        .or_else(|| PixelFormat::from_color_type(color_type))
        .ok_or_else(|| {
            ImgSeqError::new(format!(
                "unsupported color type {color_type:?} in image '{}'",
                path.display()
            ))
        })?;

    Ok(ImageInfo {
        path: path.to_path_buf(),
        width,
        height,
        color_type,
        original_color_type,
        has_icc_profile,
        cicp,
        // The `image` decoders have no accessor for a chroma sample position,
        // and the containers that state one are read by the modules that know
        // how to read it out of their own boxes.
        chroma_location: None,
        orientation,
        transform: if apply_rotation {
            Transform::from_orientation(orientation)
        } else {
            Transform::IDENTITY
        },
        format,
    })
}

pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    if let Some(decoded) = format_decoder(info) {
        return decoded;
    }

    let open_started = Instant::now();
    let decoder = open_decoder(&info.path)?;
    let open = open_started.elapsed();

    let metadata_started = Instant::now();
    let (width, height) = decoder.dimensions();
    let color_type = decoder.color_type();
    let metadata = metadata_started.elapsed();
    if (width, height, color_type) != (info.width, info.height, info.color_type) {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {old_width}x{old_height} {old:?}, now {new_width}x{new_height} {new:?})",
            info.path.display(),
            old_width = info.width,
            old_height = info.height,
            old = info.color_type,
            new_width = width,
            new_height = height,
            new = color_type,
        )));
    }

    let size = usize::try_from(decoder.total_bytes()).map_err(|_| {
        ImgSeqError::new(format!(
            "decoded image '{}' is too large for this platform",
            info.path.display()
        ))
    })?;
    let buffer_started = Instant::now();
    let mut pixels = vec![0; size];
    let buffer = buffer_started.elapsed();
    let read_started = Instant::now();
    decoder
        .read_image(&mut pixels)
        .map_err(|error| image_error("decode", &info.path, error))?;
    let read = read_started.elapsed();

    Ok(DecodedImage {
        width,
        height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Interleaved {
            color_type,
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
