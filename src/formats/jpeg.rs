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
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// File extensions that hold a jpeg.
const EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "jfif"];

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
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
/// path of another extension to the generic still path.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or is not a jpeg this
/// decoder can make sense of.
pub fn image_info(path: &Path, apply_rotation: bool) -> Result<Option<ImageInfo>> {
    if !owns(path) {
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

/// The orientation an exif payload states, for a payload that states one.
///
/// The payload is the TIFF header and its first IFD, which is what the `APP1`
/// segment holds once its `Exif\0\0` heading is removed. A tag whose value is
/// not one of the eight codes, a payload that is truncated, and an endianness
/// marker that is neither `II` nor `MM` all state no orientation rather than a
/// wrong one, which is what the `image` reader did with them too.
fn orientation_of(exif: &[u8]) -> Option<Orientation> {
    let reader = Tiff::new(exif)?;
    let ifd = usize::try_from(reader.u32(4)?).ok()?;
    let count = reader.u16(ifd)?;
    for index in 0..count {
        // Twelve bytes per entry: tag, type, count, then the value or its
        // offset. The orientation is a `SHORT` of one element, so its value is
        // in the first two bytes of the value field.
        // The count is the first two bytes of the IFD and the entries follow it,
        // so entry `index` starts at `ifd + 2 + index * 12`.
        let entry = ifd
            .checked_add(2)?
            .checked_add(usize::from(index).checked_mul(12)?)?;
        if reader.u16(entry) != Some(0x0112) {
            continue;
        }
        let value = reader.u16(entry.checked_add(8)?)?;
        return Orientation::from_exif(u8::try_from(value).ok()?);
    }
    None
}

/// A TIFF header, read with the endianness it states.
///
/// Both byte orders are read, because an exif payload states which one it uses
/// and a jpeg from either kind of camera has to be read the same way.
struct Tiff<'a> {
    bytes: &'a [u8],
    big_endian: bool,
}

impl<'a> Tiff<'a> {
    fn new(bytes: &'a [u8]) -> Option<Self> {
        let order = bytes.get(..2)?;
        let big_endian = match order {
            b"MM" => true,
            b"II" => false,
            _ => return None,
        };
        // The magic number is in the order the file states, so it is checked
        // one way or the other rather than literally.
        let magic = bytes.get(2..4)?;
        let expected = if big_endian {
            [0x00, 0x2a]
        } else {
            [0x2a, 0x00]
        };
        if magic != expected {
            return None;
        }
        Some(Self { bytes, big_endian })
    }

    fn u16(&self, at: usize) -> Option<u16> {
        let pair = self.bytes.get(at..at.checked_add(2)?)?;
        let pair: [u8; 2] = pair.try_into().ok()?;
        Some(if self.big_endian {
            u16::from_be_bytes(pair)
        } else {
            u16::from_le_bytes(pair)
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let quad = self.bytes.get(at..at.checked_add(4)?)?;
        let quad: [u8; 4] = quad.try_into().ok()?;
        Some(if self.big_endian {
            u32::from_be_bytes(quad)
        } else {
            u32::from_le_bytes(quad)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Tiff, orientation_of};
    use crate::layout::Orientation;

    /// A tiff header and one IFD entry stating `value`, in `order`.
    fn exif(order: &[u8; 2], value: u16) -> Vec<u8> {
        let big = order == b"MM";
        let mut bytes = order.to_vec();
        bytes.extend_from_slice(if big { &[0x00, 0x2a] } else { &[0x2a, 0x00] });
        // Offset of the first IFD, and its single entry.
        bytes.extend_from_slice(if big { &[0, 0, 0, 8] } else { &[8, 0, 0, 0] });
        bytes.extend_from_slice(if big { &[0, 1] } else { &[1, 0] });
        // Tag 0x0112, type SHORT, count 1, then the value in the first two bytes.
        bytes.extend_from_slice(if big { &[0x01, 0x12] } else { &[0x12, 0x01] });
        bytes.extend_from_slice(if big { &[0, 3] } else { &[3, 0] });
        bytes.extend_from_slice(if big { &[0, 0, 0, 1] } else { &[1, 0, 0, 0] });
        let encoded = if big {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        bytes.extend_from_slice(&encoded);
        bytes.extend_from_slice(&[0, 0]);
        // No next IFD. Zero is zero in either order.
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes
    }

    /// Both byte orders are read, and every exif code round trips.
    #[test]
    fn an_exif_tag_reads_in_either_byte_order() {
        for order in [b"II", b"MM"] {
            for code in 1..=8u8 {
                let bytes = exif(order, u16::from(code));
                assert_eq!(
                    orientation_of(&bytes).expect("a known code"),
                    Orientation::from_exif(code).expect("a known code"),
                    "{order:?} code {code}"
                );
            }
        }
    }

    /// A payload that states no orientation, or states one this build does not
    /// name, is not a wrong orientation.
    #[test]
    fn an_unusable_exif_payload_states_no_orientation() {
        assert_eq!(orientation_of(&[]), None);
        assert_eq!(orientation_of(b"XX\x2a\x00"), None);
        assert_eq!(orientation_of(b"II\x00\x00"), None);
        // A code outside the eight, and a code of zero.
        assert_eq!(orientation_of(&exif(b"II", 0)), None);
        assert_eq!(orientation_of(&exif(b"II", 9)), None);
        // A truncated entry: the value field is cut off.
        let mut short = exif(b"II", 6);
        short.truncate(18);
        assert_eq!(orientation_of(&short), None);
    }

    /// A payload whose first entry is another tag still finds the orientation
    /// when it is the second one, which is what a real `APP1` looks like: the
    /// orientation tag is written wherever the camera put it, not first.
    #[test]
    fn a_later_entry_states_the_orientation() {
        // Little endian, two entries, the second one the orientation.
        let mut bytes = vec![b'I', b'I', 0x2a, 0x00, 8, 0, 0, 0];
        bytes.extend_from_slice(&[2, 0]);
        // Entry one: an unrelated tag whose value is an offset, not a code.
        bytes.extend_from_slice(&[0x0f, 0x01, 3, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        // Entry two: tag 0x0112, SHORT, one element, value 6.
        bytes.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
        // No next IFD.
        bytes.extend_from_slice(&[0, 0, 0, 0]);

        let found = orientation_of(&bytes).expect("the second entry states it");
        assert_eq!(found, Orientation::Rotate90);
    }

    /// A tiff header this build cannot read states nothing.
    #[test]
    fn a_header_that_is_not_tiff_is_refused() {
        assert!(Tiff::new(b"").is_none());
        assert!(Tiff::new(b"II").is_none());
        assert!(Tiff::new(b"II\x2b\x00\x08\x00\x00\x00").is_none());
        assert!(Tiff::new(b"II\x2a\x00\x08\x00\x00\x00").is_some());
    }
}
