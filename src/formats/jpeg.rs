//! One jpeg, read by a direct decoder instead of by the `image` crate.
//!
//! `image`'s jpeg reader reads the whole compressed file to identify it and then
//! again for each of the three questions a probe asks — dimensions, colour type
//! and orientation — because each accessor builds a header decoder of its own,
//! and it then builds a fourth for the pixels. zune-jpeg decodes the headers
//! once and answers all of them from that one pass, which is what this module
//! does: one `decode_headers` for every fact a probe needs, and one
//! `decode_into` for the picture.
//!
//! A probe reads that pass from a *stream* and stops where the pixels begin, so
//! creating a clip over the 35 page corpus reads a few hundred kilobytes of
//! headers instead of 213 MB of pictures; it went from 105 ms to 3 ms. The
//! picture itself is decoded from the file read whole, because a decoder that
//! seeks around a small buffer is measurably slower than one over a slice, and
//! the compressed bytes are a fraction of the frame they produce.
//!
//! The samples are the crate's, and they are the same samples: on the 35 page
//! corpus of `sandbox/jpeg` this reader is byte-for-byte identical to the
//! `image` path it replaces, for the 31 monochrome pages and the four colour
//! ones. That comparison is recorded in
//! `docs/improvements/27-direct-still-decoders.md`.
//!
//! The two facts `image` supplied that zune-jpeg does not are read here:
//! the ICC profile from the `APP2` segments the crate stitches back together,
//! and the exif orientation from the tag in the `APP1` payload the crate hands
//! over at the TIFF header.

use std::{fs::File, io::BufReader, path::Path, time::Instant};

use zune_core::{
    bytestream::ZByteReaderTrait, bytestream::ZCursor, colorspace::ColorSpace,
    options::DecoderOptions,
};
use zune_jpeg::JpegDecoder;

/// The stream one jpeg is read from.
///
/// A buffered file rather than a slice, because a probe has no business
/// holding a whole page in memory to answer four questions about it: the crate
/// reads exactly the bytes the headers occupy and stops. `image`'s reader did
/// the same, so this keeps the memory the old path used rather than paying for
/// the convenience of having the bytes to hand.
type Stream = BufReader<File>;

use crate::{
    color::Cicp,
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    exif::orientation_of,
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    // Content first: a file whose bytes say it is something else is that
    // something else however it is named, and the extension is the hint a
    // format with no signature of its own has to fall back on. See
    // [`identify::owns`](crate::formats::identify::owns).
    crate::formats::identify::owns(crate::formats::identify::Format::Jpeg, path)
}

/// What the headers of one jpeg state about it.
struct Header {
    width: u32,
    height: u32,
    /// The layout a decode of this file produces.
    color_type: ColorType,
    /// The encoding the file holds, which for a jpeg is its own colour space.
    source: SourceColorType,
    /// The output format the colour clip is written as.
    format: PixelFormat,
    /// The colour space to ask the decoder for, which follows the source
    /// because the buffer and the layout have to agree on the channel count.
    out_colorspace: ColorSpace,
    icc_profile: Option<Vec<u8>>,
    orientation: Orientation,
    has_icc_profile: bool,
}

/// Reads a jpeg's headers and everything they state.
///
/// The options are the ones `image`'s reader used effectively: no strict mode,
/// and no dimension limit, because a probe that refuses a large page would turn
/// a readable file into a failure the old build did not report.
fn options() -> DecoderOptions {
    DecoderOptions::default()
        .set_strict_mode(false)
        .set_max_width(usize::MAX)
        .set_max_height(usize::MAX)
}

/// Reads the headers of `path` from a stream, which is one pass to where the
/// pixels begin.
fn read_headers<T: ZByteReaderTrait>(stream: T, path: &Path) -> Result<JpegDecoder<T>> {
    let mut decoder = JpegDecoder::new_with_options(stream, options());
    decoder
        .decode_headers()
        .map_err(|error| image_error("read the headers of", path, error))?;
    Ok(decoder)
}

/// Opens `path` as a stream, for a probe that reads only the headers.
///
/// Nothing but the header bytes is read, because a probe answers four questions
/// about a file and has no reason to hold the page in memory to do it. The
/// stream is what makes that true: the crate stops reading where the pixels
/// begin.
fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|error| image_error("open", path, error))
}

fn open_stream(path: &Path) -> Result<Stream> {
    let file = File::open(path).map_err(|error| image_error("open", path, error))?;
    Ok(BufReader::new(file))
}

/// Probes `path` without decoding its picture.
///
/// Returns `None` for a file this module does not read, which is what leaves a
/// path of another extension to whatever format its bytes name.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or is not a jpeg this
/// decoder can make sense of.
pub fn image_info(
    path: &Path,
    apply_rotation: bool,
    route: Option<crate::formats::identify::Format>,
) -> Result<Option<ImageInfo>> {
    if !route.map_or_else(
        || owns(path),
        |saved| saved == crate::formats::identify::Format::Jpeg,
    ) {
        return Ok(None);
    }
    let decoder = read_headers(open_stream(path)?, path)?;
    let header = header(&decoder, path)?;
    Ok(Some(info(path, &header, apply_rotation)))
}

/// The layout a jpeg's colour space produces, and the colour space to ask the
/// decoder for.
///
/// Two things are named rather than one, and the pair is the point: a jpeg
/// states one of four colour spaces and `image` reports the same four, so the
/// *requested* output has to follow the *input* or the buffer and the layout
/// disagree. A monochrome page asked for as rgb decodes to three channels and
/// the frame writer, told the picture is `L8`, then refuses the buffer -- which
/// is exactly what this did before the pair was separated.
///
/// A cmyk or ycck page is the one case where the two differ: its source label
/// is `Cmyk8` but its pixels are r,g,b, because both decoders convert it.
const fn output_format(
    source: ColorSpace,
) -> (PixelFormat, ColorType, SourceColorType, ColorSpace) {
    match source {
        ColorSpace::Luma | ColorSpace::LumaA => (
            PixelFormat::Gray8,
            ColorType::L8,
            SourceColorType::L8,
            ColorSpace::Luma,
        ),
        ColorSpace::CMYK | ColorSpace::YCCK => (
            PixelFormat::Rgb8,
            ColorType::Rgb8,
            SourceColorType::Cmyk8,
            ColorSpace::RGB,
        ),
        // YCbCr, RGB, and anything else the crate can name are handed out as
        // r,g,b, which is what the `image` path did for all of them.
        _ => (
            PixelFormat::Rgb8,
            ColorType::Rgb8,
            SourceColorType::Rgb8,
            ColorSpace::RGB,
        ),
    }
}

/// Everything a probe reports about one jpeg, from headers that were read.
fn header<T: ZByteReaderTrait>(decoder: &JpegDecoder<T>, path: &Path) -> Result<Header> {
    let (width, height) = decoder
        .dimensions()
        .ok_or_else(|| image_error("read the size of", path, "the headers state no dimensions"))?;
    let (width, height) = (
        u32::try_from(width).map_err(|_| too_wide(path))?,
        u32::try_from(height).map_err(|_| too_wide(path))?,
    );
    let source = decoder.input_colorspace().unwrap_or(ColorSpace::RGB);
    let (format, color_type, label, out_colorspace) = output_format(source);
    let icc_profile = decoder.icc_profile();
    let orientation = decoder
        .exif()
        .and_then(|exif| orientation_of(exif))
        .unwrap_or(Orientation::NoTransforms);

    Ok(Header {
        width,
        height,
        color_type,
        source: label,
        format,
        out_colorspace,
        has_icc_profile: icc_profile.is_some(),
        icc_profile,
        orientation,
    })
}

fn too_wide(path: &Path) -> ImgSeqError {
    image_error("read the size of", path, "it does not fit in 32 bits")
}

/// The [`ImageInfo`] one probed header describes.
fn info(path: &Path, header: &Header, apply_rotation: bool) -> ImageInfo {
    ImageInfo {
        route: None,
        subimage: None,
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type: header.color_type,
        original_color_type: header.source,
        has_icc_profile: header.has_icc_profile,
        icc_profile: header
            .icc_profile
            .as_ref()
            .map(|bytes| std::sync::Arc::from(bytes.as_slice())),
        // A jpeg states no colour description of its own: its `APP2` payload is
        // an ICC profile, and a file without one says nothing rather than
        // saying "unspecified".
        cicp: None::<Cicp>,
        // The chroma sample position is not stated by a jpeg at all.
        chroma_location: None,
        orientation: header.orientation,
        transform: if apply_rotation {
            Transform::from_orientation(header.orientation)
        } else {
            Transform::IDENTITY
        },
        format: header.format,
    }
}

/// Decodes the picture of a probed jpeg.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read, no longer matches what
/// it probed as, or cannot be decoded.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = Instant::now();
    let mut decoder = read_headers(ZCursor::new(read_file(&info.path)?), &info.path)?;
    let open = open_started.elapsed();

    let metadata_started = Instant::now();
    let header = header(&decoder, &info.path)?;
    if (
        header.width,
        header.height,
        header.color_type,
        header.format,
    ) != (info.width, info.height, info.color_type, info.format)
    {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {}x{} {:?} {}, now {}x{} {:?} {})",
            info.path.display(),
            info.width,
            info.height,
            info.color_type,
            info.format.name(),
            header.width,
            header.height,
            header.color_type,
            header.format.name(),
        )));
    }
    let metadata = metadata_started.elapsed();

    let layout_started = Instant::now();
    // The requested output has to match the layout the probe recorded, because
    // the two decide different things: the layout says how many channels the
    // writer will read and this says how many the decoder writes.
    decoder.set_options(
        decoder
            .options()
            .jpeg_set_out_colorspace(header.out_colorspace),
    );
    let size = decoder.output_buffer_size().ok_or_else(|| {
        image_error(
            "decode",
            &info.path,
            "the picture is too large for this platform",
        )
    })?;
    let mut pixels = vec![0; size];
    let allocate = layout_started.elapsed();

    let read_started = Instant::now();
    decoder
        .decode_into(&mut pixels)
        .map_err(|error| image_error("decode", &info.path, error))?;
    let read = read_started.elapsed();

    Ok(DecodedImage {
        width: header.width,
        height: header.height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Interleaved {
            color_type: header.color_type,
            buffer: pixels,
        },
        timings: DecodeTimings {
            open,
            metadata,
            buffer: allocate,
            read,
        },
    })
}
