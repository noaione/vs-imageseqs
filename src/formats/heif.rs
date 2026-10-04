//! heif and heic decoding through `libheif` itself.
//!
//! Every heif and avif this plugin reads goes through this module's own use of
//! libheif's API, from the planes the library hands over. There is no
//! integration with the `image` crate left to fall back on, and there is nothing
//! to fall back to: `image` reads an avif itself (`avif-native`) but has no heif
//! of its own, so `register_heic_decoding_hook` was the only thing that ever let
//! it read one. That hook is gone.
//!
//! What that buys is the picture as the container states it rather than as a
//! chosen conversion: the colour clip is the yuv format the file holds, the
//! alpha clip is the gray plane of the same depth, and an r,g,b container is
//! handed over one plane per channel. libheif's `planes()` names the channels of
//! whichever colourscheme it produced -- luma and chroma for a yuv picture, red,
//! green and blue for an r,g,b one -- so [`decode`] picks the set that matches the
//! format the probe recorded, and the frame's own plane order is what it writes
//! them into. See `docs/improvements/12-heif-avif-yuv-output.md`.
//!
//! The probe is here for the same reason, and for one more: `libheif` says what
//! a file holds without decoding it. [`image_info`] reads the size, the color
//! type, the depth, whether an alpha plane exists and the colour description
//! from the primary image handle, so the frames are sized from what the container
//! says rather than from a decode of the whole picture.
//!
//! A container libheif will not describe is left to whatever else can read it,
//! and after the hooks were removed that is nothing: [`image_info`] returning
//! `None` for a heif means the file is refused rather than handed on, which is
//! the honest answer for a container this tree cannot read.
use std::{path::Path, sync::Arc, time::Instant};

use crate::layout::{ColorType, Orientation};
use libheif_rs::{
    Chroma, ColorPrimaries, ColorProfile, ColorSpace, HeifContext, ImageHandle, LibHeif,
    MatrixCoefficients, Plane, RgbChroma, TransferCharacteristics, color_profile_types,
};
use vapoursynth4_rs::ColorFamily;

use crate::{
    color::{Cicp, UNSPECIFIED},
    decoder::{DecodeTimings, DecodedImage, Demand, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    pixel::{PixelFormat, Transform, inverse_orientation},
};

/// What the container of a heif states about its primary image, when this
/// module can describe the file from it.
///
/// `None` means this module cannot describe the file: another container, or one
/// whose image libheif reports in a shape this probe will not state a format
/// for. No reader here reads such a file.
pub fn image_info(path: &Path, apply_rotation: bool) -> Option<ImageInfo> {
    if !owns(path) {
        return None;
    }
    describe(path, apply_rotation)
}

/// Whether this module owns a file: the container says heif, the name is a hint.
///
/// The brand in the `ftyp` box is what separates a heif from an avif, and both
/// containers state one, so the bytes answer this without the name -- which is
/// what lets a renamed `heic` still be read. See [`crate::formats::identify`].
///
/// # Panics
///
/// Never: it is a read of the file's opening bytes that answers `false` when the
/// file cannot be opened at all.
#[must_use]
pub fn owns(path: &Path) -> bool {
    crate::formats::identify::owns(crate::formats::identify::Format::Heif, path)
}
/// What libheif states about a file's primary image, whatever its extension.
///
/// [`image_info`] is this plus the extension gate, and the extra entry point
/// exists because a container this tree's own avif walker refuses is libheif's
/// to read: libheif opens a grid of tiles and reports the picture it holds,
/// where the `image` decoder could not -- it has no monochrome avif, so a grid
/// of monochrome tiles ended as `Invalid argument` rather than as a frame.
///
/// Returning `None` means libheif will not open the file either, and the
/// `image` decoder keeps whatever it makes of it.
pub fn describe(path: &Path, apply_rotation: bool) -> Option<ImageInfo> {
    let handle = heif_handle(path)?;
    let header = HeifHeader::read(&handle)?;
    // The container's own `irot` and `imir`, which `libheif` applies as it
    // decodes and whose getters the wrapper does not expose; see
    // [`crate::formats::avif::container_orientation`]. A file whose boxes cannot
    // be walked is reported as stating no orientation rather than as one whose
    // orientation is unknown, which is what the property has always said for a
    // container-only transform.
    let orientation =
        crate::formats::avif::container_orientation(path).unwrap_or(Orientation::NoTransforms);
    Some(ImageInfo {
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type: header.color_type,
        // The libheif integration does not override the original color type, so
        // the probe it replaces reported this one.
        original_color_type: header.color_type.into(),
        has_icc_profile: header.has_icc_profile,
        icc_profile: header.icc_profile.clone(),
        cicp: header.cicp,
        // The position of the chroma samples of a hevc picture is in its vui,
        // which libheif does not report, and not in any item property this walk
        // reads.
        chroma_location: None,
        // libheif applies the container's own transformations as it decodes, so
        // the width and height read here are the size after them, which is the
        // size the frames are built with. Rotation on is therefore the identity
        // - the decoder already drew the picture the file describes - and
        // rotation off is what undoes it, which is the same shape
        // [`crate::formats::jxl`] has for the other decoder that applies a
        // file's orientation itself.
        orientation,
        transform: if apply_rotation {
            Transform::IDENTITY
        } else {
            Transform::from_orientation(inverse_orientation(orientation))
        },
        format: header.format()?,
    })
}

/// The primary image handle of a heif file, which is `None` for a file
/// `libheif` will not open.
fn heif_handle(path: &Path) -> Option<ImageHandle> {
    let path = path.to_str()?;
    HeifContext::read_from_file(path)
        .ok()?
        .primary_image_handle()
        .ok()
}

/// What the primary image of a heif container states about itself.
struct HeifHeader {
    width: u32,
    height: u32,
    /// The colours of the samples, which is also what decides their format.
    color_space: ColorSpace,
    /// Bits of one luma sample.
    depth: u8,
    color_type: ColorType,
    has_icc_profile: bool,
    icc_profile: Option<Arc<[u8]>>,
    /// The colour description the container states, when it states one.
    cicp: Option<Cicp>,
}

impl HeifHeader {
    /// Reads the primary image handle.
    ///
    /// The color type is the one the `image` integration probes such a file
    /// through, so a file this module leaves to it is probed identically: the
    /// preferred colorspace decides whether the samples are one per pixel and
    /// whether they are deeper than eight bits, and the alpha plane is the last
    /// channel.
    fn read(handle: &ImageHandle) -> Option<Self> {
        let color_space = handle.preferred_decoding_colorspace().ok()?;
        let depth = handle.luma_bits_per_pixel();
        let has_alpha = handle.has_alpha_channel();
        let is_hdr = match color_space {
            ColorSpace::YCbCr(_) | ColorSpace::Monochrome | ColorSpace::Undefined => depth > 8,
            // The rgb color spaces name their depth in the variant itself: the
            // `hdr` ones are sixteen bits per sample, every other one is eight.
            ColorSpace::Rgb(rgb) => matches!(
                rgb,
                RgbChroma::HdrRgbBe
                    | RgbChroma::HdrRgbaBe
                    | RgbChroma::HdrRgbLe
                    | RgbChroma::HdrRgbaLe
            ),
            _ => return None,
        };
        let color_type = match (color_space, has_alpha) {
            (ColorSpace::Monochrome, false) if is_hdr => ColorType::L16,
            (ColorSpace::Monochrome, false) => ColorType::L8,
            (ColorSpace::Monochrome, true) if is_hdr => ColorType::La16,
            (ColorSpace::Monochrome, true) => ColorType::La8,
            (_, false) if is_hdr => ColorType::Rgb16,
            (_, false) => ColorType::Rgb8,
            (_, true) if is_hdr => ColorType::Rgba16,
            (_, true) => ColorType::Rgba8,
        };
        let icc_profile = icc_profile(handle);
        Some(Self {
            width: handle.width(),
            height: handle.height(),
            color_space,
            depth,
            color_type,
            has_icc_profile: icc_profile.is_some(),
            icc_profile,
            cicp: handle.color_profile_nclx().map(|profile| Cicp {
                primaries: profile.color_primaries().code(),
                transfer: profile.transfer_characteristics().code(),
                matrix: profile.matrix_coefficients().code(),
                full_range: profile.full_range_flag() != 0,
            }),
        })
    }

    /// Format the file is handed out as.
    ///
    /// A monochrome image is the gray format its color type maps to, at the
    /// depth libheif reports for it - which is the depth of the planes it hands
    /// over, so a ten bit page is a `Gray10` frame holding the samples as they
    /// are - and a 4:2:0 one is handed out as the yuv planes it holds. Anything
    /// else keeps the `image` decoder: r,g,b samples have no yuv format to be
    /// handed out as, and a file whose matrix the frame properties cannot name
    /// would have to be labelled with a guess.
    fn format(&self) -> Option<PixelFormat> {
        match self.color_space {
            ColorSpace::Monochrome => {
                Some(PixelFormat::from_color_type(self.color_type)?.at_depth(self.depth.into()))
            }
            ColorSpace::YCbCr(chroma) => {
                self.cicp.filter(|cicp| usable_matrix(cicp.matrix))?;
                yuv_format(chroma, self.depth)
            }
            // A colour picture can be coded as full resolution r,g,b rather
            // than as yuv, which is what this plugin hands one out as. The
            // planar `C444` spelling is the one [`color_space_of`] asks for, so
            // the planes libheif hands over are already one per channel and
            // nothing has to be split. The interleaved and hdr variants are not
            // what this module requests, so a file that reports one keeps the
            // `image` decoder rather than being asked for in a spelling this
            // module would then have to take apart.
            ColorSpace::Rgb(RgbChroma::C444) => {
                Some(PixelFormat::from_color_type(self.color_type)?.at_depth(self.depth.into()))
            }
            _ => None,
        }
    }
}

/// Returns the raw ICC payload of the primary image, excluding an nclx profile.
fn icc_profile(handle: &ImageHandle) -> Option<Arc<[u8]>> {
    let profile = handle.color_profile_raw()?;
    matches!(
        profile.profile_type(),
        color_profile_types::R_ICC | color_profile_types::PROF
    )
    .then(|| Arc::from(profile.data))
}

/// Whether the chroma subsampling libheif reports is one this module hands out.
const fn yuv_format(chroma: Chroma, depth: u8) -> Option<PixelFormat> {
    match (chroma, depth) {
        (Chroma::C420, 8) => Some(PixelFormat::Yuv420P8),
        (Chroma::C422, 8) => Some(PixelFormat::Yuv422P8),
        (Chroma::C444, 8) => Some(PixelFormat::Yuv444P8),
        (Chroma::C420, 10) => Some(PixelFormat::Yuv420P10),
        (Chroma::C422, 10) => Some(PixelFormat::Yuv422P10),
        (Chroma::C444, 10) => Some(PixelFormat::Yuv444P10),
        (Chroma::C444, 12) => Some(PixelFormat::Yuv444P12),
        _ => None,
    }
}

/// Whether `_Matrix` can name the coefficients a file states.
///
/// The identity is a statement about samples that are already r,g,b, and
/// VapourSynth has no planar format for those, so a file that states it - like
/// one that states nothing, or states "unspecified" - keeps the r,g,b the
/// `image` path produces.
const fn usable_matrix(matrix: u8) -> bool {
    matrix != 0 && matrix != UNSPECIFIED
}

/// The colours libheif is asked for, so that it hands the planes over unconverted.
pub fn color_space_of(format: PixelFormat) -> Option<ColorSpace> {
    match format {
        PixelFormat::Gray8
        | PixelFormat::Gray9
        | PixelFormat::Gray10
        | PixelFormat::Gray11
        | PixelFormat::Gray12
        | PixelFormat::Gray13
        | PixelFormat::Gray14
        | PixelFormat::Gray15
        | PixelFormat::Gray16 => Some(ColorSpace::Monochrome),
        PixelFormat::Yuv420P8 | PixelFormat::Yuv420P10 => Some(ColorSpace::YCbCr(Chroma::C420)),
        PixelFormat::Yuv422P8 | PixelFormat::Yuv422P10 => Some(ColorSpace::YCbCr(Chroma::C422)),
        PixelFormat::Yuv444P8 | PixelFormat::Yuv444P10 | PixelFormat::Yuv444P12 => {
            Some(ColorSpace::YCbCr(Chroma::C444))
        }
        // A colour picture can be coded as full resolution rgb rather than as
        // yuv, which is what this plugin hands out for one: the still path
        // reaches it through the `image` decoder, and a sequence track asks
        // `libheif` for it directly. `C444` is the planar spelling, one plane
        // per channel, which is the layout a frame is written from; the
        // interleaved `Rgb` variant would have to be split again here. The
        // word, not the depth, is what the request names, so a deeper rgb frame
        // is written from the same planes.
        PixelFormat::Rgb8
        | PixelFormat::Rgb9
        | PixelFormat::Rgb10
        | PixelFormat::Rgb11
        | PixelFormat::Rgb12
        | PixelFormat::Rgb13
        | PixelFormat::Rgb14
        | PixelFormat::Rgb15
        | PixelFormat::Rgb16 => Some(ColorSpace::Rgb(RgbChroma::C444)),
        _ => None,
    }
}

/// The h.273 code point a libheif colour enum names.
///
/// The enums hold the code points the container stores, so this is the table
/// that turns what libheif reports back into what a file states. `Unspecified`
/// and `Unknown` are both "this file says nothing this module can use": the
/// first is the code point every container family has for it, and libheif
/// answers the second for a value it does not know, which the file will have
/// written for one of the codes VapourSynth has no property for.
trait H273Code {
    fn code(self) -> u8;
}

impl H273Code for ColorPrimaries {
    fn code(self) -> u8 {
        use ColorPrimaries as P;
        match self {
            P::ITU_R_BT_709_5 => 1,
            P::Unspecified | P::Unknown => UNSPECIFIED,
            P::ITU_R_BT_470_6_System_M => 4,
            P::ITU_R_BT_470_6_System_B_G => 5,
            P::ITU_R_BT_601_6 => 6,
            P::SMPTE_240M => 7,
            P::GenericFilm => 8,
            P::ITU_R_BT_2020_2_and_2100_0 => 9,
            P::SMPTE_ST_428_1 => 10,
            P::SMPTE_RP_431_2 => 11,
            P::SMPTE_EG_432_1 => 12,
            P::EBU_Tech_3213_E => 22,
        }
    }
}

impl H273Code for TransferCharacteristics {
    fn code(self) -> u8 {
        use TransferCharacteristics as T;
        match self {
            T::ITU_R_BT_709_5 => 1,
            T::Unspecified | T::Unknown => UNSPECIFIED,
            T::ITU_R_BT_470_6_System_M => 4,
            T::ITU_R_BT_470_6_System_B_G => 5,
            T::ITU_R_BT_601_6 => 6,
            T::SMPTE_240M => 7,
            T::Linear => 8,
            T::Logarithmic100 => 9,
            T::Logarithmic100Sqrt10 => 10,
            T::IEC_61966_2_4 => 11,
            T::ITU_R_BT_1361 => 12,
            T::IEC_61966_2_1 => 13,
            T::ITU_R_BT_2020_2_10bit => 14,
            T::ITU_R_BT_2020_2_12bit => 15,
            T::ITU_R_BT_2100_0_PQ => 16,
            T::SMPTE_ST_428_1 => 17,
            T::ITU_R_BT_2100_0_HLG => 18,
        }
    }
}

impl H273Code for MatrixCoefficients {
    fn code(self) -> u8 {
        use MatrixCoefficients as M;
        match self {
            M::RGB_GBR => 0,
            M::ITU_R_BT_709_5 => 1,
            M::Unspecified | M::Unknown => UNSPECIFIED,
            M::US_FCC_T47 => 4,
            M::ITU_R_BT_470_6_System_B_G => 5,
            M::ITU_R_BT_601_6 => 6,
            M::SMPTE_240M => 7,
            M::YCgCo => 8,
            M::ITU_R_BT_2020_2_NonConstantLuminance => 9,
            M::ITU_R_BT_2020_2_ConstantLuminance => 10,
            M::SMPTE_ST_2085 => 11,
            M::ChromaticityDerivedNonConstantLuminance => 12,
            M::ChromaticityDerivedConstantLuminance => 13,
            M::ICtCp => 14,
        }
    }
}

/// Decodes one heif image into the planes of its own format.
///
/// `libheif` decodes a page's alpha plane whether or not anything asks for it —
/// the wrapper has no way to suppress it — so what a colour-only call saves here
/// is the buffer and the copy that would carry the plane into a frame: see
/// [`crate::decoder::Demand`].
pub fn decode(info: &ImageInfo, demand: Demand) -> Result<DecodedImage> {
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
    // The planes libheif is asked for are the planes of the format the probe
    // recorded, so a file whose samples libheif cannot hand over as they are
    // is an error rather than a silently converted picture.
    let color_space = color_space_of(info.format).ok_or_else(|| {
        ImgSeqError::new(format!(
            "image '{}' cannot be decoded as {}",
            info.path.display(),
            info.format.name(),
        ))
    })?;
    let expects_alpha = crate::pixel::alpha_channel(info.color_type).is_some() && demand.alpha;
    let sizes = plane_sizes(info)?;
    let alpha_size = alpha_plane_size(info)?;
    let metadata = metadata_started.elapsed();

    let buffer_started = Instant::now();
    let mut planes = sizes.iter().map(|size| vec![0; *size]).collect::<Vec<_>>();
    let mut alpha = expects_alpha.then(|| vec![0; alpha_size]);
    let buffer = buffer_started.elapsed();

    let read_started = Instant::now();
    let image = LibHeif::new()
        .decode(&handle, color_space, None)
        .map_err(|error| image_error("decode", &info.path, error))?;
    if image.color_space() != Some(color_space) {
        return Err(image_error(
            "decode",
            &info.path,
            format!(
                "libheif produced {:?}, not {color_space:?}",
                image.color_space()
            ),
        ));
    }
    let decoded = image.planes();
    // libheif names the planes of the colorspace it produced, and the two
    // arrangements are different channels rather than different layouts: a yuv
    // or monochrome picture fills the luma and chroma channels, and an r,g,b
    // one fills red, green and blue instead, leaving `y` empty. The frame's
    // planes are the same three in the same order either way -- the format's
    // own plane order -- so the colorspace is the only thing that decides
    // which channel feeds which plane.
    let channels = if info.format.color_family() == ColorFamily::RGB {
        [decoded.r.as_ref(), decoded.g.as_ref(), decoded.b.as_ref()]
    } else {
        [decoded.y.as_ref(), decoded.cb.as_ref(), decoded.cr.as_ref()]
    };
    for (plane, channel) in channels.iter().enumerate().take(planes.len()) {
        pack_plane(info, *channel, plane, &mut planes[plane])?;
    }
    if let (Some(alpha), Some(target)) = (alpha.as_mut(), &decoded.a) {
        pack_plane(info, Some(target), 3, alpha)?;
    } else if expects_alpha {
        // The probe reported an alpha channel, so a picture without one is not
        // the file that was probed.
        return Err(ImgSeqError::new(format!(
            "decoded image '{}' has no alpha plane, but it was probed as {}",
            info.path.display(),
            info.format.name(),
        )));
    }
    let read = read_started.elapsed();

    Ok(DecodedImage {
        width: info.width,
        height: info.height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Planar { planes, alpha },
        timings: DecodeTimings {
            open,
            metadata,
            buffer,
            read,
        },
    })
}

/// Size of every plane of the frame this image is written into.
fn plane_sizes(info: &ImageInfo) -> Result<Vec<usize>> {
    let width = usize::try_from(info.width)
        .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
    let height = usize::try_from(info.height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    (0..info.format.plane_count())
        .map(|plane| {
            let (plane_width, plane_height) = info.format.plane_dimensions(plane, width, height);
            plane_width
                .checked_mul(plane_height)
                .and_then(|samples| samples.checked_mul(info.format.bytes_per_sample()))
                .ok_or_else(|| ImgSeqError::new("image is too large"))
        })
        .collect()
}

/// Size of the alpha plane, which is never subsampled.
fn alpha_plane_size(info: &ImageInfo) -> Result<usize> {
    let width = usize::try_from(info.width)
        .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
    let height = usize::try_from(info.height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(info.format.alpha_format().bytes_per_sample()))
        .ok_or_else(|| ImgSeqError::new("image is too large"))
}

/// Copies one plane of a decoded heif image into a tightly packed plane of the
/// frame, dropping the padding libheif keeps between its rows.
///
/// `frame_plane` is the index of the plane in the frame format, and `3` is the
/// alpha plane, which is a picture of its own and always the size of the image.
/// libheif rounds the height of a subsampled plane up to whole sample rows, so a
/// plane that is larger than the frame needs is accepted and its extra rows are
/// left behind; one that is smaller cannot fill the frame and is refused.
fn pack_plane(
    info: &ImageInfo,
    plane: Option<&Plane<&[u8]>>,
    frame_plane: usize,
    target: &mut [u8],
) -> Result<()> {
    // The name follows the format's own plane order, so an error says which
    // channel of the picture it is about rather than which channel of some
    // other colourscheme.
    let name = match (info.format.color_family(), frame_plane) {
        (_, 3) => "alpha",
        (ColorFamily::RGB, 0) => "red",
        (ColorFamily::RGB, 1) => "green",
        (ColorFamily::RGB, 2) => "blue",
        (_, 0) => "luma",
        (_, 1) => "blue chroma",
        _ => "red chroma",
    };
    let plane = plane.ok_or_else(|| {
        ImgSeqError::new(format!(
            "decoded image '{}' has no {name} plane",
            info.path.display()
        ))
    })?;
    let width = usize::try_from(info.width)
        .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
    let height = usize::try_from(info.height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    let (plane_width, plane_height) = if frame_plane < info.format.plane_count() {
        info.format.plane_dimensions(frame_plane, width, height)
    } else {
        (width, height)
    };
    let sample_bytes = info.format.bytes_per_sample();
    let row = plane_width
        .checked_mul(sample_bytes)
        .ok_or_else(|| ImgSeqError::new("image plane row is too large"))?;
    let expected = row
        .checked_mul(plane_height)
        .ok_or_else(|| ImgSeqError::new("image plane is too large"))?;
    if target.len() != expected {
        return Err(ImgSeqError::new(format!(
            "the {name} buffer is {} bytes, expected {expected}",
            target.len(),
        )));
    }
    if usize::try_from(plane.width).unwrap_or(usize::MAX) < plane_width
        || usize::try_from(plane.height).unwrap_or(usize::MAX) < plane_height
    {
        return Err(ImgSeqError::new(format!(
            "{name} plane of image '{}' is {}x{}, too small for the {plane_width}x{plane_height} the frame holds",
            info.path.display(),
            plane.width,
            plane.height,
        )));
    }
    let stored = usize::from(plane.storage_bits_per_pixel) / 8;
    if stored != 0 && stored != sample_bytes {
        return Err(ImgSeqError::new(format!(
            "{name} plane of image '{}' stores {stored} bytes per sample, but the probe reported a format of {sample_bytes}",
            info.path.display(),
        )));
    }
    if row > plane.stride {
        return Err(ImgSeqError::new(format!(
            "{name} plane of image '{}' does not fit its stride of {}",
            info.path.display(),
            plane.stride,
        )));
    }
    for (index, destination) in target.chunks_exact_mut(row).enumerate() {
        let start = index * plane.stride;
        let source = plane.data.get(start..start + row).ok_or_else(|| {
            ImgSeqError::new(format!(
                "{name} plane of image '{}' is shorter than its own rows",
                info.path.display(),
            ))
        })?;
        destination.copy_from_slice(source);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::layout::SourceColorType;

    fn info(path: &str, format: PixelFormat, color_type: ColorType) -> ImageInfo {
        sized_info(path, format, color_type, 3, 2)
    }

    fn sized_info(
        path: &str,
        format: PixelFormat,
        color_type: ColorType,
        width: u32,
        height: u32,
    ) -> ImageInfo {
        ImageInfo {
            path: PathBuf::from(path),
            width,
            height,
            color_type,
            original_color_type: SourceColorType::L8,
            has_icc_profile: false,
            icc_profile: None,
            cicp: None,
            chroma_location: None,
            orientation: Orientation::NoTransforms,
            transform: Transform::IDENTITY,
            format,
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

    /// A heif is this module's, and the route is what says so now: a `handles`
    /// here was a second copy of the rule, and asking it again cost a walk per
    /// frame request to answer a question the decode already had the answer to.
    #[test]
    fn a_heif_container_routes_to_this_module() {
        let route = crate::formats::identify::route;
        let heif = Some(crate::formats::identify::Format::Heif);
        for path in ["a.heic", "a.HEIC", "a.heif", "a.hif", "a.avci"] {
            assert_eq!(route(Path::new(path)), heif, "{path}");
        }
        // An avif shares the container and is the avif module's.
        assert_ne!(route(Path::new("a.avif")), heif);
        assert_ne!(route(Path::new("a.png")), heif);
    }

    #[test]
    fn the_subsampling_libheif_reports_maps_onto_a_format() {
        assert_eq!(yuv_format(Chroma::C420, 8), Some(PixelFormat::Yuv420P8));
        assert_eq!(yuv_format(Chroma::C422, 8), Some(PixelFormat::Yuv422P8));
        assert_eq!(yuv_format(Chroma::C444, 8), Some(PixelFormat::Yuv444P8));
        assert_eq!(yuv_format(Chroma::C420, 10), Some(PixelFormat::Yuv420P10));
        assert_eq!(yuv_format(Chroma::C422, 10), Some(PixelFormat::Yuv422P10));
        assert_eq!(yuv_format(Chroma::C444, 10), Some(PixelFormat::Yuv444P10));
        assert_eq!(yuv_format(Chroma::C444, 12), Some(PixelFormat::Yuv444P12));
        // A depth with no format of its own keeps the decoder that has one.
        assert_eq!(yuv_format(Chroma::C420, 12), None);
        assert_eq!(yuv_format(Chroma::C422, 12), None);
    }

    #[test]
    fn every_format_is_asked_of_libheif_in_its_own_colours() {
        for (format, colorspace) in [
            (PixelFormat::Gray8, ColorSpace::Monochrome),
            (PixelFormat::Gray16, ColorSpace::Monochrome),
            (PixelFormat::Yuv420P8, ColorSpace::YCbCr(Chroma::C420)),
            (PixelFormat::Yuv420P10, ColorSpace::YCbCr(Chroma::C420)),
            (PixelFormat::Yuv422P8, ColorSpace::YCbCr(Chroma::C422)),
            (PixelFormat::Yuv444P12, ColorSpace::YCbCr(Chroma::C444)),
        ] {
            assert_eq!(color_space_of(format), Some(colorspace), "{format:?}");
        }
        // A colour picture coded as full resolution rgb is asked for as planar
        // rgb, which is the layout a frame is written from.
        for format in [PixelFormat::Rgb8, PixelFormat::Rgb16, PixelFormat::Rgb10] {
            assert_eq!(
                color_space_of(format),
                Some(ColorSpace::Rgb(RgbChroma::C444)),
                "{format:?}"
            );
        }
        // A float frame is not a layout libheif has, and neither is a format
        // whose planes are not one of the ones above.
        assert_eq!(color_space_of(PixelFormat::Rgb32F), None);
    }

    #[test]
    fn a_matrix_the_properties_cannot_name_is_not_handed_out_as_yuv() {
        assert!(usable_matrix(6));
        assert!(usable_matrix(1));
        // The identity means the samples are r,g,b, and a file that states
        // nothing or "unspecified" would have to be labelled with a guess.
        assert!(!usable_matrix(0));
        assert!(!usable_matrix(UNSPECIFIED));
    }

    #[test]
    fn a_colour_fixture_is_probed_as_the_yuv_it_holds() {
        let path = PathBuf::from("tests/fixtures/alpha-rgba8.heic");
        if !path.is_file() {
            return;
        }
        let info = image_info(&path, true).expect("a colour heic");
        assert_eq!((info.width, info.height), (3, 2));
        assert_eq!(info.format, PixelFormat::Yuv420P8);
        assert_eq!(info.color_type, ColorType::Rgba8);
        assert_eq!(info.original_color_type, SourceColorType::Rgba8);
        assert_eq!(info.transform, Transform::IDENTITY);
        assert_eq!(
            info.cicp,
            Some(Cicp {
                primaries: 1,
                transfer: 13,
                matrix: 6,
                full_range: true
            })
        );
    }

    /// A heic whose container rotates it: `libheif` applies the rotation as it
    /// decodes, so rotation on is the identity at the displayed size and
    /// rotation off is what undoes it, at the stored size.
    #[test]
    fn a_rotated_heic_reports_its_code_and_hands_the_stored_picture_back() {
        for (name, code, stored, shown) in [
            (
                "orientation-heic-rot-90.heic",
                Orientation::Rotate90,
                (4, 3),
                (3, 4),
            ),
            (
                "orientation-heic-rot-180.heic",
                Orientation::Rotate180,
                (4, 3),
                (4, 3),
            ),
            (
                "orientation-heic-rot-270.heic",
                Orientation::Rotate270,
                (4, 3),
                (3, 4),
            ),
        ] {
            let path = PathBuf::from("tests/fixtures").join(name);
            if !path.is_file() {
                return;
            }
            let rotated = image_info(&path, true).expect("a rotated heic");
            assert_eq!(rotated.orientation, code, "{name}");
            // `libheif` hands the displayed picture over, so the size the decoder
            // produced is the displayed one and there is nothing left to apply.
            assert_eq!((rotated.width, rotated.height), shown, "{name}");
            assert_eq!(rotated.transform, Transform::IDENTITY, "{name}");

            let stored_info = image_info(&path, false).expect("a rotated heic");
            assert_eq!(stored_info.orientation, code, "{name}");
            assert_eq!((stored_info.width, stored_info.height), shown, "{name}");
            assert_eq!(
                (stored_info.output_width(), stored_info.output_height()),
                stored,
                "{name}"
            );
            assert_eq!(
                stored_info.transform,
                Transform::from_orientation(inverse_orientation(code)),
                "{name}"
            );
        }
    }

    /// Rotation off hands `libheif`'s displayed picture back with the transform
    /// that undoes it, which is what the frame writer applies: this module never
    /// rearranges a sample itself.
    #[test]
    fn rotation_off_undoes_a_rotated_heic_through_the_transform() {
        let path = PathBuf::from("tests/fixtures/orientation-heic-rot-90.heic");
        if !path.is_file() {
            return;
        }
        let info = image_info(&path, false).expect("a rotated heic");
        assert_eq!((info.output_width(), info.output_height()), (4, 3));
        assert_eq!(
            info.transform,
            Transform::from_orientation(inverse_orientation(Orientation::Rotate90))
        );
        let decoded = decode(&info, Demand::ALL).expect("the image is decoded");
        let Pixels::Planar { planes, .. } = decoded.pixels else {
            panic!("a yuv page is handed out as planes");
        };
        // The decoder's own picture, which the transform above turns back into
        // the stored one: the source grid read down the columns.
        assert_eq!(planes[0], [15, 11, 7, 16, 12, 8, 17, 13, 9, 18, 14, 10]);
    }

    #[test]
    fn a_monochrome_fixture_keeps_its_gray_format() {
        let path = PathBuf::from("tests/fixtures/mono-alpha.heic");
        if !path.is_file() {
            return;
        }
        let info = image_info(&path, true).expect("a monochrome heic");
        assert_eq!((info.width, info.height), (7, 5));
        assert_eq!(info.format, PixelFormat::Gray8);
        // The alpha item is a channel of the file, not of the gray frame it is
        // handed out as.
        assert_eq!(info.color_type, ColorType::La8);
    }

    #[test]
    fn another_container_is_not_described_here() {
        for path in [
            "tests/fixtures/mono-alpha.avif",
            "tests/fixtures/mono-alpha.png",
            "tests/fixtures/alpha-rgba8.jxl",
        ] {
            assert!(image_info(Path::new(path), true).is_none(), "{path}");
        }
    }

    #[test]
    fn padded_rows_are_packed_without_the_padding() {
        let buffer = [1, 2, 3, 9, 9, 4, 5, 6, 9, 9];
        let mut pixels = vec![0; 6];
        let luma = plane(&buffer, 3, 2, 5, 8);
        let info = info("a.heic", PixelFormat::Gray8, ColorType::L8);
        pack_plane(&info, Some(&luma), 0, &mut pixels).unwrap();
        assert_eq!(pixels, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn a_chroma_plane_larger_than_the_frame_is_copied_row_by_row() {
        // libheif rounds the height of a subsampled plane up to whole sample
        // rows, so the 2x2 chroma plane of a 3x3 frame can arrive as 2x3.
        let buffer = [1, 2, 0, 0, 3, 4, 0, 0, 9, 9, 0, 0];
        let mut target = vec![0; 4];
        let chroma = plane(&buffer, 2, 3, 4, 8);
        let info = sized_info("a.heic", PixelFormat::Yuv420P8, ColorType::Rgb8, 3, 3);
        pack_plane(&info, Some(&chroma), 1, &mut target).unwrap();
        assert_eq!(target, [1, 2, 3, 4], "the extra row is left behind");
    }

    #[test]
    fn sixteen_bit_samples_are_copied_byte_for_byte() {
        let buffer = [0x34, 0x12, 0x78, 0x56, 0xff, 0xff];
        let mut pixels = vec![0; 4];
        let luma = plane(&buffer, 2, 1, 6, 16);
        let info = sized_info("a.heif", PixelFormat::Gray16, ColorType::L16, 2, 1);
        pack_plane(&info, Some(&luma), 0, &mut pixels).unwrap();
        assert_eq!(pixels, [0x34, 0x12, 0x78, 0x56]);
    }

    #[test]
    fn a_plane_that_does_not_fit_its_stride_is_rejected() {
        let buffer = [1, 2, 3, 4, 5, 6];
        let mut pixels = vec![0; 6];
        let luma = plane(&buffer, 3, 2, 2, 8);
        let info = info("a.heic", PixelFormat::Gray8, ColorType::L8);
        let error = pack_plane(&info, Some(&luma), 0, &mut pixels)
            .expect_err("a row wider than the stride cannot be packed");
        assert!(error.to_string().contains("stride"), "{error}");
    }

    #[test]
    fn a_plane_with_the_wrong_size_is_rejected() {
        let buffer = [1, 2, 3, 4, 5, 6];
        let mut pixels = vec![0; 6];
        let luma = plane(&buffer, 2, 2, 4, 8);
        let info = info("a.heic", PixelFormat::Gray8, ColorType::L8);
        let error = pack_plane(&info, Some(&luma), 0, &mut pixels)
            .expect_err("the probe reported a different size");
        assert!(error.to_string().contains("too small"), "{error}");
    }

    #[test]
    fn a_plane_that_is_missing_is_rejected() {
        let mut pixels = vec![0; 6];
        let info = info("a.heic", PixelFormat::Gray8, ColorType::L8);
        let error = pack_plane(&info, None, 0, &mut pixels).expect_err("no luma plane");
        assert!(error.to_string().contains("no luma plane"), "{error}");
    }

    #[test]
    fn a_frame_sized_color_type_is_probed_for_its_own_depth() {
        // The planes the frame is built with are the ones the format names, and
        // a colour file's alpha plane is the size of the image.
        let info = sized_info("a.heic", PixelFormat::Yuv420P10, ColorType::Rgba16, 5, 3);
        assert_eq!(
            plane_sizes(&info).unwrap(),
            [5 * 3 * 2, 3 * 2 * 2, 3 * 2 * 2]
        );
        assert_eq!(alpha_plane_size(&info).unwrap(), 5 * 3 * 2);
    }

    #[test]
    fn the_h273_codes_are_the_ones_a_file_states() {
        // The enum variants hold the code points the container stores, which is
        // what the frame properties are written from.
        assert_eq!(ColorPrimaries::ITU_R_BT_709_5.code(), 1);
        assert_eq!(ColorPrimaries::ITU_R_BT_2020_2_and_2100_0.code(), 9);
        assert_eq!(TransferCharacteristics::IEC_61966_2_1.code(), 13);
        assert_eq!(TransferCharacteristics::SMPTE_ST_428_1.code(), 17);
        assert_eq!(MatrixCoefficients::RGB_GBR.code(), 0);
        assert_eq!(MatrixCoefficients::ITU_R_BT_601_6.code(), 6);
        assert_eq!(MatrixCoefficients::ICtCp.code(), 14);
        // A file that states "unspecified" states nothing this plugin writes.
        assert_eq!(ColorPrimaries::Unspecified.code(), UNSPECIFIED);
        assert_eq!(MatrixCoefficients::Unspecified.code(), UNSPECIFIED);
        assert_eq!(TransferCharacteristics::Unknown.code(), UNSPECIFIED);
    }

    /// The colours libheif reports, and the format each one is handed out as.
    ///
    /// An r,g,b container is the one this arm was added for: the samples are
    /// already one per channel, and the planar `C444` spelling libheif hands
    /// over is the layout a frame is written from, so nothing is converted.
    #[test]
    fn a_colorspace_is_handed_out_as_the_format_its_samples_are() {
        let header = |color_space, depth, color_type| HeifHeader {
            width: 1,
            height: 1,
            color_space,
            depth,
            color_type,
            has_icc_profile: false,
            icc_profile: None,
            cicp: None,
        };
        // r,g,b, with and without an alpha channel. The format is the three
        // channels either way: alpha is a plane of its own and a clip of its
        // own, not part of this format.
        assert_eq!(
            header(ColorSpace::Rgb(RgbChroma::C444), 8, ColorType::Rgb8).format(),
            Some(PixelFormat::Rgb8)
        );
        assert_eq!(
            header(ColorSpace::Rgb(RgbChroma::C444), 8, ColorType::Rgba8).format(),
            Some(PixelFormat::Rgb8)
        );
        assert_eq!(
            header(ColorSpace::Rgb(RgbChroma::C444), 16, ColorType::Rgb16).format(),
            Some(PixelFormat::Rgb16)
        );
        // A spelling this module never asks for is left to the `image`
        // decoder rather than described as something it would have to take
        // apart: the interleaved variant is not the layout it requests.
        assert_eq!(
            header(ColorSpace::Rgb(RgbChroma::Rgb), 8, ColorType::Rgb8).format(),
            None
        );
        // And the arrangements that were already handled still are.
        assert_eq!(
            header(ColorSpace::Monochrome, 8, ColorType::L8).format(),
            Some(PixelFormat::Gray8)
        );
        assert_eq!(
            header(ColorSpace::Monochrome, 10, ColorType::L16).format(),
            Some(PixelFormat::Gray10)
        );
    }

    /// Whether one plane of a frame agrees with the same plane decoded the
    /// other way, byte for byte.
    ///
    /// The two decoders hand the same picture out in different layouts --
    /// libheif's planes are the frame's own, and the `image` decoder's are
    /// interleaved where the format is not -- so this compares the samples of
    /// each plane rather than the buffers.
    fn plane_of(decoded: &DecodedImage, plane: usize) -> Vec<u8> {
        match &decoded.pixels {
            Pixels::Planar { planes, .. } => planes[plane].clone(),
            // An interleaved buffer carries the channels of its own color
            // type, which is not always the format's: the `image` decoder
            // hands an r,g,b picture over as r,g,b,a and the frame is written
            // from the three channels the format names.
            Pixels::Interleaved {
                color_type, buffer, ..
            } => {
                let channels = color_type.channels();
                buffer[plane..].iter().step_by(channels).copied().collect()
            }
            _ => Vec::new(),
        }
    }

    /// An r,g,b container is read here, and every one of them still decodes.
    ///
    /// Every r,g,b fixture is checked, because the arrangement this module had
    /// to be taught is the one all of them take: libheif fills the red, green
    /// and blue channels for these and leaves the luma channel empty, which is
    /// what made this path fail before. The alpha channel is a picture of its
    /// own and is decoded as one.
    ///
    /// The pictures themselves are pinned byte for byte elsewhere: by
    /// `tests/readalpha.vpy`, which checks these fixtures against the values
    /// they hold, and by `target/bench/hash-frames.py`, whose baseline is every
    /// plane of every fixture. The `image` decoder this used to compare against
    /// is gone from the tree, and so is the comparison.
    #[test]
    fn every_rgb_container_still_decodes_here() {
        for name in [
            // The one container with a rotation is checked by the test below,
            // because the transform is applied where this reader applies it and
            // that is not a difference in the picture.
            "cicp-rgb8.avif",
            "alpha-rgba8.avif",
            "animation.avif",
        ] {
            let path = std::path::PathBuf::from("tests/fixtures").join(name);
            let info = describe(&path, true).unwrap_or_else(|| panic!("{name} is described"));
            assert!(
                matches!(info.format.color_family(), ColorFamily::RGB),
                "{name} is r,g,b: {:?}",
                info.format
            );
            let ours = decode(&info, crate::decoder::Demand::ALL)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(
                (ours.width, ours.height),
                (info.width, info.height),
                "{name}"
            );
            for plane in 0..info.format.plane_count() {
                assert_eq!(
                    plane_of(&ours, plane).len(),
                    info.format
                        .plane_bytes(plane, info.width as usize, info.height as usize),
                    "{name} plane {plane}"
                );
            }
        }
    }

    /// An r,g,b container that states a rotation is handed out the way a jpeg
    /// xl is: the decoder applies the container's own transform as it decodes,
    /// so the size read here is the rotated one and nothing turns the frame
    /// again.
    ///
    /// This is the one place the two decoders legitimately disagree, and it is
    /// why the test above leaves this fixture out. Our own avif walk reports
    /// the *stored* size and a transform derived from `irot`, because `dav1d`
    /// hands the item over as it is coded; libheif does the opposite. Both
    /// reach the same frame, by turning it in a different place.
    #[test]
    fn a_rotated_rgb_container_arrives_already_turned() {
        let path = Path::new("tests/fixtures/orientation-avif-rgb-irot-1.avif");
        let info = describe(path, true).expect("libheif describes it");
        assert_eq!(info.format, PixelFormat::Rgb8);
        // `irot` one is a quarter turn, which swaps the sides; libheif has
        // already done it, so the frame is built at this size.
        assert_eq!((info.width, info.height), (3, 4));
        assert_eq!(info.transform, Transform::IDENTITY);
        // Asking for the stored picture is what undoes it, the same shape
        // [`crate::formats::jxl`] has for the other decoder that applies a
        // file's orientation itself.
        let stored = describe(path, false).expect("libheif describes it");
        assert_ne!(stored.transform, Transform::IDENTITY);
    }
}
