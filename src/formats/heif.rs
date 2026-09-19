//! heif, heic and avif decoding through `libheif` itself.
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
//!
//! The `image` avif decoder reaches the same samples by another road, but it
//! converts every file to r,g,b as it decodes it, so it reports `Rgba8` for a
//! monochrome avif and the file would be handed out as `RGB24`, three times the
//! bytes of the single plane it holds. The container says what the samples are,
//! so [`output_format`] reads the leading boxes and the probe hands the file out
//! as `Gray8` or `Gray16` with the alpha channel the boxes name. Decoding avif
//! through `libheif` instead would copy the plane the file holds, but only a
//! `libheif` built with a decoder for av1 can read avif at all, and the windows
//! build linked here has none (`libde265` is its only decoder), so the format is
//! corrected rather than a second decode path being added.
//!
//! That decoder is also why avif is probed here at all. Its decoder decodes the
//! whole picture and the alpha item beside it when it is created, which is what
//! lets it report a size, so probing through it costs a full decode that the
//! frame request then repeats ([`image_info`] reads the same facts out of the
//! container for a few hundred microseconds instead). A file this module will
//! not describe keeps the decoder and the cost, and so does the format
//! correction above, which is the fallback for exactly those files.

use std::{fs::File, io::Read, path::Path, time::Instant};

use image::{ColorType, metadata::Orientation};
use libheif_rs::{ColorSpace, HeifContext, LibHeif, Plane};

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    pixel::{PixelFormat, Transform},
};

/// File extensions that hold a heif container.
const HEIF_EXTENSIONS: [&str; 4] = ["heic", "heics", "heif", "hif"];

/// File extensions that hold an avif container.
const AVIF_EXTENSIONS: [&str; 1] = ["avif"];

/// Whether this module decodes `info`.
///
/// Only the images the `image` integration cannot represent are taken over, so
/// colour heif files keep going through the hook and the colour conversion it
/// performs.
pub fn handles(info: &ImageInfo) -> bool {
    has_heif_extension(&info.path) && !integration_can_decode(info.color_type)
}

/// Format a monochrome avif is handed out as, or `None` when the file keeps the
/// format its color type maps to.
///
/// This is the fallback for a file [`image_info`] would not describe, so it only
/// reads the leading boxes of the container; see the module documentation. The
/// pixels stay the ones the `image` decoder produced: for a monochrome bitstream
/// the conversion it performs has no chroma to mix in, so every channel holds
/// the same sample.
pub fn output_format(path: &Path, color_type: ColorType) -> Option<PixelFormat> {
    // A decoder that already reports one of the monochrome color types maps to
    // the same format on its own, and every other file keeps what it reports.
    let decoded = PixelFormat::from_color_type(color_type)?;
    if is_monochrome(decoded) {
        return None;
    }
    if !has_avif_extension(path) {
        return None;
    }
    let header = avif_header(&leading_boxes(path)?)?;
    header.format()
}

/// What the container of an avif states about its image, when [`image_info`] can
/// describe the file from it.
///
/// The probe would otherwise have to build the `image` decoder, which decodes
/// the whole picture and the alpha item beside it before it can report a size
/// (see the module documentation), and that decode is then repeated when a frame
/// asks for the file. Everything the probe records is in the container instead:
/// `ispe` holds the size of the image, `av1C` the bit depth and whether the
/// samples are monochrome, `colr` whether an ICC profile is attached, and `auxC`
/// whether an alpha item exists.
///
/// `None` means "let the `image` decoder describe this file", which is what an
/// unusual container gets: a second `ispe` that disagrees with the first, coded
/// records of different bit depths, and any geometry this probe does not
/// replicate.
pub fn image_info(path: &Path) -> Option<ImageInfo> {
    if !has_avif_extension(path) {
        return None;
    }
    let boxes = leading_boxes(path)?;
    if !has_avif_brand(&boxes) {
        return None;
    }
    let header = avif_header(&boxes)?;
    Some(ImageInfo {
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        // Whatever the file holds, the `image` decoder decodes four channels,
        // so this is the color type it reports and the frame request is checked
        // against. For a monochrome file the format is corrected below instead.
        color_type: header.decoded_color_type(),
        // The color type of the samples the file holds, which is what the other
        // containers report as the original one.
        original_color_type: header.file_color_type().into(),
        has_icc_profile: header.has_icc_profile,
        // The avif decoder reports no orientation: it does not read the exif
        // metadata the `image` trait would take one from. The box walk this
        // probe is built on could read the `Exif` item the same way it reads
        // `ispe` and `colr`, which is the left over plan 09 leaves open.
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: header.format()?,
    })
}

/// What the leading boxes of an avif state about its image.
struct AvifHeader {
    width: u32,
    height: u32,
    /// Bits of one sample of the primary item, as the coded record states it.
    depth: u8,
    /// Whether every coded record describes one sample per pixel.
    monochrome: bool,
    /// Whether an auxiliary item holds alpha.
    alpha: bool,
    has_icc_profile: bool,
}

impl AvifHeader {
    /// The color type the `image` avif decoder reports for this file.
    ///
    /// Its decoder hands back four channels whatever the bitstream holds, and
    /// the frame request compares what it reports against what the probe
    /// recorded, so the probe has to record this and not the file's own depth.
    fn decoded_color_type(&self) -> ColorType {
        if self.depth > 8 {
            ColorType::Rgba16
        } else {
            ColorType::Rgba8
        }
    }

    /// The color type of the samples the file holds.
    ///
    /// A colour file holds three channels and an alpha channel besides, which
    /// is what the decoder reports for it and what the other containers call
    /// the original color type. A monochrome file holds the one sample the
    /// container says it does, and the alpha item beside it when there is one.
    fn file_color_type(&self) -> ColorType {
        match (self.monochrome, self.depth > 8, self.alpha) {
            (false, false, _) => ColorType::Rgba8,
            (false, true, _) => ColorType::Rgba16,
            (true, false, false) => ColorType::L8,
            (true, false, true) => ColorType::La8,
            (true, true, false) => ColorType::L16,
            (true, true, true) => ColorType::La16,
        }
    }

    /// Format the file is handed out as: the one its own samples map to when
    /// they are monochrome, and the one the decoded color type maps to when they
    /// are not.
    fn format(&self) -> Option<PixelFormat> {
        PixelFormat::from_color_type(self.file_color_type())
    }
}

/// Whether this format holds one sample per pixel.
const fn is_monochrome(format: PixelFormat) -> bool {
    matches!(
        format,
        PixelFormat::Gray8 | PixelFormat::Gray16 | PixelFormat::Gray32F
    )
}

/// Whether the leading boxes start with a file type that can hold av1 items.
///
/// A file that is not one of these is left to the `image` decoder, which sniffs
/// the format itself and may route it elsewhere: an `mif1` file under an `.avif`
/// name decodes through the heif hook, whose color type `image_info` cannot
/// predict.
fn has_avif_brand(boxes: &[u8]) -> bool {
    const BRANDS: [&[u8]; 2] = [b"avif", b"avis"];
    let Some((major, rest)) = child_boxes(boxes).and_then(|boxes| boxes.into_iter().next()) else {
        return false;
    };
    if major != *b"ftyp" {
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

/// The facts the leading boxes of an avif state, or `None` when they are not the
/// shape this module is willing to describe.
fn avif_header(boxes: &[u8]) -> Option<AvifHeader> {
    const MONO_CHROME: u8 = 0x10;

    let mut size = None;
    let mut depth = None;
    let mut monochrome = true;
    let mut alpha = false;
    let mut has_icc_profile = false;
    for (kind, payload) in item_properties(boxes)? {
        match &kind {
            b"ispe" => {
                let width = u32::from_be_bytes(payload.get(4..8)?.try_into().ok()?);
                let height = u32::from_be_bytes(payload.get(8..12)?.try_into().ok()?);
                // One property list can hold the properties of several items,
                // and only the size of the primary one may be assumed. Files
                // where they disagree are left to the decoder.
                if size.is_some_and(|known: (u32, u32)| known != (width, height)) {
                    return None;
                }
                size = Some((width, height));
            }
            b"av1C" => {
                let flags = *payload.get(2)?;
                let record = av1_bit_depth(flags);
                if depth.is_some_and(|known: u8| known != record) {
                    return None;
                }
                depth = Some(record);
                monochrome &= flags & MONO_CHROME != 0;
            }
            b"colr" => has_icc_profile |= matches!(payload.get(..4)?, b"prof" | b"rICC"),
            b"auxC" => {
                alpha |= ALPHA_AUX_TYPES
                    .iter()
                    .any(|kind| find_bytes(payload, kind).is_some())
            }
            // Boxes that move, crop or rotate the picture: the decoder reports
            // the size after them and this probe would report the size before
            // them, so the file keeps the decoder that reads them.
            b"clap" | b"irot" | b"imir" => return None,
            _ => {}
        }
    }

    let (width, height) = size?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(AvifHeader {
        width,
        height,
        depth: depth?,
        monochrome,
        alpha,
        has_icc_profile,
    })
}

/// The properties of the items of an avif, which its `meta` box holds under
/// `iprp`.
///
/// Walking the boxes rather than searching the bytes keeps a name that appears
/// inside a payload from being read as a box.
fn item_properties(boxes: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    fn child<'a>(boxes: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
        child_boxes(boxes)?
            .into_iter()
            .find(|(found, _)| found == kind)
            .map(|(_, payload)| payload)
    }

    // `meta` is a full box: the version and flags come before its children.
    let meta = child(boxes, b"meta")?.get(4..)?;
    let ipco = child(child(meta, b"iprp")?, b"ipco")?;
    child_boxes(ipco)
}

/// The boxes one container payload holds, in order, as `(kind, payload)` pairs.
///
/// A box whose size is zero runs to the end of its container, and one whose size
/// is one carries a 64 bit size after its kind. A malformed box ends the walk,
/// which leaves its file to the decoder rather than guessing.
fn child_boxes(payload: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    let mut boxes = Vec::new();
    let mut at = 0;
    while at + 8 <= payload.len() {
        let size = u64::from(u32::from_be_bytes(payload[at..at + 4].try_into().ok()?));
        let kind = payload[at + 4..at + 8].try_into().ok()?;
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
        boxes.push((kind, &payload[at + header..at + size]));
        at += size;
    }
    Some(boxes)
}

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

/// `auxC` types that name the auxiliary item holding alpha. The first is the one
/// the avif specification requires, the second is the older hevc type that heif
/// writers used before it.
const ALPHA_AUX_TYPES: [&[u8]; 2] = [
    b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
    b"urn:mpeg:hevc:2015:auxid:1",
];

/// Size limit for the leading boxes of an avif; the metadata of a real file is
/// orders of magnitude smaller, and a file past it is left to the decoder.
const HEADER_LIMIT: usize = 1024 * 1024;

/// The leading boxes of `path`, up to the media data box that holds the coded
/// image itself.
fn leading_boxes(path: &Path) -> Option<Vec<u8>> {
    read_boxes(File::open(path).ok()?)
}

/// Reads whole boxes until the media data box starts, which is not read.
fn read_boxes(mut file: impl Read) -> Option<Vec<u8>> {
    let mut boxes = Vec::new();
    loop {
        let mut size_bytes = [0; 4];
        if file.read_exact(&mut size_bytes).is_err() {
            break;
        }
        let mut kind = [0; 4];
        file.read_exact(&mut kind).ok()?;
        if &kind == b"mdat" {
            break;
        }
        let mut header = size_bytes.to_vec();
        header.extend_from_slice(&kind);
        let mut size = u64::from(u32::from_be_bytes(size_bytes));
        if size == 1 {
            let mut extended = [0; 8];
            file.read_exact(&mut extended).ok()?;
            header.extend_from_slice(&extended);
            size = u64::from_be_bytes(extended);
        }
        let payload = usize::try_from(size.checked_sub(header.len() as u64)?).ok()?;
        if size == 0 || boxes.len() + header.len() + payload > HEADER_LIMIT {
            return None;
        }
        let start = boxes.len();
        boxes.extend_from_slice(&header);
        boxes.resize(start + header.len() + payload, 0);
        file.read_exact(&mut boxes[start + header.len()..]).ok()?;
    }
    (!boxes.is_empty()).then_some(boxes)
}

/// Offset of `needle` in `haystack`.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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
    has_extension(path, &HEIF_EXTENSIONS)
}

fn has_avif_extension(path: &Path) -> bool {
    has_extension(path, &AVIF_EXTENSIONS)
}

fn has_extension(path: &Path, known_extensions: &[&str]) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    known_extensions
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
        transform: info.transform,
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
            transform: Transform::IDENTITY,
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
    fn only_avif_extensions_are_probed() {
        for path in ["a.avif", "b.AVIF"] {
            assert!(has_avif_extension(Path::new(path)), "{path}");
        }
        for path in ["a.heic", "b.png", "c", "d.avifs"] {
            assert!(!has_avif_extension(Path::new(path)), "{path}");
        }
    }

    /// Wraps `payload` in a box of `kind`.
    fn box_with(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut boxed = u32::try_from(payload.len() + 8)
            .expect("box size")
            .to_be_bytes()
            .to_vec();
        boxed.extend_from_slice(kind);
        boxed.extend_from_slice(payload);
        boxed
    }

    /// An `av1C` coding record with the given flags byte.
    fn av1c(flags: u8) -> Vec<u8> {
        box_with(b"av1C", &[0x81, 0x00, flags, 0x00])
    }

    /// An `ispe` box for an image of `width` by `height`.
    fn ispe(width: u32, height: u32) -> Vec<u8> {
        let mut payload = vec![0, 0, 0, 0];
        payload.extend_from_slice(&width.to_be_bytes());
        payload.extend_from_slice(&height.to_be_bytes());
        box_with(b"ispe", &payload)
    }

    /// A `colr` box of the given type.
    fn colr(kind: &[u8; 4]) -> Vec<u8> {
        box_with(b"colr", kind)
    }

    /// An `auxC` box naming `kind` as its auxiliary type.
    fn auxc(kind: &[u8]) -> Vec<u8> {
        let mut payload = vec![0, 0, 0, 0];
        payload.extend_from_slice(kind);
        box_with(b"auxC", &payload)
    }

    /// A file with `brand` as both its major and its only compatible brand, and
    /// `properties` as the item property list of its `meta` box.
    fn container(brand: &[u8; 4], properties: &[Vec<u8>]) -> Vec<u8> {
        let mut file_type = brand.to_vec();
        file_type.extend_from_slice(&[0, 0, 0, 0]);
        file_type.extend_from_slice(brand);

        let mut properties = properties.iter().flatten().copied().collect::<Vec<u8>>();
        properties = box_with(b"ipco", &properties);
        properties = box_with(b"iprp", &properties);
        let mut meta = vec![0, 0, 0, 0];
        meta.extend_from_slice(&properties);

        let mut boxes = box_with(b"ftyp", &file_type);
        boxes.extend_from_slice(&box_with(b"meta", &meta));
        boxes
    }

    #[test]
    fn a_monochrome_record_is_read_with_its_depth() {
        let boxes = container(b"avif", &[ispe(7, 5), av1c(0x10)]);
        let header = avif_header(&boxes).expect("8 bit monochrome");
        assert_eq!((header.width, header.height), (7, 5));
        assert_eq!(header.depth, 8);
        assert!(header.monochrome);
        assert!(
            header
                .format()
                .is_some_and(|format| format == PixelFormat::Gray8)
        );

        let boxes = container(b"avif", &[ispe(7, 5), av1c(0x50)]);
        let header = avif_header(&boxes).expect("10 bit monochrome");
        assert_eq!(header.depth, 10);
        assert_eq!(header.file_color_type(), ColorType::L16);
        assert!(
            header
                .format()
                .is_some_and(|format| format == PixelFormat::Gray16)
        );

        let boxes = container(b"avif", &[ispe(7, 5), av1c(0x70)]);
        assert_eq!(avif_header(&boxes).expect("12 bit monochrome").depth, 12);
    }

    #[test]
    fn a_colour_record_is_left_to_the_image_decoder() {
        let boxes = container(b"avif", &[ispe(3, 2), av1c(0x00)]);
        let header = avif_header(&boxes).expect("colour");
        assert!(!header.monochrome);
        assert_eq!(header.decoded_color_type(), ColorType::Rgba8);
        assert!(
            header
                .format()
                .is_some_and(|format| format == PixelFormat::Rgb8)
        );

        // A monochrome alpha item next to a colour image does not make it one.
        let boxes = container(b"avif", &[ispe(3, 2), av1c(0x00), av1c(0x10)]);
        let header = avif_header(&boxes).expect("colour with an alpha item");
        assert!(!header.monochrome);
        assert_eq!(header.format(), Some(PixelFormat::Rgb8));

        // The clip format of a monochrome image is gray whether or not the file
        // holds alpha: alpha is a clip of its own.
        let boxes = container(
            b"avif",
            &[
                ispe(3, 2),
                av1c(0x10),
                auxc(b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha"),
            ],
        );
        let header = avif_header(&boxes).expect("monochrome with alpha");
        assert!(header.alpha);
        assert_eq!(header.file_color_type(), ColorType::La8);
        assert_eq!(header.format(), Some(PixelFormat::Gray8));
    }

    #[test]
    fn an_alpha_item_is_recognized_by_either_aux_type() {
        for kind in [
            b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha".as_slice(),
            b"urn:mpeg:hevc:2015:auxid:1".as_slice(),
        ] {
            let boxes = container(b"avif", &[ispe(3, 2), av1c(0x10), auxc(kind)]);
            assert!(avif_header(&boxes).expect("monochrome with alpha").alpha);
        }
        // A depth map is an auxiliary item too, and is not alpha.
        let boxes = container(
            b"avif",
            &[
                ispe(3, 2),
                av1c(0x10),
                auxc(b"urn:mpeg:mpegB:cicp:systems:auxiliary:depth"),
            ],
        );
        assert!(!avif_header(&boxes).expect("monochrome").alpha);
    }

    #[test]
    fn metadata_is_read_from_the_properties() {
        for kind in [b"prof", b"rICC"] {
            let boxes = container(b"avif", &[ispe(3, 2), av1c(0x10), colr(kind)]);
            assert!(avif_header(&boxes).expect("an ICC profile").has_icc_profile);
        }
        // `nclx` names the colour primaries instead of holding a profile.
        let boxes = container(b"avif", &[ispe(3, 2), av1c(0x10), colr(b"nclx")]);
        assert!(!avif_header(&boxes).expect("no profile").has_icc_profile);
    }

    #[test]
    fn a_payload_is_not_read_as_a_box() {
        // A name that appears inside a payload is not a property, which is why
        // the property list is walked rather than searched.
        let decoy = av1c(0x10);
        let boxes = container(
            b"avif",
            &[ispe(3, 2), av1c(0x00), box_with(b"free", &decoy)],
        );
        let header = avif_header(&boxes).expect("colour");
        assert!(!header.monochrome, "the decoy record is not read");
        assert_eq!(header.depth, 8);

        // The same for a size that only looks like another `ispe`.
        let decoy = ispe(999, 999);
        let boxes = container(
            b"avif",
            &[ispe(3, 2), av1c(0x10), box_with(b"free", &decoy)],
        );
        let header = avif_header(&boxes).expect("monochrome");
        assert_eq!((header.width, header.height), (3, 2));
    }

    #[test]
    fn a_container_the_probe_will_not_describe_keeps_the_decoder() {
        let boxes = container(b"avif", &[ispe(3, 2)]);
        assert!(
            avif_header(&boxes).is_none(),
            "a file with no coding record"
        );

        let boxes = container(b"avif", &[av1c(0x10)]);
        assert!(avif_header(&boxes).is_none(), "a file with no size");
        assert!(avif_header(&container(b"avif", &[])).is_none());

        let boxes = container(b"avif", &[ispe(3, 2), ispe(9, 9), av1c(0x10)]);
        assert!(
            avif_header(&boxes).is_none(),
            "two sizes, only one of which can be the primary image"
        );

        let boxes = container(b"avif", &[ispe(3, 2), av1c(0x10), av1c(0x50)]);
        assert!(
            avif_header(&boxes).is_none(),
            "two depths, and the probe cannot tell which item holds which"
        );

        let boxes = container(b"avif", &[ispe(0, 2), av1c(0x10)]);
        assert!(avif_header(&boxes).is_none(), "a size of zero");

        for kind in [b"clap", b"irot", b"imir"] {
            let boxes = container(b"avif", &[ispe(3, 2), av1c(0x10), box_with(kind, &[0; 8])]);
            assert!(
                avif_header(&boxes).is_none(),
                "{} moves the picture the decoder reports",
                String::from_utf8_lossy(kind)
            );
        }
    }

    #[test]
    fn only_a_file_type_that_can_hold_av1_items_is_described() {
        for brand in [b"avif", b"avis"] {
            assert!(has_avif_brand(&container(brand, &[])), "{brand:?}");
        }
        // `mif1` files can hold av1 items, but `image` may route them through
        // the heif hook, whose color type this module cannot predict.
        for brand in [b"mif1", b"msf1", b"heic", b"jpeg"] {
            assert!(!has_avif_brand(&container(brand, &[])), "{brand:?}");
        }
        assert!(!has_avif_brand(&[]));
        assert!(!has_avif_brand(b"\0\0\0\x10ftyp"));
    }

    #[test]
    fn the_leading_boxes_stop_at_the_media_data() {
        let mut file = container(b"avif", &[ispe(3, 2), av1c(0x10)]);
        file.extend_from_slice(b"\0\0\0\x20mdat");
        file.extend_from_slice(&container(b"avif", &[ispe(9, 9), av1c(0x00)]));

        let boxes = read_boxes(file.as_slice()).expect("the leading boxes");
        assert!(
            find_bytes(&boxes, b"mdat").is_none(),
            "the media data is not read"
        );
        let header = avif_header(&boxes).expect("monochrome");
        assert_eq!((header.width, header.height), (3, 2));
        assert!(header.monochrome);
    }

    #[test]
    fn a_header_past_the_limit_is_refused() {
        let mut huge = vec![0; 8];
        huge[0..4].copy_from_slice(&(HEADER_LIMIT as u32 + 9).to_be_bytes());
        huge[4..8].copy_from_slice(b"free");
        assert!(read_boxes(huge.as_slice()).is_none());
        assert!(read_boxes([].as_slice()).is_none());
    }

    #[test]
    fn a_fixture_is_probed_from_its_container() {
        // The fixtures the validator uses; a missing one is not a failure here.
        let monochrome = PathBuf::from("tests/fixtures/mono-alpha.avif");
        if monochrome.is_file() {
            let info = image_info(&monochrome).expect("a monochrome avif");
            assert_eq!((info.width, info.height), (7, 5));
            assert_eq!(info.color_type, ColorType::Rgba8);
            assert_eq!(info.original_color_type, ExtendedColorType::La8);
            assert_eq!(info.format, PixelFormat::Gray8);
            assert_eq!(
                image_info(&monochrome).expect("again").format,
                PixelFormat::Gray8
            );
        }

        let colour = PathBuf::from("tests/fixtures/alpha-rgba8.avif");
        if colour.is_file() {
            let info = image_info(&colour).expect("a colour avif");
            assert_eq!((info.width, info.height), (3, 2));
            assert_eq!(info.format, PixelFormat::Rgb8);
            assert_eq!(info.original_color_type, ExtendedColorType::Rgba8);
        }

        // Another container is not described here, whatever it holds.
        assert!(image_info(Path::new("tests/fixtures/mono-alpha.heic")).is_none());
        assert!(image_info(Path::new("tests/fixtures/mono-alpha.png")).is_none());
    }

    #[test]
    fn a_file_the_probe_declines_keeps_its_format_correction() {
        // A file the probe does not describe is decoded by `image`, and its
        // format still has to be corrected, so the two readers of the leading
        // boxes have to agree about what the container says.
        let path = std::env::temp_dir().join(format!(
            "vs-imageseqs-probe-{}-{}.avif",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let mut boxes = container(b"mif1", &[ispe(7, 5), av1c(0x10)]);
        boxes.extend_from_slice(b"\0\0\0\x20mdat");
        std::fs::write(&path, &boxes).expect("write the container");

        assert!(
            image_info(&path).is_none(),
            "a brand the probe will not describe keeps the decoder"
        );
        assert_eq!(
            output_format(&path, ColorType::Rgba8),
            Some(PixelFormat::Gray8),
            "the format is still corrected"
        );

        std::fs::remove_file(&path).expect("remove the container");
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
        // An avif keeps the `image` decoder, whatever its container holds.
        assert!(!handles(&info("page.avif", ColorType::L8)));
        assert!(!handles(&info("page.avif", ColorType::Rgb8)));
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
