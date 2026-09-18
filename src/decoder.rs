use std::{
    path::{Path, PathBuf},
    sync::Once,
    time::{Duration, Instant},
};

use image::metadata::Orientation;
use image::{ColorType, ExtendedColorType, ImageDecoder, ImageReader};

use crate::{
    error::{ImgSeqError, Result},
    formats,
    pixel::PixelFormat,
};

static DECODER_HOOKS: Once = Once::new();

fn register_decoder_hooks() {
    DECODER_HOOKS.call_once(|| {
        jxl_image_rs_integration::register_image_decoding_hook();
        libheif_rs::integration::image::register_heif_decoding_hook();
        libheif_rs::integration::image::register_heic_decoding_hook();
    });
}

/// Format modules that decode what the registered hooks cannot; see
/// [`crate::formats`].
fn format_decoder(info: &ImageInfo) -> Option<Result<DecodedImage>> {
    if formats::heif::handles(info) {
        return Some(formats::heif::decode(info));
    }
    None
}

#[derive(Clone, Debug)]
pub struct ImageInfo {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub color_type: ColorType,
    pub original_color_type: ExtendedColorType,
    pub has_icc_profile: bool,
    pub orientation: Orientation,
    pub format: PixelFormat,
}

#[derive(Debug)]
pub struct DecodeTimings {
    pub open: Duration,
    pub metadata: Duration,
    pub buffer: Duration,
    pub read: Duration,
}

#[derive(Debug)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub color_type: ColorType,
    pub pixels: Vec<u8>,
    pub timings: DecodeTimings,
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

pub fn probe(path: &Path) -> Result<ImageInfo> {
    let mut decoder = open_decoder(path)?;
    let (width, height) = decoder.dimensions();
    let color_type = decoder.color_type();
    let original_color_type = decoder.original_color_type();
    let has_icc_profile = decoder
        .icc_profile()
        .map_err(|error| image_error("read metadata from", path, error))?
        .is_some();
    let orientation = decoder
        .orientation()
        .map_err(|error| image_error("read orientation from", path, error))?;
    let format = PixelFormat::from_color_type(color_type).ok_or_else(|| {
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
        orientation,
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
        color_type,
        pixels,
        timings: DecodeTimings {
            open,
            metadata,
            buffer,
            read,
        },
    })
}
