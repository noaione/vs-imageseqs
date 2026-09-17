use vapoursynth4_rs::frame::{Frame, VideoFrame};
use vapoursynth4_rs::map::{AppendMode, Value};
use vapoursynth4_rs::{ffi, key};

use crate::{
    decoder::ImageInfo,
    error::{ImgSeqError, Result},
};

pub fn set_frame_properties(frame: &mut VideoFrame, image: &ImageInfo, index: usize) -> Result<()> {
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
        .set(
            key!(c"_Range"),
            Value::Int(i64::from(ffi::VSRange::VSC_RANGE_FULL as i32)),
            AppendMode::Replace,
        )
        .map_err(ImgSeqError::from_display)?;
    properties
        .set(key!(c"_FieldBased"), Value::Int(0), AppendMode::Replace)
        .map_err(ImgSeqError::from_display)?;

    if matches!(
        image.format.color_family(),
        vapoursynth4_rs::ColorFamily::RGB
    ) {
        properties
            .set(
                key!(c"_Matrix"),
                Value::Int(i64::from(ffi::VSMatrixCoefficients::VSC_MATRIX_RGB as i32)),
                AppendMode::Replace,
            )
            .map_err(ImgSeqError::from_display)?;
    }

    Ok(())
}
