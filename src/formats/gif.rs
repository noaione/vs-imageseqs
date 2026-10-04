//! Gif stills, read from the container's own crate.
//!
//! A gif is a sequence of *sub-rectangles* drawn onto a logical screen, so a
//! reader that hands a frame back as the picture gets two things wrong: the size
//! is the screen rather than the rectangle, and every sample the rectangle does
//! not cover is the reader's to decide. The compositor that answers both lives in
//! [`crate::animation::gif`], because the animation path needs it for every
//! presentation it hands out; this module is the still path onto the same canvas,
//! and it exists so that a one frame gif is read here rather than by the `image`
//! crate's own reader, which this tree no longer links.
//!
//! What a still gif is *described* as is the `gif` crate's own header read, which
//! is also where `image`'s gif reader took it:
//!
//! - The size is the logical screen, whatever the one frame's rectangle says.
//! - The colour type is `Rgba8` unconditionally. A gif's samples are palette
//!   indices with a per-frame transparency index, so the crate composites to four
//!   channels and reports four channels even for a file with nothing transparent
//!   in it. `image`'s reader does the same (`ColorType::Rgba8`, in as many words),
//!   which is why a still gif has always come out with an alpha clip.
//! - There is no orientation: a gif states none, and `image`'s reader implements
//!   no accessor for one.
//! - There *is* an embedded profile, in the `ICCRGBG1012` application extension,
//!   and `image` read it through the same `icc_profile()` this module calls. A gif
//!   that carries one therefore still reports `ImgSeqHasICC`.

use std::{path::Path, sync::Arc};

use crate::{
    animation::gif as composite,
    decoder::{DecodedImage, ImageInfo},
    error::{ImgSeqError, Result},
    formats::identify,
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// The colour type a gif is described as, whatever its samples are.
///
/// `image`'s gif reader answers `ColorType::Rgba8` here without looking at the
/// file, and the frames this replaces were built from that answer, so a file with
/// nothing transparent in it still has an alpha clip.
const COLOR_TYPE: ColorType = ColorType::Rgba8;

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    identify::owns(identify::Format::Gif, path)
}

/// What a gif states, when this module reads the file.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info(path: &Path, _apply_rotation: bool) -> Result<Option<ImageInfo>> {
    if !owns(path) {
        return Ok(None);
    }
    let (width, height, icc_profile) = composite::screen(path)?;
    let profile = icc_profile.map(Arc::<[u8]>::from);
    Ok(Some(ImageInfo {
        path: path.to_path_buf(),
        width,
        height,
        color_type: COLOR_TYPE,
        original_color_type: SourceColorType::Rgba8,
        has_icc_profile: profile.is_some(),
        icc_profile: profile,
        cicp: None,
        chroma_location: None,
        // A gif states no orientation and `image`'s reader has no accessor for
        // one, so the picture is handed out as stored whether or not the caller
        // asked for a rotation. That is why the parameter goes unused.
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: PixelFormat::from_color_type(COLOR_TYPE).ok_or_else(|| {
            ImgSeqError::new(format!(
                "image '{}' is described as a colour type with no frame format",
                path.display()
            ))
        })?,
    }))
}

/// Decodes a gif into one interleaved buffer of four channel samples.
///
/// The picture is the one frame drawn onto the logical screen, which is what
/// [`composite`] composes and what `image`'s reader produced before it: the screen
/// is the size, and the samples outside the rectangle are transparent rather than
/// the background colour the file names.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or is malformed.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    // The compositor qualifies its own errors with the path already, so there is
    // nothing to add here: wrapping them would read "failed to decode image 'x':
    // failed to decode image 'x': ...".
    composite::still(&info.path, info.transform, info.format)
}
