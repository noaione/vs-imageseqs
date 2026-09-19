//! avif containers, read from their own boxes and decoded with dav1d.
//!
//! The `image` avif decoder decodes the whole picture and the alpha item beside
//! it when it is created, which is how it reports a size, so probing a file
//! through it costs a full decode that the frame request then repeats. Every
//! fact the probe records is in the container instead: `ispe` holds the size of
//! the primary item, `av1C` its bit depth, its chroma subsampling and whether it
//! is monochrome, `colr` whether an ICC profile is attached and what colour it
//! states, and `auxC` whether an auxiliary item holds alpha.
//!
//! The samples themselves are yuv, and `image` converts every file to r,g,b as
//! it decodes it: for a 4:2:0 page that is a bilinear upsample of two thirds of
//! the samples, forty percent more memory in the frame, and a conversion the
//! graph consuming the frames usually undoes again. `dav1d` hands the planes
//! over as they are coded, so a file whose samples the container describes is
//! handed out as the yuv format they are in; see
//! `docs/improvements/12-heif-avif-yuv-output.md`.
//!
//! A file that states no matrix the frame properties can name keeps the r,g,b
//! the `image` decoder produces for it. [`image_info`] still describes such a
//! file, and [`handles`] then declines it, so the two readers of the boxes have
//! to agree about what the container says.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    ops::Range,
    path::Path,
    time::Instant,
};

use dav1d::{PixelLayout, PlanarImageComponent};
use image::{ColorType, metadata::Orientation};
use vapoursynth4_rs::ColorFamily;

use crate::{
    color::{Cicp, UNSPECIFIED},
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    pixel::{PixelFormat, Transform},
};

/// File extensions that hold an avif container.
const AVIF_EXTENSIONS: [&str; 1] = ["avif"];

/// Size limit for the leading boxes of an avif. The metadata of a real file is
/// orders of magnitude smaller, and a file past it is left to the decoder.
const HEADER_LIMIT: usize = 1024 * 1024;

/// Bytes of the primary item payload read to find its sequence header.
const SEQUENCE_HEADER_LIMIT: usize = 4096;

/// `auxC` types that name the auxiliary item holding alpha: the one the avif
/// specification requires, and the older hevc type that heif writers used.
const ALPHA_AUX_TYPES: [&[u8]; 2] = [
    b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
    b"urn:mpeg:hevc:2015:auxid:1",
];

/// Whether this module decodes `info`.
///
/// Only a file the probe described as yuv is decoded here; a file whose samples
/// the container does not name keeps the r,g,b the `image` decoder produces.
pub fn handles(info: &ImageInfo) -> bool {
    has_avif_extension(&info.path) && info.format.color_family() == ColorFamily::YUV
}

/// What the container of an avif states about its image, when this module can
/// describe the file from it.
///
/// `None` means "let the `image` decoder describe this file": a container this
/// module will not walk, or one whose primary item is not the shape a probe can
/// describe. A file this answers with an r,g,b format is described but not
/// decoded here, and its format correction is read from the same boxes by
/// [`output_format`].
pub fn image_info(path: &Path) -> Option<ImageInfo> {
    let mut file = File::open(path).ok()?;
    let boxes = leading_boxes(&mut file)?;
    if !has_avif_brand(&boxes) {
        return None;
    }
    let meta = Meta::read(&boxes)?;
    let header = AvifHeader::read(&meta.primary_properties()?)?;
    let (width, height) = (header.width, header.height);
    if width == 0 || height == 0 {
        return None;
    }
    let has_alpha = meta.has_alpha();

    // The colour the frames are tagged with is the one the `colr` box states,
    // and the sequence header of the coded image when the item has no box:
    // `avifenc` writes one, and a lot of other writers state the codes in the
    // bitstream alone.
    let cicp = header.cicp.or_else(|| {
        let payload = read_range(&mut file, meta.primary_data(SEQUENCE_HEADER_LIMIT).ok()?).ok()?;
        sequence_header_cicp(&payload)
    });

    if header.monochrome {
        // A monochrome item holds one plane, and the `image` decoder converts it
        // into the four channels it reports. The probe records the gray format
        // the samples map to, at the depth the `av1C` box states, and leaves the
        // decode to `image`, which is where that path has always been; see
        // `docs/improvements/05-monochrome-heif.md`.
        return Some(ImageInfo {
            path: path.to_path_buf(),
            width,
            height,
            color_type: header.decoded_color_type(),
            original_color_type: header.file_color_type(has_alpha).into(),
            has_icc_profile: header.has_icc_profile,
            cicp,
            // A single plane has no chroma to place.
            chroma_location: None,
            orientation: Orientation::NoTransforms,
            transform: Transform::IDENTITY,
            format: PixelFormat::from_color_type(header.file_color_type(has_alpha))?
                .at_depth(header.depth.into()),
        });
    }

    // The samples are yuv when the container states a matrix the frame
    // properties can name. A file that states none, states "unspecified", or
    // states the identity - whose samples are already r,g,b and have no planar
    // yuv format to be handed out as - keeps the r,g,b the `image` decoder
    // produces, and is still described here, because the colour it states is
    // written into the frame's properties either way.
    let yuv = cicp
        .filter(|cicp| usable_matrix(cicp.matrix))
        .and_then(|_| yuv_format(header.chroma, header.depth));
    let (format, color_type) = match yuv {
        Some(format) => (format, header.colour_color_type(has_alpha)),
        // The color type the `image` decoder reports, which is what the frame
        // request of such a file is checked against, and the depth the `av1C`
        // box states, which is what its samples are handed out at.
        None => (
            PixelFormat::from_color_type(header.decoded_color_type())?
                .at_depth(header.depth.into()),
            header.decoded_color_type(),
        ),
    };
    Some(ImageInfo {
        path: path.to_path_buf(),
        width,
        height,
        color_type,
        original_color_type: color_type.into(),
        has_icc_profile: header.has_icc_profile,
        cicp,
        chroma_location: crate::color::chroma_location(header.chroma_position),
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format,
    })
}

/// Format a file this module describes is handed out as, for a file whose
/// decode is left to the `image` decoder.
///
/// This reads the same boxes as [`image_info`] but without asking for the brand,
/// because the files it matters for are exactly the ones [`image_info`] declines:
/// a monochrome item in a container whose major brand is not `avif` is described
/// by whichever decoder `image` picks, which reports the four channels it always
/// reports, and its single plane is still the format it should be handed out as.
/// For such a bitstream the conversion `image` performs has no chroma to mix in,
/// so every channel of the decoded picture holds the same sample.
pub fn output_format(path: &Path, color_type: ColorType) -> Option<PixelFormat> {
    // A decoder that already reports one of the monochrome color types maps to
    // the gray format on its own, and a file that is not one of these is handed
    // out as the format its probe describes.
    let decoded = PixelFormat::from_color_type(color_type)?;
    if decoded.color_family() != ColorFamily::RGB || !has_avif_extension(path) {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let boxes = leading_boxes(&mut file)?;
    let meta = Meta::read(&boxes)?;
    let header = AvifHeader::read(&meta.primary_properties()?)?;
    header
        .monochrome
        .then(|| PixelFormat::from_color_type(header.file_color_type(meta.has_alpha())))
        .flatten()
        .map(|format| format.at_depth(header.depth.into()))
}

/// Decodes one avif item into the planes of the format its probe recorded.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = Instant::now();
    let mut file =
        File::open(&info.path).map_err(|error| image_error("open", &info.path, error))?;
    let boxes = leading_boxes(&mut file)
        .ok_or_else(|| image_error("read the boxes of", &info.path, "no leading boxes"))?;
    let open = open_started.elapsed();

    let metadata_started = Instant::now();
    let meta = Meta::read(&boxes)
        .ok_or_else(|| image_error("read the boxes of", &info.path, "malformed item boxes"))?;
    if meta.grid {
        return Err(image_error(
            "read the boxes of",
            &info.path,
            "its primary item is a grid of tiles, which this reader does not join",
        ));
    }
    let header = AvifHeader::read(&meta.primary_properties().ok_or_else(|| {
        image_error(
            "read the boxes of",
            &info.path,
            "no properties for the primary item",
        )
    })?)
    .ok_or_else(|| image_error("read the boxes of", &info.path, "no coding record"))?;
    let (width, height) = (header.width, header.height);
    if (width, height) != (info.width, info.height)
        || yuv_format(header.chroma, header.depth) != Some(info.format)
    {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {}x{} {}, now {width}x{height} {:?})",
            info.path.display(),
            info.width,
            info.height,
            info.format.name(),
            yuv_format(header.chroma, header.depth).map(PixelFormat::name),
        )));
    }
    let coded = read_range(&mut file, meta.primary_data(usize::MAX)?)
        .map_err(|error| image_error("read", &info.path, error))?;
    let expects_alpha = crate::pixel::alpha_channel(info.color_type).is_some();
    let alpha_coded = if expects_alpha {
        Some(
            meta.alpha_data(usize::MAX)?
                .ok_or_else(|| {
                    image_error(
                        "read the alpha item of",
                        &info.path,
                        "the container holds none",
                    )
                })
                .and_then(|range| {
                    read_range(&mut file, range)
                        .map_err(|error| image_error("read", &info.path, error))
                })?,
        )
    } else {
        None
    };
    let metadata = metadata_started.elapsed();

    let buffer_started = Instant::now();
    let mut planes = Vec::with_capacity(info.format.plane_count());
    for plane in 0..info.format.plane_count() {
        planes.push(vec![0; plane_size(info, plane)?.2]);
    }
    let mut alpha = if expects_alpha {
        Some(vec![0; alpha_size(info)?])
    } else {
        None
    };
    let buffer = buffer_started.elapsed();

    let read_started = Instant::now();
    let picture = decode_item(&coded, info)?;
    check_picture(&picture, info, info.format.plane_count())?;
    for (index, target) in planes.iter_mut().enumerate() {
        let component = [
            PlanarImageComponent::Y,
            PlanarImageComponent::U,
            PlanarImageComponent::V,
        ][index.min(2)];
        copy_plane(&picture, component, info, index, target)?;
    }
    if let (Some(coded), Some(target)) = (alpha_coded, alpha.as_mut()) {
        let plane = decode_item(&coded, info)?;
        let depth = plane
            .bits_per_component()
            .map(|bits| bits.0)
            .unwrap_or_default();
        if plane.pixel_layout() != PixelLayout::I400
            || depth != nominal_depth(info.format.alpha_format())
        {
            return Err(image_error(
                "decode the alpha item of",
                &info.path,
                "it is not the depth of the image it belongs to",
            ));
        }
        copy_alpha(&plane, info, target)?;
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

/// Decodes one coded av1 item into its picture.
fn decode_item(coded: &[u8], info: &ImageInfo) -> Result<dav1d::Picture> {
    let mut decoder = dav1d::Decoder::new().map_err(|error| decode_error(&info.path, error))?;
    if let Err(error) = decoder.send_data(coded.to_vec(), None, None, None) {
        return Err(decode_error(&info.path, error));
    }
    loop {
        match decoder.get_picture() {
            Err(dav1d::Error::Again) => match decoder.send_pending_data() {
                Ok(()) | Err(dav1d::Error::Again) => {}
                Err(error) => return Err(decode_error(&info.path, error)),
            },
            Ok(picture) => return Ok(picture),
            Err(error) => return Err(decode_error(&info.path, error)),
        }
    }
}

/// Checks the coded picture against what the probe recorded.
fn check_picture(picture: &dav1d::Picture, info: &ImageInfo, planes: usize) -> Result<()> {
    let depth = picture.bits_per_component().map(|bits| bits.0);
    let layout = picture.pixel_layout();
    let wanted = match planes {
        1 => PixelLayout::I400,
        _ => expected_layout(info.format),
    };
    let ok = (picture.width(), picture.height()) == (info.width, info.height)
        && layout == wanted
        && depth == Some(nominal_depth(info.format));
    if !ok {
        return Err(ImgSeqError::new(format!(
            "image '{}' is not the {} its container describes (coded as {layout:?} {depth:?} bit {}x{})",
            info.path.display(),
            info.format.name(),
            picture.width(),
            picture.height(),
        )));
    }
    Ok(())
}

/// Row bytes of one plane of the frame, and how many bytes the plane holds.
fn plane_size(info: &ImageInfo, plane: usize) -> Result<(usize, usize, usize)> {
    let width = usize::try_from(info.width)
        .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
    let height = usize::try_from(info.height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    let (plane_width, plane_height) = info.format.plane_dimensions(plane, width, height);
    let row = plane_width
        .checked_mul(info.format.bytes_per_sample())
        .ok_or_else(|| ImgSeqError::new("image row is too large"))?;
    let size = row
        .checked_mul(plane_height)
        .ok_or_else(|| ImgSeqError::new("image is too large"))?;
    Ok((row, plane_height, size))
}

/// Row bytes, height and size of the alpha plane, which is never subsampled.
fn alpha_plane_size(info: &ImageInfo) -> Result<(usize, usize, usize)> {
    let width = usize::try_from(info.width)
        .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
    let height = usize::try_from(info.height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    let row = width
        .checked_mul(info.format.alpha_format().bytes_per_sample())
        .ok_or_else(|| ImgSeqError::new("image row is too large"))?;
    let size = row
        .checked_mul(height)
        .ok_or_else(|| ImgSeqError::new("image is too large"))?;
    Ok((row, height, size))
}

/// Size of the alpha plane, which is never subsampled.
fn alpha_size(info: &ImageInfo) -> Result<usize> {
    alpha_plane_size(info).map(|(_, _, size)| size)
}

/// Copies one plane of a decoded picture into the tightly packed buffer of the
/// frame, dropping the stride padding the decoder leaves between rows.
fn copy_plane(
    picture: &dav1d::Picture,
    component: PlanarImageComponent,
    info: &ImageInfo,
    plane: usize,
    target: &mut [u8],
) -> Result<()> {
    let (row, height, _) = plane_size(info, plane)?;
    let stride = usize::try_from(picture.stride(component))
        .map_err(|_| ImgSeqError::new("the decoded stride does not fit in memory"))?;
    let data = picture.plane(component);
    copy_rows(
        target,
        row,
        stride,
        height,
        data.as_ref(),
        if plane == 0 { "luma" } else { "chroma" },
        info,
    )
}

/// Copies the alpha item into the buffer the alpha clip is written from.
fn copy_alpha(picture: &dav1d::Picture, info: &ImageInfo, target: &mut [u8]) -> Result<()> {
    let (row, height, _) = alpha_plane_size(info)?;
    let stride = usize::try_from(picture.stride(PlanarImageComponent::Y))
        .map_err(|_| ImgSeqError::new("the decoded stride does not fit in memory"))?;
    let data = picture.plane(PlanarImageComponent::Y);
    copy_rows(target, row, stride, height, data.as_ref(), "alpha", info)
}

/// Copies the rows of one decoded plane into a tightly packed buffer. The
/// decoder lays a plane out with a stride of its own, which is what the frame
/// format does not have room for.
fn copy_rows(
    target: &mut [u8],
    row: usize,
    stride: usize,
    height: usize,
    data: &[u8],
    name: &str,
    info: &ImageInfo,
) -> Result<()> {
    if target.len() != row * height {
        return Err(ImgSeqError::new(
            "the plane buffer is not the size of its plane",
        ));
    }
    if stride < row {
        return Err(ImgSeqError::new(format!(
            "the decoded {name} plane of '{}' has a stride of {stride} bytes, less than the {row} the frame needs",
            info.path.display(),
        )));
    }
    for (index, destination) in target.chunks_exact_mut(row).enumerate() {
        let start = index * stride;
        let source = data.get(start..start + row).ok_or_else(|| {
            ImgSeqError::new(format!(
                "the decoded {name} plane of '{}' is shorter than its own rows",
                info.path.display(),
            ))
        })?;
        destination.copy_from_slice(source);
    }
    Ok(())
}

/// The layout dav1d reports for a format this module hands out.
const fn expected_layout(format: PixelFormat) -> PixelLayout {
    match format {
        PixelFormat::Yuv422P8 | PixelFormat::Yuv422P10 => PixelLayout::I422,
        PixelFormat::Yuv444P8 | PixelFormat::Yuv444P10 | PixelFormat::Yuv444P12 => {
            PixelLayout::I444
        }
        _ => PixelLayout::I420,
    }
}

/// The number of bits a format names.
const fn nominal_depth(format: PixelFormat) -> usize {
    match format {
        PixelFormat::Gray10
        | PixelFormat::Yuv420P10
        | PixelFormat::Yuv422P10
        | PixelFormat::Yuv444P10 => 10,
        PixelFormat::Gray12 | PixelFormat::Yuv444P12 => 12,
        PixelFormat::Gray16 | PixelFormat::Rgb16 => 16,
        _ => 8,
    }
}

/// The yuv format a subsampled layout of `depth` bits is handed out as.
///
/// A depth with no format of its own - twelve bit 4:2:0, say - keeps the r,g,b
/// the `image` decoder produces, which is why this answers `None` rather than
/// rounding the samples into a format that would misstate them.
const fn yuv_format(chroma: u8, depth: u8) -> Option<PixelFormat> {
    match (chroma, depth) {
        (2, 8) => Some(PixelFormat::Yuv420P8),
        (1, 8) => Some(PixelFormat::Yuv422P8),
        (0, 8) => Some(PixelFormat::Yuv444P8),
        (2, 10) => Some(PixelFormat::Yuv420P10),
        (1, 10) => Some(PixelFormat::Yuv422P10),
        (0, 10) => Some(PixelFormat::Yuv444P10),
        (0, 12) => Some(PixelFormat::Yuv444P12),
        _ => None,
    }
}

/// Whether `_Matrix` can name the coefficients a file states.
///
/// The identity is a statement about samples that are already r,g,b, and
/// VapourSynth has no planar format for those, so a file that states it - like
/// one that states nothing, or states "unspecified" - keeps the r,g,b the
/// `image` decoder produces.
const fn usable_matrix(matrix: u8) -> bool {
    matrix != 0 && matrix != UNSPECIFIED
}

/// What the item properties of the primary item state about it.
struct AvifHeader {
    width: u32,
    height: u32,
    /// Bits of one sample of the primary item.
    depth: u8,
    /// Whether the primary item is one sample per pixel.
    monochrome: bool,
    /// The subsampled layout of the primary item, as the two `av1C` flags name
    /// it: 0 is 4:4:4, 1 is 4:2:2 and 2 is 4:2:0.
    chroma: u8,
    /// The two bit chroma sample position of the primary item, which is stated
    /// for a subsampled picture and is zero for a picture that names none.
    chroma_position: u8,
    has_icc_profile: bool,
    /// The colour description a `colr` box of the primary item states, when it
    /// holds one.
    cicp: Option<Cicp>,
}

impl AvifHeader {
    /// Reads the properties of the primary item.
    fn read(properties: &[Property<'_>]) -> Option<Self> {
        let mut size = None;
        let mut depth = None;
        let mut monochrome = false;
        let mut subsampling = None;
        let mut chroma_position = 0;
        let mut has_icc_profile = false;
        let mut cicp = None;
        for property in properties {
            match property.kind {
                b"ispe" => {
                    let width = u32::from_be_bytes(property.payload.get(4..8)?.try_into().ok()?);
                    let height = u32::from_be_bytes(property.payload.get(8..12)?.try_into().ok()?);
                    size = Some((width, height));
                }
                b"av1C" => {
                    let flags = *property.payload.get(2)?;
                    depth = Some(av1_bit_depth(flags));
                    monochrome = flags & MONO_CHROME != 0;
                    // The two subsampling flags are one bit each, and their sum
                    // is the layout this module names a format by: neither is
                    // 4:4:4, the horizontal one alone is 4:2:2, and both are
                    // 4:2:0.
                    subsampling = Some(((flags >> 3) & 1) + ((flags >> 2) & 1));
                    chroma_position = flags & 0x03;
                }
                b"colr" => {
                    let kind = property.payload.get(..4)?;
                    has_icc_profile |= matches!(kind, b"prof" | b"rICC");
                    // A file may hold both an opaque profile and the codes, and
                    // the codes are what the frame properties are written from;
                    // an ICC profile on its own is `ImgSeqHasICC`.
                    if let Some(stated) = nclx_cicp(property.payload) {
                        cicp = Some(stated);
                    }
                }
                _ => {}
            }
        }
        let (width, height) = size?;
        Some(Self {
            width,
            height,
            depth: depth?,
            monochrome,
            chroma: subsampling?,
            chroma_position,
            has_icc_profile,
            cicp,
        })
    }

    /// The color type the `image` avif decoder reports for this file, whose
    /// decoder hands back four channels whatever the bitstream holds. That is
    /// what a file this module leaves to it is probed as.
    const fn decoded_color_type(&self) -> ColorType {
        if self.depth > 8 {
            ColorType::Rgba16
        } else {
            ColorType::Rgba8
        }
    }

    /// The color type of the samples the file holds: three channels for a colour
    /// image, and a fourth when an alpha item exists. A monochrome image holds
    /// the one sample the container says it does, beside the alpha item when
    /// there is one.
    const fn file_color_type(&self, has_alpha: bool) -> ColorType {
        match (self.monochrome, self.depth > 8, has_alpha) {
            (true, false, false) => ColorType::L8,
            (true, false, true) => ColorType::La8,
            (true, true, false) => ColorType::L16,
            (true, true, true) => ColorType::La16,
            (false, false, false) => ColorType::Rgb8,
            (false, false, true) => ColorType::Rgba8,
            (false, true, false) => ColorType::Rgb16,
            (false, true, true) => ColorType::Rgba16,
        }
    }

    /// Color type a colour image of this depth is probed as, which is what the
    /// alpha item beside it decides the last channel of.
    const fn colour_color_type(&self, has_alpha: bool) -> ColorType {
        match (self.depth > 8, has_alpha) {
            (false, false) => ColorType::Rgb8,
            (false, true) => ColorType::Rgba8,
            (true, false) => ColorType::Rgb16,
            (true, true) => ColorType::Rgba16,
        }
    }
}

/// The `av1C` flag that says the primary item holds one sample per pixel.
const MONO_CHROME: u8 = 0x10;

/// Bit depth the flags byte of an av1 coding record describes.
fn av1_bit_depth(flags: u8) -> u8 {
    const HIGH_BIT_DEPTH: u8 = 0x40;
    const TWELVE_BIT: u8 = 0x20;
    match (flags & HIGH_BIT_DEPTH, flags & TWELVE_BIT) {
        (0, _) => 8,
        (_, 0) => 10,
        _ => 12,
    }
}

/// A property of an item, as one box of the property list.
struct Property<'a> {
    kind: &'a [u8; 4],
    payload: &'a [u8],
}

/// The item boxes of one avif.
///
/// One property of the item property list: the kind of its box, and the range
/// its payload occupies in the file.
type Listed = ([u8; 4], Range<usize>);

/// One box of a container, as the walk over its payload finds them: the kind,
/// the range its payload occupies in the file, and that payload.
type ChildBox<'a> = (&'a [u8; 4], Range<usize>, &'a [u8]);

/// The property numbers of one item, as `ipma` associates them.
type Associations = Vec<(u32, Vec<u32>)>;

/// The extents of every item, as `iloc` states them: the item, its construction
/// method, and the offset and length of each of its extents.
type Extents = Vec<(u32, u8, Vec<(usize, usize)>)>;

/// The references of a container, as `iref` states them: the kind of the
/// reference, the item it is from, and the items it is to.
type References = Vec<([u8; 4], u32, Vec<u32>)>;

/// Every range in here is an offset into the leading boxes buffer, which is a
/// prefix of the file, so the same range reads from either.
struct Meta {
    /// The leading boxes this was read from.
    boxes: Vec<u8>,
    /// The item property list, in the order it was written: its kind and the
    /// range of its payload.
    properties: Vec<Listed>,
    /// The property indices of every item, as `ipma` associates them.
    associations: Associations,
    /// The primary item, as `pitm` names it.
    primary: u32,
    /// The extents of every item, as `iloc` names them, with the construction
    /// method that says what their offsets are relative to.
    extents: Extents,
    /// The `idat` payload, which is where an extent of construction method one
    /// is read from.
    idat: Option<Range<usize>>,
    /// The item an `auxl` reference points at the primary item from.
    aux: Option<u32>,
    /// Whether the primary item is a grid of tiles, whose payload describes the
    /// tiling rather than holding a coded picture.
    grid: bool,
    /// The properties of the auxiliary item, which have to name alpha.
    aux_properties: Vec<Listed>,
}

impl Meta {
    /// Reads the item boxes of a file whose leading boxes are `boxes`.
    fn read(boxes: &[u8]) -> Option<Self> {
        let meta = child(boxes, b"meta", 0)?.1;
        let meta = meta.get(4..)?;
        let base = offset_of(boxes, meta)?;
        // The property list hangs under the item property container, which is
        // what the `ipco` payload holds, and the associations of every item are
        // the `ipma` box beside it.
        let iprp = child(meta, b"iprp", base)?.1;
        let iprp_base = offset_of(boxes, iprp)?;
        let ipco = child(iprp, b"ipco", iprp_base)?.1;
        let properties: Vec<Listed> = child_boxes(ipco, offset_of(boxes, ipco)?)?
            .into_iter()
            .map(|(kind, range, _)| (*kind, range))
            .collect();
        let idat = child(meta, b"idat", base).and_then(|(_, payload)| {
            let start = offset_of(boxes, payload)?;
            (start + payload.len() <= boxes.len()).then_some(start..start + payload.len())
        });
        let pitm = child(meta, b"pitm", base)?.1;
        let primary = match pitm.first()? {
            0 => u32::from(u16::from_be_bytes(pitm.get(4..6)?.try_into().ok()?)),
            _ => u32::from_be_bytes(pitm.get(4..8)?.try_into().ok()?),
        };
        let associations = read_ipma(child(iprp, b"ipma", iprp_base)?.1)?;
        let extents = read_iloc(child(meta, b"iloc", base)?.1)?;
        // `iref` is only written by a file that has something to reference, so a
        // file without it is a file without an alpha item and without tiles.
        let references = match child(meta, b"iref", base) {
            Some((_, payload)) => read_references(payload)?,
            None => Vec::new(),
        };
        let aux = references
            .iter()
            .find(|(kind, _, to)| *kind == *b"auxl" && to.contains(&primary))
            .map(|(_, from, _)| *from);
        // A tiled avif is one item that references the tiles it is made of, and
        // its own payload describes the tiling rather than holding a picture.
        let grid = references
            .iter()
            .any(|(kind, from, _)| *kind == *b"dimg" && *from == primary);
        let aux_properties = match aux {
            Some(item) => associations
                .iter()
                .find(|(id, _)| *id == item)
                .map(|(_, indices)| {
                    indices
                        .iter()
                        .filter_map(|index| property(&properties, *index).cloned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        Some(Self {
            boxes: boxes.to_vec(),
            properties,
            associations,
            primary,
            extents,
            idat,
            aux,
            grid,
            aux_properties,
        })
    }

    /// The properties of the primary item.
    fn primary_properties(&self) -> Option<Vec<Property<'_>>> {
        let (_, indices) = self
            .associations
            .iter()
            .find(|(id, _)| *id == self.primary)?;
        Some(
            indices
                .iter()
                .filter_map(|index| property(&self.properties, *index))
                .map(|(kind, range)| Property {
                    kind,
                    payload: &self.boxes[range.clone()],
                })
                .collect(),
        )
    }

    /// Whether an auxiliary item that names alpha belongs to the primary item.
    fn has_alpha(&self) -> bool {
        self.aux.is_some()
            && self.aux_properties.iter().any(|(kind, range)| {
                *kind == *b"auxC"
                    && ALPHA_AUX_TYPES
                        .iter()
                        .any(|aux_type| find_bytes(&self.boxes[range.clone()], aux_type).is_some())
            })
    }

    /// The extents of one item, and what their offsets are relative to.
    fn extents_of(&self, item: u32) -> Option<(u8, &[(usize, usize)])> {
        self.extents
            .iter()
            .find(|(id, _, _)| *id == item)
            .map(|(_, method, ranges)| (*method, ranges.as_slice()))
    }

    /// The bytes of the primary item's payload, up to `limit` of them.
    fn primary_data(&self, limit: usize) -> Result<Range<usize>> {
        let (method, ranges) = self
            .extents_of(self.primary)
            .ok_or_else(|| ImgSeqError::new("the container does not locate its primary item"))?;
        self.data_range(method, ranges, limit)
    }

    /// The bytes of the alpha item's payload, when the container holds one.
    fn alpha_data(&self, limit: usize) -> Result<Option<Range<usize>>> {
        let Some(aux) = self.aux.filter(|_| self.has_alpha()) else {
            return Ok(None);
        };
        let (method, ranges) = self
            .extents_of(aux)
            .ok_or_else(|| ImgSeqError::new("the container does not locate its alpha item"))?;
        self.data_range(method, ranges, limit).map(Some)
    }

    /// One contiguous range of an item's bytes.
    ///
    /// An extent of an item written into `idat` is an offset into that box, and
    /// every other one is an offset into the file. Only a payload one extent
    /// holds is read here: a file that splits its item over several extents is
    /// left to the `image` decoder, which joins them.
    fn data_range(
        &self,
        method: u8,
        ranges: &[(usize, usize)],
        limit: usize,
    ) -> Result<Range<usize>> {
        let Some((offset, length)) = ranges.first().copied() else {
            return Err(ImgSeqError::new("the container lists no data for an item"));
        };
        if ranges.len() > 1 {
            return Err(ImgSeqError::new(
                "the item is split over several extents, which this reader does not join",
            ));
        }
        let base = match method {
            0 => 0,
            1 => {
                self.idat
                    .as_ref()
                    .ok_or_else(|| ImgSeqError::new("the container has no idat box"))?
                    .start
            }
            _ => {
                return Err(ImgSeqError::new(
                    "the item uses a construction method this reader does not read",
                ));
            }
        };
        let length = length.min(limit);
        base.checked_add(offset)
            .map(|start| start..start + length)
            .ok_or_else(|| ImgSeqError::new("the item is past the end of the file"))
    }
}

/// A box of `boxes` with the given kind, as its payload and the offset of that
/// payload in the file.
fn child<'a>(boxes: &'a [u8], kind: &[u8; 4], base: usize) -> Option<(&'a [u8; 4], &'a [u8])> {
    child_boxes(boxes, base)?
        .into_iter()
        .find(|(found, _, _)| **found == *kind)
        .map(|(kind, _, payload)| (kind, payload))
}

/// Offset of `inner` inside `outer`, which must be a slice of it.
fn offset_of(outer: &[u8], inner: &[u8]) -> Option<usize> {
    let start = (inner.as_ptr() as usize).checked_sub(outer.as_ptr() as usize)?;
    (start + inner.len() <= outer.len()).then_some(start)
}

/// The boxes one container payload holds, in order: its kind, the range its
/// payload occupies in the file, and that payload.
///
/// A box whose size is zero runs to the end of its container, and one whose size
/// is one carries a 64 bit size after its kind. A malformed box ends the walk,
/// which leaves its file to the decoder rather than guessing.
fn child_boxes(payload: &[u8], base: usize) -> Option<Vec<ChildBox<'_>>> {
    let mut boxes = Vec::new();
    let mut at = 0;
    while at + 8 <= payload.len() {
        let size = u64::from(u32::from_be_bytes(payload[at..at + 4].try_into().ok()?));
        let kind: &[u8; 4] = payload[at + 4..at + 8].try_into().ok()?;
        let (header, size) = if size == 1 {
            let extended = u64::from_be_bytes(payload.get(at + 8..at + 16)?.try_into().ok()?);
            (16, extended)
        } else if size == 0 {
            (8, u64::try_from(payload.len() - at).ok()?)
        } else {
            (8, size)
        };
        let size = usize::try_from(size).ok()?;
        if size < header || at.checked_add(size)? > payload.len() {
            return None;
        }
        boxes.push((
            kind,
            base + at + header..base + at + size,
            &payload[at + header..at + size],
        ));
        at += size;
    }
    Some(boxes)
}

/// One property of a property list, by the number `ipma` associates it by, which
/// counts the list from one.
fn property(properties: &[Listed], index: u32) -> Option<&Listed> {
    properties.get(usize::try_from(index).ok()?.checked_sub(1)?)
}

/// The properties each item is associated with, as `ipma` states them.
fn read_ipma(payload: &[u8]) -> Option<Associations> {
    let version = *payload.first()?;
    let wide = *payload.get(3)? & 1 != 0;
    let mut at = 4;
    let count = u32::from_be_bytes(payload.get(at..at + 4)?.try_into().ok()?);
    at += 4;
    let mut entries = Vec::new();
    for _ in 0..count {
        let item = if version < 1 {
            let item = u32::from(u16::from_be_bytes(
                payload.get(at..at + 2)?.try_into().ok()?,
            ));
            at += 2;
            item
        } else {
            let item = u32::from_be_bytes(payload.get(at..at + 4)?.try_into().ok()?);
            at += 4;
            item
        };
        let associations = usize::from(*payload.get(at)?);
        at += 1;
        let mut indices = Vec::with_capacity(associations);
        for _ in 0..associations {
            if wide {
                let association = u16::from_be_bytes(payload.get(at..at + 2)?.try_into().ok()?);
                at += 2;
                indices.push(u32::from(association & 0x7fff));
            } else {
                let association = u32::from(*payload.get(at)?);
                at += 1;
                indices.push(association & 0x7f);
            }
        }
        entries.push((item, indices));
    }
    Some(entries)
}

/// The extents of every item, as `iloc` states them.
fn read_iloc(payload: &[u8]) -> Option<Extents> {
    let version = *payload.first()?;
    let sizes = *payload.get(4)?;
    let offset_size = usize::from(sizes >> 4);
    let length_size = usize::from(sizes & 0x0f);
    let sizes = *payload.get(5)?;
    let base_offset_size = usize::from(sizes >> 4);
    let index_size = if version == 0 {
        0
    } else {
        usize::from(sizes & 0x0f)
    };
    let mut at = 6;
    let count = if version < 2 {
        let count = u32::from(u16::from_be_bytes(
            payload.get(at..at + 2)?.try_into().ok()?,
        ));
        at += 2;
        count
    } else {
        let count = u32::from_be_bytes(payload.get(at..at + 4)?.try_into().ok()?);
        at += 4;
        count
    };
    let mut items = Vec::new();
    for _ in 0..count {
        let item = if version < 2 {
            let item = u32::from(u16::from_be_bytes(
                payload.get(at..at + 2)?.try_into().ok()?,
            ));
            at += 2;
            item
        } else {
            let item = u32::from_be_bytes(payload.get(at..at + 4)?.try_into().ok()?);
            at += 4;
            item
        };
        let method = if version == 0 {
            0
        } else {
            let flags = u16::from_be_bytes(payload.get(at..at + 2)?.try_into().ok()?);
            at += 2;
            u8::try_from(flags & 0x0f).ok()?
        };
        // The data reference index, which is zero for a file that holds its own
        // media data, which is every file this module reads.
        at += 2;
        let base = read_sized(payload, &mut at, base_offset_size)?;
        let extents = u32::from(u16::from_be_bytes(
            payload.get(at..at + 2)?.try_into().ok()?,
        ));
        at += 2;
        let mut ranges = Vec::new();
        for _ in 0..extents {
            if index_size > 0 {
                read_sized(payload, &mut at, index_size)?;
            }
            let offset = read_sized(payload, &mut at, offset_size)?;
            let length = read_sized(payload, &mut at, length_size)?;
            ranges.push((base + offset, length));
        }
        items.push((item, method, ranges));
    }
    Some(items)
}

/// Reads a big endian number of `size` bytes, advancing `at`.
fn read_sized(payload: &[u8], at: &mut usize, size: usize) -> Option<usize> {
    let mut value = 0usize;
    for _ in 0..size {
        value = (value << 8) | usize::from(*payload.get(*at)?);
        *at += 1;
    }
    Some(value)
}

/// The item an `auxl` reference points at the primary item from, which is the
/// item that holds its alpha, and whether the primary item is a grid of tiles.
fn read_references(payload: &[u8]) -> Option<References> {
    let version = *payload.first()?;
    let body = payload.get(4..)?;
    let mut references = Vec::new();
    for (kind, _, reference) in child_boxes(body, 0)? {
        let (from, count, mut at) = if version == 0 {
            (
                u32::from(u16::from_be_bytes(reference.get(..2)?.try_into().ok()?)),
                usize::from(u16::from_be_bytes(reference.get(2..4)?.try_into().ok()?)),
                4,
            )
        } else {
            (
                u32::from_be_bytes(reference.get(..4)?.try_into().ok()?),
                usize::try_from(u32::from_be_bytes(reference.get(4..8)?.try_into().ok()?)).ok()?,
                8,
            )
        };
        let width = if version == 0 { 2 } else { 4 };
        let mut to = Vec::with_capacity(count);
        for _ in 0..count {
            let end = at + width;
            to.push(if version == 0 {
                u32::from(u16::from_be_bytes(reference.get(at..end)?.try_into().ok()?))
            } else {
                u32::from_be_bytes(reference.get(at..end)?.try_into().ok()?)
            });
            at = end;
        }
        references.push((*kind, from, to));
    }
    Some(references)
}

/// Whether the leading boxes start with a file type that can hold av1 items.
///
/// A file that is not one of these is left to the `image` decoder, which sniffs
/// the format itself and may route it elsewhere: an `mif1` file under an `.avif`
/// name decodes through the heif hook, whose color type this module cannot
/// predict.
fn has_avif_brand(boxes: &[u8]) -> bool {
    const BRANDS: [&[u8]; 2] = [b"avif", b"avis"];
    let Some((major, _, rest)) = child_boxes(boxes, 0).and_then(|boxes| boxes.into_iter().next())
    else {
        return false;
    };
    if *major != *b"ftyp" {
        return false;
    }
    let Some((brand, compatible)) = rest.split_at_checked(8) else {
        return false;
    };
    BRANDS.contains(&&brand[..4])
        || compatible
            .as_chunks::<4>()
            .0
            .iter()
            .any(|brand| BRANDS.contains(&brand.as_slice()))
}

/// The colour description an `nclx` `colr` payload states.
///
/// The payload is the four byte type name, then the primaries, transfer and
/// matrix code points as big endian 16 bit numbers, then a flag byte whose top
/// bit says whether the samples use the full range. The other kinds of `colr`
/// payload hold an ICC profile, which states no codes at all.
fn nclx_cicp(payload: &[u8]) -> Option<Cicp> {
    const FULL_RANGE: u8 = 0x80;
    if payload.get(..4)? != b"nclx" {
        return None;
    }
    // Every code point fits in a byte, so one that does not is a file whose
    // colour this module does not state.
    let code = |at: usize| -> Option<u8> {
        let stored = u16::from_be_bytes(payload.get(at..at + 2)?.try_into().ok()?);
        u8::try_from(stored).ok()
    };
    Some(Cicp {
        primaries: code(4)?,
        transfer: code(6)?,
        matrix: code(8)?,
        full_range: (payload.get(10)? & FULL_RANGE) != 0,
    })
}

/// The colour description the sequence header of a coded av1 item states.
///
/// Every avif that states its colour has to state it here, because this is where
/// the codes of the bitstream itself are written; the `colr` box is the
/// container's second telling of the same thing, and a lot of writers leave it
/// out. Only the fields up to `color_config()` are walked, because the sequence
/// header of a still image states nothing past it that this reads.
fn sequence_header_cicp(payload: &[u8]) -> Option<Cicp> {
    let header = sequence_header_payload(payload)?;
    let mut bits = Bits::new(header);
    let seq_profile = bits.bits(3)?;
    let _still_picture = bits.bit()?;
    let reduced = bits.bit()? == 1;
    if !reduced {
        let timing = bits.bit()? == 1;
        if timing {
            bits.skip(32 + 32)?;
            if bits.bit()? == 1 {
                bits.uvlc()?;
            }
        }
        // A sequence header that describes a decoder model keeps a layout this
        // reader does not walk, and its file states its colour in a `colr` box
        // instead. The flag is stated whether or not a timing record is, which
        // is why it is read outside the branch above.
        if bits.bit()? == 1 {
            return None;
        }
        let initial_display_delay = bits.bit()? == 1;
        let count = bits.bits(5)? + 1;
        for _ in 0..count {
            bits.skip(12)?;
            let level = bits.bits(5)?;
            if level > 7 {
                bits.skip(1)?;
            }
            if initial_display_delay && bits.bit()? == 1 {
                bits.skip(4)?;
            }
        }
    } else {
        // A reduced still picture header states one operating point and no tier.
        bits.skip(5)?;
    }
    let width_bits = bits.bits(4)? + 1;
    let height_bits = bits.bits(4)? + 1;
    bits.skip(width_bits + height_bits)?;
    let frame_ids = !reduced && bits.bit()? == 1;
    if frame_ids {
        bits.skip(4 + 3)?;
    }
    bits.skip(1 + 1 + 1)?;
    if !reduced {
        bits.skip(1 + 1 + 1 + 1)?;
        let order_hint = bits.bit()? == 1;
        if order_hint {
            bits.skip(1 + 1)?;
        }
        let chosen = bits.bit()? == 1;
        let screen_content_tools = if chosen { 2 } else { bits.bit()? };
        if screen_content_tools > 0 {
            let chosen = bits.bit()? == 1;
            if !chosen {
                bits.skip(1)?;
            }
        }
        if order_hint {
            bits.skip(3)?;
        }
    }
    bits.skip(1 + 1 + 1)?;

    // color_config(), up to the point every branch has stated its range.
    let high_bitdepth = bits.bit()? == 1;
    let _twelve_bit = seq_profile == 2 && high_bitdepth && bits.bit()? == 1;
    let monochrome = seq_profile != 1 && bits.bit()? == 1;
    let described = bits.bit()? == 1;
    let (primaries, transfer, matrix) = if described {
        (
            u8::try_from(bits.bits(8)?).ok()?,
            u8::try_from(bits.bits(8)?).ok()?,
            u8::try_from(bits.bits(8)?).ok()?,
        )
    } else {
        (UNSPECIFIED, UNSPECIFIED, UNSPECIFIED)
    };
    // The range of a monochrome item is stated here too, and it is the one the
    // frame properties are written from.
    let _monochrome = monochrome;
    let full_range = bits.bit()? == 1;
    Some(Cicp {
        primaries,
        transfer,
        matrix,
        full_range,
    })
}

/// The payload of the first sequence header of a coded av1 item.
///
/// The payload of a still image is a series of open bitstream units, and the
/// sequence header is the one that carries the colour description. A file whose
/// payload holds none states its colour in a `colr` box.
fn sequence_header_payload(payload: &[u8]) -> Option<&[u8]> {
    const SEQUENCE_HEADER: u8 = 1;
    let mut at = 0;
    while at < payload.len() {
        let header = *payload.get(at)?;
        let kind = (header >> 3) & 0x0f;
        let extension = header & 0x04 != 0;
        let has_size = header & 0x02 != 0;
        at += 1;
        if extension {
            at += 1;
        }
        let size = if has_size {
            let (size, consumed) = leb128(payload.get(at..)?)?;
            at += consumed;
            size
        } else {
            payload.len() - at
        };
        let body = payload.get(at..at.checked_add(size)?)?;
        if kind == SEQUENCE_HEADER {
            return Some(body);
        }
        at += size;
    }
    None
}

/// A little endian base 128 number, and how many bytes it took.
fn leb128(data: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0usize;
    for (index, byte) in data.iter().enumerate() {
        value = (value << 7) | usize::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
    }
    None
}

/// A reader over the bits of a sequence header, most significant bit first.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Bits<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    /// One bit.
    fn bit(&mut self) -> Option<u32> {
        let byte = self.data.get(self.at / 8)?;
        let bit = u32::from(*byte >> (7 - self.at % 8)) & 1;
        self.at += 1;
        Some(bit)
    }

    /// `count` bits, as a number.
    fn bits(&mut self, count: u32) -> Option<u32> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | self.bit()?;
        }
        Some(value)
    }

    /// Skips `count` bits.
    fn skip(&mut self, count: u32) -> Option<()> {
        self.at = self.at.checked_add(usize::try_from(count).ok()?)?;
        (self.at <= self.data.len() * 8).then_some(())
    }

    /// An unsigned variable length code, which av1 writes as a run of zeros, a
    /// one, and that many more bits.
    fn uvlc(&mut self) -> Option<u32> {
        let mut leading = 0;
        while self.bit()? == 0 {
            leading += 1;
            if leading > 32 {
                return None;
            }
        }
        if leading == 0 {
            return Some(0);
        }
        Some((1 << leading) - 1 + self.bits(leading)?)
    }
}

/// Offset of `needle` in `haystack`.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The leading boxes of a file, up to the media data box that holds the coded
/// image itself.
///
/// The boxes read here are a prefix of the file, which is what lets a range into
/// this buffer be read from the file as well.
fn leading_boxes(file: &mut File) -> Option<Vec<u8>> {
    let mut boxes = Vec::new();
    loop {
        let mut header = [0; 8];
        if file.read_exact(&mut header).is_err() {
            break;
        }
        let mut size = u64::from(u32::from_be_bytes(header[..4].try_into().ok()?));
        if &header[4..] == b"mdat" {
            break;
        }
        let start = boxes.len();
        boxes.extend_from_slice(&header);
        if size == 1 {
            let mut extended = [0; 8];
            file.read_exact(&mut extended).ok()?;
            boxes.extend_from_slice(&extended);
            size = u64::from_be_bytes(extended);
        }
        let payload = usize::try_from(size.checked_sub((boxes.len() - start) as u64)?).ok()?;
        if size == 0 || start + header.len() + payload > HEADER_LIMIT {
            return None;
        }
        boxes.resize(start + header.len() + payload, 0);
        file.read_exact(&mut boxes[start + header.len()..]).ok()?;
    }
    (!boxes.is_empty()).then_some(boxes)
}

/// Reads one byte range of an open file.
fn read_range(file: &mut File, range: Range<usize>) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(
        u64::try_from(range.start).unwrap_or(u64::MAX),
    ))?;
    let mut buffer = vec![0; range.end.saturating_sub(range.start)];
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

/// Builds the error a dav1d call reports for one image.
fn decode_error(path: &Path, error: impl std::fmt::Display) -> ImgSeqError {
    image_error("decode", path, error)
}

fn has_avif_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            AVIF_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use image::ExtendedColorType;

    use crate::pixel::alpha_channel;

    fn info(path: &str, format: PixelFormat, color_type: ColorType) -> ImageInfo {
        ImageInfo {
            path: PathBuf::from(path),
            width: 64,
            height: 48,
            color_type,
            original_color_type: ExtendedColorType::Rgb8,
            has_icc_profile: false,
            cicp: None,
            chroma_location: None,
            orientation: Orientation::NoTransforms,
            transform: Transform::IDENTITY,
            format,
        }
    }

    /// A box whose contents are `payload`.
    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(8 + payload.len());
        bytes.extend_from_slice(&u32::try_from(8 + payload.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn ispe(width: u32, height: u32) -> Vec<u8> {
        let mut payload = vec![0; 4];
        payload.extend_from_slice(&width.to_be_bytes());
        payload.extend_from_slice(&height.to_be_bytes());
        payload
    }

    /// The coding record of an item whose `av1C` flags byte is `flags`.
    fn av1c(flags: u8) -> Vec<u8> {
        vec![0x81, 0x10, flags, 0, 0, 0, 0]
    }

    /// An `nclx` colour box payload.
    fn nclx(primaries: u8, transfer: u8, matrix: u8, full_range: bool) -> Vec<u8> {
        let mut payload = b"nclx".to_vec();
        for code in [primaries, transfer, matrix] {
            payload.extend_from_slice(&u16::from(code).to_be_bytes());
        }
        payload.push(if full_range { 0x80 } else { 0x00 });
        payload
    }

    /// The properties every fixture in these tests states: a size, a coding
    /// record for 4:2:0 eight bit samples, and bt.601 full range codes.
    fn properties() -> Vec<(&'static [u8; 4], Vec<u8>)> {
        vec![
            (b"ispe", ispe(3, 2)),
            (b"av1C", av1c(0x0c)),
            (b"colr", nclx(1, 13, 6, true)),
        ]
    }

    /// A file type box with `major` as its major brand and `compatible` beside
    /// it.
    fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let mut payload = major.to_vec();
        payload.extend_from_slice(&0u32.to_be_bytes());
        for brand in compatible {
            payload.extend_from_slice(*brand);
        }
        boxed(b"ftyp", &payload)
    }

    /// The leading boxes of an avif whose primary item is described by
    /// `properties`, with the references and the auxiliary item given.
    ///
    /// Every reference is a `(kind, from, to)` triple of an `iref` box, and
    /// `aux` is the type name of a second item's `auxC` when the container holds
    /// one.
    fn container(
        properties: &[(&[u8; 4], Vec<u8>)],
        references: &[(&[u8; 4], u16, u16)],
        aux: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut ipco = Vec::new();
        for (kind, payload) in properties {
            ipco.extend_from_slice(&boxed(kind, payload));
        }
        let mut ipma = vec![0; 4];
        ipma.extend_from_slice(&u32::from(if aux.is_some() { 2u16 } else { 1u16 }).to_be_bytes());
        ipma.extend_from_slice(&1u16.to_be_bytes());
        ipma.push(u8::try_from(properties.len()).unwrap());
        for index in 1..=properties.len() {
            ipma.push(u8::try_from(index).unwrap() | 0x80);
        }
        if let Some(aux) = aux {
            // The second item's own property, and its association.
            ipco.extend_from_slice(&boxed(b"auxC", aux));
            ipma.extend_from_slice(&2u16.to_be_bytes());
            ipma.push(1);
            ipma.push(u8::try_from(properties.len() + 1).unwrap() | 0x80);
        }
        // Every item is one extent of eight bytes at offset zero, except the one
        // a reference points at, which is not located at all.
        let items = if aux.is_some() { 2 } else { 1 };
        let mut iloc = vec![0, 0, 0, 0, 0x44, 0x40];
        iloc.extend_from_slice(&u16::try_from(items).unwrap().to_be_bytes());
        for item in 1..=items {
            iloc.extend_from_slice(&u16::try_from(item).unwrap().to_be_bytes());
            iloc.extend_from_slice(&0u16.to_be_bytes());
            iloc.extend_from_slice(&0u32.to_be_bytes());
            iloc.extend_from_slice(&1u16.to_be_bytes());
            iloc.extend_from_slice(&0u32.to_be_bytes());
            iloc.extend_from_slice(&8u32.to_be_bytes());
        }
        let mut meta = vec![0; 4];
        meta.extend_from_slice(&boxed(b"pitm", &[0, 0, 0, 0, 0, 1]));
        if !references.is_empty() {
            let mut body = vec![0; 4];
            for (kind, from, to) in references {
                let mut reference = Vec::new();
                reference.extend_from_slice(&from.to_be_bytes());
                reference.extend_from_slice(&1u16.to_be_bytes());
                reference.extend_from_slice(&to.to_be_bytes());
                body.extend_from_slice(&boxed(kind, &reference));
            }
            meta.extend_from_slice(&boxed(b"iref", &body));
        }
        meta.extend_from_slice(&boxed(
            b"iprp",
            &[boxed(b"ipco", &ipco), boxed(b"ipma", &ipma)].concat(),
        ));
        meta.extend_from_slice(&boxed(b"iloc", &iloc));
        let mut file = ftyp(b"avif", &[b"mif1", b"miaf"]);
        file.extend_from_slice(&boxed(b"meta", &meta));
        file
    }

    /// A writer of the bits of a sequence header.
    struct Header {
        bits: Vec<bool>,
    }

    impl Header {
        fn new() -> Self {
            Self { bits: Vec::new() }
        }

        fn put(&mut self, value: u32, count: usize) -> &mut Self {
            for shift in (0..count).rev() {
                self.bits.push((value >> shift) & 1 == 1);
            }
            self
        }

        fn bytes(&self) -> Vec<u8> {
            let mut bytes = vec![0; self.bits.len().div_ceil(8)];
            for (index, bit) in self.bits.iter().enumerate() {
                if *bit {
                    bytes[index / 8] |= 0x80 >> (index % 8);
                }
            }
            bytes
        }
    }

    /// The bits up to `color_config()` of a still picture header of the reduced
    /// shape, three samples wide, two tall, with the ten bit colour codes
    /// `described` states.
    fn reduced_header(described: bool) -> Vec<u8> {
        let mut header = Header::new();
        header.put(0, 3); // seq_profile
        header.put(1, 1); // still_picture
        header.put(1, 1); // reduced_still_picture_header
        header.put(0, 5); // operating_points_cnt_minus_1
        header.put(3, 4).put(3, 4); // frame width and height bits
        header.put(2, 4).put(1, 4); // three wide, two tall
        header.put(0, 1).put(0, 1).put(0, 1); // superblock, filter intra, edge
        header.put(0, 1).put(1, 1).put(1, 1); // superres, cdef, restoration
        header.put(1, 1); // high_bitdepth
        header.put(0, 1); // monochrome
        header.put(u32::from(described), 1);
        if described {
            header.put(1, 8).put(13, 8).put(6, 8);
        }
        header.put(1, 1); // color_range
        header.bytes()
    }

    /// The bits up to `color_config()` of a header of the longer shape, with a
    /// timing record absent, one operating point, frame ids, an order hint and
    /// screen content tools, and limited range codes.
    fn longer_header() -> Vec<u8> {
        let mut header = Header::new();
        header.put(0, 3).put(1, 1).put(0, 1); // profile, still picture, not reduced
        header.put(0, 1); // timing_info_present_flag
        header.put(0, 1); // decoder_model_info_present_flag
        header.put(1, 1); // initial_display_delay_present_flag
        header.put(0, 5); // operating_points_cnt_minus_1
        header.put(0, 12); // operating_point_idc
        header.put(8, 5).put(0, 1); // seq_level_idx, which is past seven
        header.put(1, 1).put(2, 4); // initial display delay
        header.put(3, 4).put(3, 4);
        header.put(2, 4).put(1, 4); // three wide, two tall
        header.put(1, 1); // frame_id_numbers_present_flag
        header.put(0, 4).put(0, 3); // the two frame id lengths
        header.put(0, 1).put(0, 1).put(0, 1); // superblock, filter intra, edge
        header.put(0, 1).put(0, 1).put(0, 1).put(0, 1); // the four compound tools
        header.put(1, 1); // enable_order_hint
        header.put(0, 1).put(0, 1); // joint compound, ref frame mvs
        header.put(1, 1); // seq_choose_screen_content_tools
        header.put(0, 1); // seq_choose_integer_mv
        header.put(0, 1); // seq_force_integer_mv, which a chosen zero states too
        header.put(2, 3); // order_hint_bits_minus_1
        header.put(0, 1).put(1, 1).put(1, 1); // superres, cdef, restoration
        header.put(1, 1); // high_bitdepth
        header.put(0, 1); // monochrome
        header.put(1, 1); // color_description_present_flag
        header.put(1, 8).put(13, 8).put(6, 8);
        header.put(0, 1); // color_range
        header.bytes()
    }

    /// The payload of an item that starts with one sequence header box.
    fn with_sequence_header(header: &[u8]) -> Vec<u8> {
        let mut payload = vec![(1 << 3) | 0x02];
        payload.push(u8::try_from(header.len()).unwrap());
        payload.extend_from_slice(header);
        payload
    }

    #[test]
    fn only_avif_extensions_are_taken_over() {
        for path in ["a.avif", "b.AVIF", "c.AvIf"] {
            assert!(has_avif_extension(Path::new(path)), "{path}");
        }
        for path in ["a.heic", "b.png", "c.avifx", "d"] {
            assert!(!has_avif_extension(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn handles_needs_an_avif_extension_and_a_yuv_probe() {
        assert!(handles(&info(
            "a.avif",
            PixelFormat::Yuv420P8,
            ColorType::Rgb8
        )));
        assert!(handles(&info(
            "a.avif",
            PixelFormat::Yuv444P12,
            ColorType::Rgb16
        )));
        // A file the probe leaves to the decoder keeps it, and a monochrome
        // item is one of those.
        assert!(!handles(&info(
            "a.avif",
            PixelFormat::Rgb8,
            ColorType::Rgb8
        )));
        assert!(!handles(&info("a.avif", PixelFormat::Gray8, ColorType::L8)));
        assert!(!handles(&info(
            "a.heic",
            PixelFormat::Yuv420P8,
            ColorType::Rgb8
        )));
    }

    #[test]
    fn the_subsampling_of_a_coding_record_maps_onto_a_format() {
        assert_eq!(yuv_format(2, 8), Some(PixelFormat::Yuv420P8));
        assert_eq!(yuv_format(1, 8), Some(PixelFormat::Yuv422P8));
        assert_eq!(yuv_format(0, 8), Some(PixelFormat::Yuv444P8));
        assert_eq!(yuv_format(2, 10), Some(PixelFormat::Yuv420P10));
        assert_eq!(yuv_format(1, 10), Some(PixelFormat::Yuv422P10));
        assert_eq!(yuv_format(0, 10), Some(PixelFormat::Yuv444P10));
        assert_eq!(yuv_format(0, 12), Some(PixelFormat::Yuv444P12));
        // A layout and a depth with no format of their own keep the decoder
        // that has one.
        assert_eq!(yuv_format(2, 12), None);
        assert_eq!(yuv_format(1, 12), None);
        assert_eq!(yuv_format(0, 16), None);
    }

    #[test]
    fn a_matrix_the_properties_cannot_name_is_not_handed_out_as_yuv() {
        assert!(usable_matrix(1));
        assert!(usable_matrix(6));
        // The identity is a statement about samples that are already r,g,b.
        assert!(!usable_matrix(0));
        // "Unspecified" would have to be labelled with a guess.
        assert!(!usable_matrix(UNSPECIFIED));
    }

    #[test]
    fn a_coding_record_states_its_depth() {
        assert_eq!(av1_bit_depth(0x0c), 8);
        assert_eq!(av1_bit_depth(0x4c), 10);
        assert_eq!(av1_bit_depth(0x6c), 12);
        // Samples are only twelve bit with the high bit depth flag, which is
        // what the specification requires of the second flag.
        assert_eq!(av1_bit_depth(0x2c), 8);
    }

    #[test]
    fn a_colour_box_states_the_codes_it_holds() {
        assert_eq!(
            nclx_cicp(&nclx(1, 13, 6, true)),
            Some(Cicp {
                primaries: 1,
                transfer: 13,
                matrix: 6,
                full_range: true,
            })
        );
        assert_eq!(
            nclx_cicp(&nclx(9, 18, 0, false)),
            Some(Cicp {
                primaries: 9,
                transfer: 18,
                matrix: 0,
                full_range: false,
            })
        );
        // An opaque profile states no codes at all, and neither does a code
        // point that does not fit in a byte.
        assert_eq!(nclx_cicp(b"prof\0\0\0\0\0\0\0\0"), None);
        let mut wide = nclx(1, 13, 6, true);
        wide[8..10].copy_from_slice(&256u16.to_be_bytes());
        assert_eq!(nclx_cicp(&wide), None);
    }

    #[test]
    fn a_sequence_header_states_the_colour_of_the_bitstream() {
        let payload = with_sequence_header(&reduced_header(true));
        assert_eq!(
            sequence_header_cicp(&payload),
            Some(Cicp {
                primaries: 1,
                transfer: 13,
                matrix: 6,
                full_range: true,
            })
        );
    }

    #[test]
    fn a_sequence_header_of_the_longer_shape_is_read_too() {
        let payload = with_sequence_header(&longer_header());
        assert_eq!(
            sequence_header_cicp(&payload),
            Some(Cicp {
                primaries: 1,
                transfer: 13,
                matrix: 6,
                full_range: false,
            })
        );
    }

    #[test]
    fn a_sequence_header_without_colour_codes_states_none() {
        let payload = with_sequence_header(&reduced_header(false));
        assert_eq!(
            sequence_header_cicp(&payload),
            Some(Cicp {
                primaries: UNSPECIFIED,
                transfer: UNSPECIFIED,
                matrix: UNSPECIFIED,
                full_range: true,
            })
        );
    }

    #[test]
    fn a_payload_without_a_sequence_header_states_nothing() {
        // A temporal delimiter and nothing else, which is what the payload of a
        // file that states its colour in a `colr` box looks like.
        assert_eq!(sequence_header_cicp(&[0x12, 0x00]), None);
        assert_eq!(sequence_header_cicp(&[]), None);
    }

    #[test]
    fn a_file_type_without_the_avif_brand_is_left_alone() {
        assert!(has_avif_brand(&container(&properties(), &[], None)));
        // The major brand may be another one, as long as the file lists an avif
        // brand among the compatible ones.
        let mut listed = ftyp(b"mif1", &[b"mif1", b"avif"]);
        listed.extend_from_slice(&boxed(b"meta", &[0; 4]));
        assert!(has_avif_brand(&listed));
        let mut heic = ftyp(b"heic", &[b"heic", b"mif1"]);
        heic.extend_from_slice(&boxed(b"meta", &[0; 4]));
        assert!(!has_avif_brand(&heic));
        assert!(!has_avif_brand(&boxed(b"mdat", &[0; 8])));
    }

    #[test]
    fn the_item_boxes_describe_the_primary_item() {
        let boxes = container(&properties(), &[], None);
        let meta = Meta::read(&boxes).expect("the container is walked");
        assert_eq!(meta.primary, 1);
        assert!(!meta.has_alpha());
        assert!(!meta.grid);
        let properties = meta.primary_properties().expect("the item has properties");
        let header = AvifHeader::read(&properties).expect("the properties describe it");
        assert_eq!((header.width, header.height), (3, 2));
        assert_eq!(header.depth, 8);
        assert_eq!(header.chroma, 2);
        assert!(!header.monochrome);
        assert_eq!(
            header.cicp,
            Some(Cicp {
                primaries: 1,
                transfer: 13,
                matrix: 6,
                full_range: true,
            })
        );
        // The property list is associated from one, not from zero.
        assert_eq!(
            properties
                .iter()
                .map(|property| std::str::from_utf8(property.kind).unwrap())
                .collect::<Vec<_>>(),
            ["ispe", "av1C", "colr"]
        );
        assert_eq!(meta.primary_data(usize::MAX).expect("located"), 0..8);
    }

    #[test]
    fn an_auxiliary_item_that_names_alpha_describes_its_own_data() {
        let boxes = container(&properties(), &[(b"auxl", 2, 1)], Some(ALPHA_AUX_TYPES[0]));
        let meta = Meta::read(&boxes).expect("the container is walked");
        assert!(meta.has_alpha());
        assert_eq!(meta.alpha_data(usize::MAX).expect("located"), Some(0..8));
    }

    #[test]
    fn an_auxiliary_item_that_does_not_name_alpha_is_not_one() {
        let boxes = container(
            &properties(),
            &[(b"auxl", 2, 1)],
            Some(b"urn:mpeg:mpegB:cicp:systems:auxiliary:depth\0"),
        );
        let meta = Meta::read(&boxes).expect("the container is walked");
        assert!(!meta.has_alpha());
        assert_eq!(
            meta.alpha_data(usize::MAX).expect("nothing to locate"),
            None
        );
    }

    #[test]
    fn a_grid_of_tiles_is_not_the_picture_itself() {
        let boxes = container(&properties(), &[(b"dimg", 1, 2)], None);
        let meta = Meta::read(&boxes).expect("the container is walked");
        assert!(meta.grid);
    }

    #[test]
    fn an_item_split_over_several_extents_is_not_read() {
        let boxes = container(&properties(), &[], None);
        let mut meta = Meta::read(&boxes).expect("the container is walked");
        let (item, method, ranges) = meta.extents.pop().expect("one item");
        assert_eq!((item, method), (1, 0));
        meta.extents.push((1, 0, vec![ranges[0], ranges[0]]));
        assert!(meta.primary_data(usize::MAX).is_err());
    }

    #[test]
    fn an_item_written_into_the_item_data_box_is_read_from_there() {
        // Version one of `iloc`, with a construction method, which is how a
        // writer that keeps its item data inside `meta` locates an item.
        let mut payload = vec![1, 0, 0, 0, 0x44, 0x40];
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.extend_from_slice(&0u16.to_be_bytes());
        payload.extend_from_slice(&16u32.to_be_bytes());
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&4u32.to_be_bytes());
        assert_eq!(read_iloc(&payload), Some(vec![(1, 1, vec![(16, 4)])]));
    }

    #[test]
    fn an_association_table_of_the_wide_shape_is_read() {
        // Version zero with the wide flag, whose association indices are
        // sixteen bit, and whose essential flag is the top bit.
        let payload = [0, 0, 0, 1, 0, 0, 0, 1, 0, 1, 1, 0x81, 0x02];
        assert_eq!(read_ipma(&payload), Some(vec![(1, vec![258])]));
        // Version one, whose item ids are thirty two bit.
        let payload = [1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 2, 2, 3, 0x84];
        assert_eq!(read_ipma(&payload), Some(vec![(2, vec![3, 4])]));
    }

    #[test]
    fn a_colour_fixture_is_probed_as_the_yuv_it_holds() {
        for (name, format, color_type) in [
            ("avif-yuv420p.avif", PixelFormat::Yuv420P8, ColorType::Rgb8),
            ("avif-yuv422p.avif", PixelFormat::Yuv422P8, ColorType::Rgb8),
            (
                "avif-yuv444p10.avif",
                PixelFormat::Yuv444P10,
                ColorType::Rgb16,
            ),
        ] {
            let path = Path::new("tests/fixtures").join(name);
            let info = image_info(&path).expect("the container describes it");
            assert_eq!(info.format, format, "{name}");
            assert_eq!(info.color_type, color_type, "{name}");
            assert!(handles(&info), "{name}");
            // The codes of the bitstream, which no `colr` box repeats here.
            assert_eq!(
                info.cicp,
                Some(Cicp {
                    primaries: 1,
                    transfer: 13,
                    matrix: 6,
                    full_range: true,
                }),
                "{name}"
            );
            assert!(!info.has_icc_profile, "{name}");
            // Every one of them states a chroma sample position of zero, which
            // is the code for "unknown".
            assert_eq!(info.chroma_location, None, "{name}");
            assert_eq!(info.transform, Transform::IDENTITY, "{name}");
        }
    }

    #[test]
    fn a_yuv_fixture_with_an_alpha_item_is_probed_as_the_page_and_its_alpha() {
        let path = Path::new("tests/fixtures/alpha-yuv420p.avif");
        let info = image_info(path).expect("the container describes it");
        assert_eq!((info.width, info.height), (4, 4));
        assert_eq!(info.format, PixelFormat::Yuv420P8);
        assert_eq!(info.color_type, ColorType::Rgba8);
        assert!(handles(&info));
        assert_eq!(alpha_channel(info.color_type), Some(3));
        // The alpha item is decoded into the gray format of the same depth.
        assert_eq!(info.format.alpha_format(), PixelFormat::Gray8);
    }

    #[test]
    fn a_fixture_that_states_the_identity_keeps_the_rgb_it_holds() {
        // Both of these state bt.2020 primaries with the identity matrix, whose
        // samples are r,g,b, so neither is handed out as yuv.
        for name in ["cicp-rgb8.avif", "alpha-rgba8.avif"] {
            let path = Path::new("tests/fixtures").join(name);
            let info = image_info(&path).expect("the container describes it");
            assert_eq!(info.format, PixelFormat::Rgb8, "{name}");
            assert!(!handles(&info), "{name}");
            // The colour of the container is still stated for a file that keeps
            // the r,g,b path.
            let (primaries, transfer) = if name.starts_with("cicp") {
                (9, 18)
            } else {
                (1, 13)
            };
            assert_eq!(
                info.cicp
                    .map(|cicp| (cicp.primaries, cicp.transfer, cicp.matrix)),
                Some((primaries, transfer, 0)),
                "{name}"
            );
        }
    }

    #[test]
    fn a_monochrome_fixture_keeps_its_gray_format() {
        let path = Path::new("tests/fixtures/mono-alpha.avif");
        let info = image_info(path).expect("the container describes it");
        assert_eq!(info.format, PixelFormat::Gray8);
        // The item holds one sample per pixel, and the decoder this file is
        // left to reports four channels for it, which is what the frame request
        // is checked against.
        assert_eq!(info.color_type, ColorType::Rgba8);
        assert_eq!(info.original_color_type, ExtendedColorType::La8);
        assert_eq!(alpha_channel(info.color_type), Some(3));
        assert!(!handles(&info));
        assert_eq!(
            output_format(path, ColorType::Rgba8),
            Some(PixelFormat::Gray8)
        );
        // A decoder that reports the gray color type needs no correction.
        assert_eq!(output_format(path, ColorType::La8), None);
        assert_eq!(output_format(Path::new("a.avif"), ColorType::Rgba8), None);
    }

    #[test]
    fn another_container_is_not_described_here() {
        assert!(image_info(Path::new("tests/fixtures/alpha-rgba8.heic")).is_none());
        assert!(image_info(Path::new("tests/fixtures/alpha-rgba8.jxl")).is_none());
        assert!(image_info(Path::new("tests/fixtures/nowhere.avif")).is_none());
    }

    #[test]
    fn a_probed_fixture_decodes_into_the_planes_its_format_describes() {
        let path = Path::new("tests/fixtures/avif-yuv420p.avif");
        let info = image_info(path).expect("the container describes it");
        let decoded = decode(&info).expect("the item is decoded");
        let Pixels::Planar { planes, alpha } = decoded.pixels else {
            panic!("a yuv page is handed out as planes");
        };
        assert_eq!(decoded.format, PixelFormat::Yuv420P8);
        assert_eq!(
            planes.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![64 * 48, 32 * 24, 32 * 24]
        );
        // The fixture has no alpha item, and the first sample of the page is not
        // zero, so a plane that was never written shows up here.
        assert!(alpha.is_none());
        assert_ne!(planes[0][0], 0);
    }

    #[test]
    fn the_planes_of_an_alpha_fixture_are_the_ones_its_source_holds() {
        let path = Path::new("tests/fixtures/alpha-yuv420p.avif");
        let info = image_info(path).expect("the container describes it");
        let decoded = decode(&info).expect("the item is decoded");
        let Pixels::Planar { planes, alpha } = decoded.pixels else {
            panic!("a yuv page is handed out as planes");
        };
        assert_eq!(decoded.format, PixelFormat::Yuv420P8);
        // The source is neutral, so its four rows are the whole luma plane and
        // both chroma planes are the midpoint.
        assert_eq!(
            planes[0],
            [
                0, 0, 0, 0, 72, 72, 72, 72, 144, 144, 144, 144, 216, 216, 216, 216
            ]
        );
        assert_eq!(planes[1], [128, 128, 128, 128]);
        assert_eq!(planes[2], [128, 128, 128, 128]);
        // Its alpha is one step per column, which the encoder writes losslessly.
        assert_eq!(
            alpha.expect("the container holds an alpha item"),
            [
                0, 85, 170, 255, 0, 85, 170, 255, 0, 85, 170, 255, 0, 85, 170, 255
            ]
        );
    }
}
