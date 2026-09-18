use vapoursynth4_rs::frame::{Frame, VideoFrame};
use vapoursynth4_rs::map::{AppendMode, Value};
use vapoursynth4_rs::{ColorFamily, ffi, key};

use crate::{
    decoder::ImageInfo,
    error::{ImgSeqError, Result},
    pixel::PixelFormat,
};

/// Attaches the source metadata of `image` to a frame of `format`.
///
/// `format` is the pixel format of the clip that owns the frame, so an alpha
/// clip is never described as RGB, and `alpha_marker` is only set when the
/// frame belongs to an alpha clip.
pub fn set_frame_properties(
    frame: &mut VideoFrame,
    image: &ImageInfo,
    index: usize,
    format: PixelFormat,
    alpha_marker: Option<bool>,
) -> Result<()> {
    let Some(mut properties) = frame.properties_mut() else {
        return Err(ImgSeqError::new("VapourSynth frame has no property map"));
    };
    let path = image.path.to_string_lossy();
    let index = i64::try_from(index)
        .map_err(|_| ImgSeqError::new("frame index does not fit in an Int property"))?;
    let original_color_type = format!("{:?}", image.original_color_type);

    properties
        .set(key!(c"ImgSeqPath"), Value::Utf8(&path), AppendMode::Replace)
        .map_err(ImgSeqError::from_display)?;
    properties
        .set(key!(c"ImgSeqIndex"), Value::Int(index), AppendMode::Replace)
        .map_err(ImgSeqError::from_display)?;
    properties
        .set(
            key!(c"ImgSeqOriginalColorType"),
            Value::Utf8(&original_color_type),
            AppendMode::Replace,
        )
        .map_err(ImgSeqError::from_display)?;
    properties
        .set(
            key!(c"ImgSeqHasICC"),
            Value::Int(i64::from(image.has_icc_profile)),
            AppendMode::Replace,
        )
        .map_err(ImgSeqError::from_display)?;
    properties
        .set(
            key!(c"ImgSeqOrientation"),
            Value::Int(i64::from(image.orientation.to_exif())),
            AppendMode::Replace,
        )
        .map_err(ImgSeqError::from_display)?;
    properties
        .set(key!(c"_FieldBased"), Value::Int(0), AppendMode::Replace)
        .map_err(ImgSeqError::from_display)?;

    // Rgb frames are what an image file means, and libwebp converts yuv to rgb
    // with the bt.601 matrix the vp8 specification defines for the limited
    // range, which is also what ffmpeg assumes for the same bitstreams. So the
    // planes this plugin hands out for a lossy webp are the ones that matrix
    // and range describe, whichever of the two paths produced the frame.
    let (matrix, range) = match format.color_family() {
        ColorFamily::YUV => (
            ffi::VSMatrixCoefficients::VSC_MATRIX_BT470_BG,
            ffi::VSRange::VSC_RANGE_LIMITED,
        ),
        _ => (
            ffi::VSMatrixCoefficients::VSC_MATRIX_RGB,
            ffi::VSRange::VSC_RANGE_FULL,
        ),
    };
    if matches!(format.color_family(), ColorFamily::RGB | ColorFamily::YUV) {
        properties
            .set(
                key!(c"_Matrix"),
                Value::Int(i64::from(matrix as i32)),
                AppendMode::Replace,
            )
            .map_err(ImgSeqError::from_display)?;
    }
    properties
        .set(
            key!(c"_Range"),
            Value::Int(i64::from(range as i32)),
            AppendMode::Replace,
        )
        .map_err(ImgSeqError::from_display)?;

    if let Some(alpha) = alpha_marker {
        properties
            .set(
                key!(c"ImgSeqAlpha"),
                Value::Int(i64::from(alpha)),
                AppendMode::Replace,
            )
            .map_err(ImgSeqError::from_display)?;
    }

    Ok(())
}
