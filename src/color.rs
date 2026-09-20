use vapoursynth4_rs::frame::{Frame, VideoFrame};
use vapoursynth4_rs::map::{AppendMode, Value};
use vapoursynth4_rs::{ColorFamily, ffi, key};

use crate::{
    decoder::ImageInfo,
    error::{ImgSeqError, Result},
    pixel::PixelFormat,
};

/// The colour description a container states about its own samples.
///
/// The four fields are the H.273 code points as the file stores them, which are
/// also the numbers VapourSynth's properties use:
/// [`ffi::VSColorPrimaries`], [`ffi::VSTransferCharacteristics`] and
/// [`ffi::VSMatrixCoefficients`]. They are kept as the codes the file has rather
/// than as the properties, because not every code has a property and "the file
/// states it or the property is not written" is the rule this type exists for;
/// see `docs/improvements/08-color-metadata.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cicp {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

/// The code every family of these enums uses for "unspecified".
///
/// It is a statement, but not a description, so it leaves a property unset
/// exactly like a file that states nothing at all.
pub const UNSPECIFIED: u8 = 2;

impl Cicp {
    /// The `_Primaries` this file states, or `None` when it states none.
    #[must_use]
    pub const fn primaries(&self) -> Option<ffi::VSColorPrimaries> {
        use ffi::VSColorPrimaries as P;
        Some(match self.primaries {
            1 => P::VSC_PRIMARIES_BT709,
            4 => P::VSC_PRIMARIES_BT470_M,
            5 => P::VSC_PRIMARIES_BT470_BG,
            6 => P::VSC_PRIMARIES_ST170_M,
            7 => P::VSC_PRIMARIES_ST240_M,
            8 => P::VSC_PRIMARIES_FILM,
            9 => P::VSC_PRIMARIES_BT2020,
            10 => P::VSC_PRIMARIES_ST428,
            11 => P::VSC_PRIMARIES_ST431_2,
            12 => P::VSC_PRIMARIES_ST432_1,
            22 => P::VSC_PRIMARIES_EBU3213_E,
            // Unspecified, and every code VapourSynth defines no property for.
            _ => return None,
        })
    }

    /// The `_Transfer` this file states, or `None` when it states none.
    #[must_use]
    pub const fn transfer(&self) -> Option<ffi::VSTransferCharacteristics> {
        use ffi::VSTransferCharacteristics as T;
        Some(match self.transfer {
            1 => T::VSC_TRANSFER_BT709,
            4 => T::VSC_TRANSFER_BT470_M,
            5 => T::VSC_TRANSFER_BT470_BG,
            6 => T::VSC_TRANSFER_BT601,
            7 => T::VSC_TRANSFER_ST240_M,
            8 => T::VSC_TRANSFER_LINEAR,
            9 => T::VSC_TRANSFER_LOG_100,
            10 => T::VSC_TRANSFER_LOG_316,
            11 => T::VSC_TRANSFER_IEC_61966_2_4,
            13 => T::VSC_TRANSFER_IEC_61966_2_1,
            14 => T::VSC_TRANSFER_BT2020_10,
            15 => T::VSC_TRANSFER_BT2020_12,
            16 => T::VSC_TRANSFER_ST2084,
            18 => T::VSC_TRANSFER_ARIB_B67,
            // Unspecified, 3, 12, 17 and every code VapourSynth has no property
            // for: the enum stops at 18 and has no room for the dci transfer.
            _ => return None,
        })
    }

    /// The `_Matrix` this file states, or `None` when it states none.
    #[must_use]
    pub const fn matrix(&self) -> Option<ffi::VSMatrixCoefficients> {
        use ffi::VSMatrixCoefficients as M;
        Some(match self.matrix {
            0 => M::VSC_MATRIX_RGB,
            1 => M::VSC_MATRIX_BT709,
            4 => M::VSC_MATRIX_FCC,
            5 => M::VSC_MATRIX_BT470_BG,
            6 => M::VSC_MATRIX_ST170_M,
            7 => M::VSC_MATRIX_ST240_M,
            8 => M::VSC_MATRIX_YCGCO,
            9 => M::VSC_MATRIX_BT2020_NCL,
            10 => M::VSC_MATRIX_BT2020_CL,
            12 => M::VSC_MATRIX_CHROMATICITY_DERIVED_NCL,
            13 => M::VSC_MATRIX_CHROMATICITY_DERIVED_CL,
            14 => M::VSC_MATRIX_ICTCP,
            // Unspecified, 3, 11, 15 and everything above the enum.
            _ => return None,
        })
    }

    /// The `_Range` this file states.
    #[must_use]
    pub const fn range(&self) -> ffi::VSRange {
        if self.full_range {
            ffi::VSRange::VSC_RANGE_FULL
        } else {
            ffi::VSRange::VSC_RANGE_LIMITED
        }
    }
}

/// The `_ChromaLocation` a chroma sample position states, from the two bit field
/// of an av1 coding record.
///
/// The field names three things: unknown, the chroma samples vertically between
/// the luma samples and co-sited with them horizontally, and the samples
/// co-sited both ways. The first is not a position, so it leaves the property
/// unset exactly like a file with no field at all; the other two are the two
/// positions VapourSynth has for them, which is how a dav1d based tool reports
/// the same bitstream.
#[must_use]
pub const fn chroma_location(position: u8) -> Option<ffi::VSChromaLocation> {
    match position {
        1 => Some(ffi::VSChromaLocation::VSC_CHROMA_LEFT),
        2 => Some(ffi::VSChromaLocation::VSC_CHROMA_TOP_LEFT),
        _ => None,
    }
}

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
    export_icc_profile: bool,
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
    if export_icc_profile && let Some(profile) = image.icc_profile.as_deref() {
        properties
            .set(
                key!(c"ICCProfile"),
                Value::Data(profile),
                AppendMode::Replace,
            )
            .map_err(ImgSeqError::from_display)?;
    }
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

    // The file states its colour or the property is not written at all, which is
    // the whole rule of `docs/improvements/08-color-metadata.md`. Primaries and
    // transfer describe any family, not only yuv: a png that states sRGB is
    // describing the r,g,b this frame holds.
    if let Some(cicp) = image.cicp {
        if let Some(primaries) = cicp.primaries() {
            properties
                .set(
                    key!(c"_Primaries"),
                    Value::Int(i64::from(primaries as i32)),
                    AppendMode::Replace,
                )
                .map_err(ImgSeqError::from_display)?;
        }
        if let Some(transfer) = cicp.transfer() {
            properties
                .set(
                    key!(c"_Transfer"),
                    Value::Int(i64::from(transfer as i32)),
                    AppendMode::Replace,
                )
                .map_err(ImgSeqError::from_display)?;
        }
    }

    // Rgb frames are what an image file means, and libwebp converts yuv to rgb
    // with the bt.601 matrix the vp8 specification defines for the limited
    // range, which is also what ffmpeg assumes for the same bitstreams. So the
    // planes this plugin hands out for a lossy webp are the ones that matrix
    // and range describe, whichever of the two paths produced the frame.
    //
    // A file that states its own matrix and range for the yuv it holds overrides
    // that default, because those are the planes in the frame. An rgb or gray
    // frame keeps the family default whatever the flag says: the flag describes
    // the coded yuv the file may not even have, and the conversion the decoder
    // ran has already applied the matrix it names.
    let (matrix, range) = match format.color_family() {
        ColorFamily::YUV => match image.cicp {
            Some(cicp) => (
                cicp.matrix()
                    .unwrap_or(ffi::VSMatrixCoefficients::VSC_MATRIX_BT470_BG),
                cicp.range(),
            ),
            None => (
                ffi::VSMatrixCoefficients::VSC_MATRIX_BT470_BG,
                ffi::VSRange::VSC_RANGE_LIMITED,
            ),
        },
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

    // The position of the chroma samples is a statement about a subsampled yuv
    // frame, and a file that does not name one leaves the property unset rather
    // than claiming the samples sit where they do not.
    if format.sub_sampling() != (0, 0)
        && let Some(location) = image.chroma_location
    {
        properties
            .set(
                key!(c"_ChromaLocation"),
                Value::Int(i64::from(location as i32)),
                AppendMode::Replace,
            )
            .map_err(ImgSeqError::from_display)?;
    }

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
#[cfg(test)]
mod tests {
    use super::*;

    /// The code points VapourSynth has a `_Primaries` for.
    const PRIMARIES: [u8; 11] = [1, 4, 5, 6, 7, 8, 9, 10, 11, 12, 22];

    /// The code points it has a `_Transfer` for.
    const TRANSFER: [u8; 14] = [1, 4, 5, 6, 7, 8, 9, 10, 11, 13, 14, 15, 16, 18];

    /// The code points it has a `_Matrix` for.
    const MATRIX: [u8; 12] = [0, 1, 4, 5, 6, 7, 8, 9, 10, 12, 13, 14];

    /// A file that states nothing but the code under test.
    const fn stating(code: u8) -> Cicp {
        Cicp {
            primaries: code,
            transfer: code,
            matrix: code,
            full_range: false,
        }
    }

    #[test]
    fn only_the_codes_vapoursynth_names_a_property_become_one() {
        // Every code point, so a code this table gains or loses is a failure
        // rather than something no file happens to state.
        for code in 0..=255u8 {
            let stated = stating(code);
            assert_eq!(
                stated.primaries().is_some(),
                PRIMARIES.contains(&code),
                "primaries code {code}"
            );
            assert_eq!(
                stated.transfer().is_some(),
                TRANSFER.contains(&code),
                "transfer code {code}"
            );
            assert_eq!(
                stated.matrix().is_some(),
                MATRIX.contains(&code),
                "matrix code {code}"
            );
        }
    }

    #[test]
    fn unspecified_states_nothing() {
        // Code 2 is a statement, and it is not a description: it leaves every
        // property exactly as unset as a file with no colour statement at all.
        let stated = stating(UNSPECIFIED);
        assert!(stated.primaries().is_none());
        assert!(stated.transfer().is_none());
        assert!(stated.matrix().is_none());
    }

    #[test]
    fn a_property_carries_the_code_the_file_states() {
        // VapourSynth's enums are the h.273 code points, which is what makes the
        // mapping above a lookup rather than a renumbering.
        for code in PRIMARIES {
            assert_eq!(
                stating(code).primaries().expect("a stated primaries") as i32,
                i32::from(code)
            );
        }
        for code in TRANSFER {
            assert_eq!(
                stating(code).transfer().expect("a stated transfer") as i32,
                i32::from(code)
            );
        }
        for code in MATRIX {
            assert_eq!(
                stating(code).matrix().expect("a stated matrix") as i32,
                i32::from(code)
            );
        }
    }

    #[test]
    fn the_range_follows_the_flag() {
        let full = Cicp {
            full_range: true,
            ..stating(1)
        };
        assert_eq!(full.range() as i32, ffi::VSRange::VSC_RANGE_FULL as i32);
        assert_eq!(
            stating(1).range() as i32,
            ffi::VSRange::VSC_RANGE_LIMITED as i32
        );
    }
}
