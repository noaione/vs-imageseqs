//! jpeg xl decoding through the `jxl` crate.
//!
//! The `image` crate has no jpeg xl decoder of its own. It used to be reached
//! through a hook `jxl-image-rs-integration` registered with it, which wrapped
//! the same `jxl` crate this module does and exposed a size, a color type, a
//! converted icc profile and pixels - and dropped everything else the codestream
//! states about itself. The dependency is the same one either way; what changes
//! is the layer in between, which is now this file.
//!
//! The fields the rest of the plugin wants are the codestream's own:
//! `basic_info()` carries the size, the bit depth, the orientation and the extra
//! channels, `embedded_color_profile()` the colour encoding the frame properties
//! are written from, and `set_pixel_format` is how a frame says which samples it
//! wants. Probing stays a header read: the decoder stops in the `WithImageInfo`
//! state before any pixel is decoded, which is where the adapter stopped too.
//!
//! **One of those fields is why this module exists.** A jpeg xl file states its
//! orientation the way an exif tag does, and the decoder renders the picture that
//! code describes: the raster it hands over is the *display* picture and the size
//! it reports is the display size. There is no way to ask for the stored picture
//! instead - `JxlDecoderOptions::adjust_orientation` is declared and read nowhere
//! in 0.7.4 - and the adapter did not implement `orientation()`, so a file that
//! states 6 was handed out the way 6 describes while `ImgSeqOrientation` said 1
//! and `apply_rotation=False` changed nothing. This module reports the code, and
//! turns rotation off by applying the code *inverted* to what the decoder
//! produced, which is the stored picture. See `docs/improvements/11-jxl-direct.md`.

use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use image::{ColorType, metadata::Orientation};
use jxl::{
    api::{
        Endianness, JxlBitDepth, JxlColorEncoding, JxlColorProfile, JxlColorType, JxlDataFormat,
        JxlDecoder, JxlDecoderOptions, JxlOutputBuffer, JxlPixelFormat, JxlPrimaries,
        JxlTransferFunction, JxlWhitePoint, ProcessingResult, states,
    },
    headers::{Orientation as JxlOrientation, extra_channels::ExtraChannel},
};

use crate::{
    color::{Cicp, UNSPECIFIED},
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    pixel::{PixelFormat, Transform, inverse_orientation},
};

/// File extension that holds a jpeg xl image.
const EXTENSION: &str = "jxl";

/// Signature of a bare jpeg xl codestream.
const CODESTREAM_SIGNATURE: [u8; 2] = [0xff, 0x0a];

/// Signature of the container a jpeg xl file with boxes, an exif block or an
/// animation starts with.
const CONTAINER_SIGNATURE: [u8; 12] = [
    0x00, 0x00, 0x00, 0x0c, b'J', b'X', b'L', b' ', 0x0d, 0x0a, 0x87, 0x0a,
];

/// Whether this module owns `path`.
///
/// The probe asks this before it opens an `image` decoder, because `image` has
/// no jpeg xl format for `ImageReader` to identify on its own: the extension and
/// the two signatures below are what the hook this module replaces used to
/// register with it.
pub fn owns(path: &Path) -> bool {
    has_jxl_extension(path)
}

/// Whether this module decodes `info`.
///
/// Every jpeg xl file goes through this module: the `image` integration has no
/// decoder to keep a color type back for.
pub fn handles(info: &ImageInfo) -> bool {
    has_jxl_extension(&info.path)
}

fn has_jxl_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(EXTENSION))
}

/// What the codestream of a jpeg xl file states about its image.
struct Header {
    /// Size the decoder hands the picture out as, which is the size the file
    /// stores with its own orientation already applied.
    width: u32,
    height: u32,
    /// The orientation the file states, as the code an exif tag carries.
    orientation: Orientation,
    /// Color type the decoded buffer is handed to the frame writer as.
    color_type: ColorType,
    /// The same layout as the `jxl` crate names it, which is what the decoder is
    /// asked for.
    jxl_color_type: JxlColorType,
    /// Sample format the decoder is asked for.
    data_format: JxlDataFormat,
    /// Bits per sample the codestream states, which is the depth the frame is
    /// built at: a ten bit file is handed out as `Gray10` or `Rgb10` holding the
    /// sample itself rather than as the word it arrives in. A float file states
    /// none and is as wide as its own sample, which narrows nothing.
    depth: u32,
    /// Extra channels the requested layout has to account for, alpha included.
    extra_channels: usize,
    /// Whether the file's embedded color profile is an icc profile rather than a
    /// set of primaries and a transfer function.
    ///
    /// This is the variant the codestream states, not `try_as_icc`, which
    /// answers for a file that states codes as well: the crate can generate a
    /// profile from an encoding, and `ImgSeqHasICC` is about a profile the file
    /// carries.
    has_icc_profile: bool,
    icc_profile: Option<Arc<[u8]>>,
    /// The colour description the codestream states with its codes, which is
    /// `None` for a file that carries an icc profile instead.
    cicp: Option<Cicp>,
}

impl Header {
    /// Reads everything the codestream states, from a decoder that has read the
    /// file header and nothing else.
    fn read(decoder: &JxlDecoder<states::WithImageInfo>, path: &Path) -> Result<Self> {
        let info = decoder.basic_info();
        let (width, height) = size_of(info.size, path)?;
        let extra_channels = info.extra_channels.len();
        let has_alpha = info
            .extra_channels
            .iter()
            .any(|channel| channel.ec_type == ExtraChannel::Alpha);
        // The layout the decoder itself has for the file is what says whether it
        // is grayscale; the color profile is not asked, because a file may state
        // one as an icc profile, whose color space this does not interpret.
        let grayscale = decoder.current_pixel_format().color_type.is_grayscale();
        let (color_type, jxl_color_type, data_format) =
            output_format(&info.bit_depth, grayscale, has_alpha);
        let depth = match &info.bit_depth {
            JxlBitDepth::Int { bits_per_sample } => *bits_per_sample,
            JxlBitDepth::Float { .. } => 32,
        };
        let profile = decoder.embedded_color_profile();
        Ok(Self {
            width,
            height,
            orientation: orientation_of(info.orientation),
            color_type,
            jxl_color_type,
            data_format,
            depth,
            extra_channels,
            has_icc_profile: matches!(profile, JxlColorProfile::Icc(_)),
            icc_profile: match profile {
                JxlColorProfile::Icc(bytes) => Some(Arc::from(bytes.as_slice())),
                JxlColorProfile::Simple(_) => None,
            },
            cicp: cicp_of(profile),
        })
    }

    /// Pixel format the frame holds.
    ///
    /// The color type is the word the samples are stored in, which knows eight
    /// bits, sixteen bits and float; the codestream's own depth is what the
    /// frame is built at, so a ten bit file is a ten bit frame whose samples the
    /// writer moves down out of the word. See [`PixelFormat::at_depth`].
    fn format(&self) -> Option<PixelFormat> {
        Some(PixelFormat::from_color_type(self.color_type)?.at_depth(self.depth))
    }

    /// Pixel format to ask the decoder for.
    fn pixel_format(&self) -> JxlPixelFormat {
        JxlPixelFormat {
            color_type: self.jxl_color_type,
            color_data_format: Some(self.data_format),
            // An extra channel is only read when it is alpha, and that one is
            // interleaved into the color buffer the requested layout describes.
            // Every other channel - depth, spot colour - is left out.
            extra_channel_format: vec![None; self.extra_channels],
        }
    }
}

/// The color type, the `jxl` layout and the sample format a file of this depth
/// is decoded as.
///
/// This is the same choice the `image` adapter made, so a file keeps the color
/// type and the sample size it had while that adapter decoded it: one byte per
/// sample up to 8 bits, two bytes for a deeper integer file, and `f32` for a
/// float one. A float file is decoded as rgb even when its color space is gray,
/// which is the adapter's choice as well; asking for `Gray32F` instead is a
/// change of what a frame holds and does not belong here.
fn output_format(
    bit_depth: &JxlBitDepth,
    grayscale: bool,
    has_alpha: bool,
) -> (ColorType, JxlColorType, JxlDataFormat) {
    match bit_depth {
        JxlBitDepth::Float { .. } => (
            if has_alpha {
                ColorType::Rgba32F
            } else {
                ColorType::Rgb32F
            },
            if has_alpha {
                JxlColorType::Rgba
            } else {
                JxlColorType::Rgb
            },
            JxlDataFormat::F32 {
                endianness: Endianness::native(),
            },
        ),
        JxlBitDepth::Int { bits_per_sample } if *bits_per_sample <= 8 => (
            match (grayscale, has_alpha) {
                (true, false) => ColorType::L8,
                (true, true) => ColorType::La8,
                (false, false) => ColorType::Rgb8,
                (false, true) => ColorType::Rgba8,
            },
            jxl_color_type(grayscale, has_alpha),
            JxlDataFormat::U8 { bit_depth: 8 },
        ),
        // 9 to 16 bits are asked for in 16 bit words, scaled onto the whole
        // range of the word. The frame is then built at the depth the codestream
        // states - see [`Header::format`], which narrows the color type with
        // [`PixelFormat::at_depth`] - and the writer moves each sample down out
        // of the word by the difference, which recovers it exactly however deep
        // the file is. Asking for `bit_depth: *bits_per_sample` instead would
        // hand the samples over already right aligned, at the price of a
        // narrower word that the writer would have to be told about.
        JxlBitDepth::Int { .. } => (
            match (grayscale, has_alpha) {
                (true, false) => ColorType::L16,
                (true, true) => ColorType::La16,
                (false, false) => ColorType::Rgb16,
                (false, true) => ColorType::Rgba16,
            },
            jxl_color_type(grayscale, has_alpha),
            JxlDataFormat::U16 {
                endianness: Endianness::native(),
                bit_depth: 16,
            },
        ),
    }
}

const fn jxl_color_type(grayscale: bool, has_alpha: bool) -> JxlColorType {
    match (grayscale, has_alpha) {
        (true, false) => JxlColorType::Grayscale,
        (true, true) => JxlColorType::GrayscaleAlpha,
        (false, false) => JxlColorType::Rgb,
        (false, true) => JxlColorType::Rgba,
    }
}

/// The colour description the codestream states, when it states it as codes.
///
/// A file whose embedded profile is an icc profile states no codes at all: its
/// colour is opaque to this module and `ImgSeqHasICC` is the whole story, which
/// is what `docs/improvements/08-color-metadata.md` decided. A file that states
/// codes is handed out as r,g,b whatever those codes are, so the matrix is not
/// one of them, and jpeg xl defines its samples as full range, so the flag is
/// always set.
fn cicp_of(profile: &JxlColorProfile) -> Option<Cicp> {
    let JxlColorProfile::Simple(encoding) = profile else {
        return None;
    };
    let (primaries, transfer) = match encoding {
        JxlColorEncoding::RgbColorSpace {
            white_point,
            primaries,
            transfer_function,
            ..
        } => (
            primaries_of(primaries, white_point),
            transfer_of(transfer_function),
        ),
        // A gray file states a transfer function and no primaries: the codes a
        // grayscale colour space is made of are the transfer function alone.
        JxlColorEncoding::GrayscaleColorSpace {
            transfer_function, ..
        } => (UNSPECIFIED, transfer_of(transfer_function)),
        // The colour space the codestream is coded in rather than the one it
        // describes; the file's own colour is in the profile this is read from,
        // which is not x,y,b for such a file either.
        JxlColorEncoding::XYB { .. } => (UNSPECIFIED, UNSPECIFIED),
    };
    Some(Cicp {
        primaries,
        transfer,
        matrix: UNSPECIFIED,
        full_range: true,
    })
}

/// The h.273 primaries code the codestream's primaries and white point state.
///
/// Each code names a set of chromaticities *and* the white point they are
/// defined against, so the two fields are read together: the sRGB primaries this
/// format calls `SRGB` are the ones code 1 names, the bt.2100 ones are code 9,
/// and the wide gamut `P3` primaries are code 11 with the dci white point and
/// code 12 with d65. Chromaticities of the file's own, or a white point that
/// disagrees with the code, state no code.
fn primaries_of(primaries: &JxlPrimaries, white_point: &JxlWhitePoint) -> u8 {
    let d65 = *white_point == JxlWhitePoint::D65;
    let dci = *white_point == JxlWhitePoint::DCI;
    match primaries {
        JxlPrimaries::SRGB if d65 => 1,
        JxlPrimaries::BT2100 if d65 => 9,
        JxlPrimaries::P3 if dci => 11,
        JxlPrimaries::P3 if d65 => 12,
        _ => UNSPECIFIED,
    }
}

/// The h.273 transfer code the codestream states.
///
/// The dci transfer function and a gamma of the file's own are the two the
/// codes VapourSynth has no property for name, so they state nothing here.
fn transfer_of(transfer: &JxlTransferFunction) -> u8 {
    match transfer {
        JxlTransferFunction::BT709 => 1,
        JxlTransferFunction::Linear => 8,
        JxlTransferFunction::SRGB => 13,
        JxlTransferFunction::PQ => 16,
        JxlTransferFunction::HLG => 18,
        _ => UNSPECIFIED,
    }
}

/// The orientation code of a jpeg xl file, as the `image` crate names the same
/// eight transformations.
///
/// The codestream numbers them exactly the way the exif tag does, so this is a
/// rename and not a conversion; a code the two disagree about would show up as a
/// rotated page, which is what the fixture in `tests/fixtures` is for.
const fn orientation_of(orientation: JxlOrientation) -> Orientation {
    match orientation {
        JxlOrientation::Identity => Orientation::NoTransforms,
        JxlOrientation::FlipHorizontal => Orientation::FlipHorizontal,
        JxlOrientation::Rotate180 => Orientation::Rotate180,
        JxlOrientation::FlipVertical => Orientation::FlipVertical,
        JxlOrientation::Transpose => Orientation::Rotate90FlipH,
        JxlOrientation::Rotate90Cw => Orientation::Rotate90,
        JxlOrientation::AntiTranspose => Orientation::Rotate270FlipH,
        JxlOrientation::Rotate90Ccw => Orientation::Rotate270,
    }
}

/// What the codestream of a jpeg xl states about its image, which is everything
/// the probe records and the decode is checked against.
///
/// Every jpeg xl file is this module's, so a file it cannot read is an error
/// rather than a fall back to the `image` decoder, which has none for the
/// format.
pub fn image_info(path: &Path, apply_rotation: bool) -> Result<ImageInfo> {
    let opened = open_header(path)?;
    let header = Header::read(&opened.decoder, path)?;
    Ok(ImageInfo {
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type: header.color_type,
        // The adapter this module replaces did not override the trait's own
        // answer, which is the color type the decoder hands out turned into the
        // extended one.
        original_color_type: header.color_type.into(),
        has_icc_profile: header.has_icc_profile,
        icc_profile: header.icc_profile.clone(),
        cicp: header.cicp,
        // A jpeg xl codestream states no chroma sample position: its yuv planes
        // are implied by its own upsampling filters.
        chroma_location: None,
        orientation: header.orientation,
        // The decoder has already handed the picture out the way the file
        // describes it, so the identity is what the caller asked for when
        // rotation is on; turning it off means undoing it.
        transform: if apply_rotation {
            Transform::IDENTITY
        } else {
            Transform::from_orientation(inverse_orientation(header.orientation))
        },
        format: header
            .format()
            .ok_or_else(|| unsupported(header.color_type, path))?,
    })
}

/// Decodes one jpeg xl image into the interleaved buffer its color type needs.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let mut opened = open_header(&info.path)?;
    let header = Header::read(&opened.decoder, &info.path)?;
    // The format is part of what the frame holds, so a file that changed its
    // depth - which a color type of the same width does not show - is caught
    // here as well.
    let format = header
        .format()
        .ok_or_else(|| unsupported(header.color_type, &info.path))?;
    if (header.width, header.height, header.color_type)
        != (info.width, info.height, info.color_type)
        || format != info.format
    {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {was_width}x{was_height} {was:?} {was_format}, now {now_width}x{now_height} {now:?} {now_format})",
            info.path.display(),
            was_width = info.width,
            was_height = info.height,
            was = info.color_type,
            was_format = info.format.name(),
            now_width = header.width,
            now_height = header.height,
            now = header.color_type,
            now_format = format.name(),
        )));
    }

    opened
        .decoder
        .set_pixel_format(header.pixel_format())
        .map_err(|error| decode_error(&info.path, error))?;
    let decoder = to_frame(opened.decoder, &mut opened.input, &info.path)?;

    let rows = usize::try_from(header.height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    let sample_bytes = header.data_format.bytes_per_sample();
    let row_bytes = usize::try_from(header.width)
        .ok()
        .and_then(|width| width.checked_mul(header.jxl_color_type.samples_per_pixel()))
        .and_then(|row| row.checked_mul(sample_bytes))
        .ok_or_else(|| ImgSeqError::new("image row is too large"))?;
    let size = row_bytes
        .checked_mul(rows)
        .ok_or_else(|| ImgSeqError::new("image is too large"))?;

    let buffer_started = Instant::now();
    let mut pixels = vec![0; size];
    let buffer = buffer_started.elapsed();

    let read_started = Instant::now();
    if pixels.as_ptr().align_offset(sample_bytes) == 0 {
        draw(
            decoder,
            &mut opened.input,
            &mut pixels,
            rows,
            row_bytes,
            &info.path,
        )?;
    } else {
        // The decoder writes two and four byte samples and wants every row to
        // start on a boundary that suits them, which the language does not
        // promise a vector of bytes; a buffer trimmed to one is drawn into and
        // copied back. No allocator this plugin builds against has ever taken
        // that path.
        let mut aligned = Aligned::new(size, sample_bytes);
        draw(
            decoder,
            &mut opened.input,
            aligned.bytes(),
            rows,
            row_bytes,
            &info.path,
        )?;
        pixels.copy_from_slice(aligned.bytes());
    }
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
            open: opened.open,
            metadata: opened.metadata,
            buffer,
            read,
        },
    })
}

/// A file whose header has been read, with its decoder waiting for the frame.
struct Opened {
    input: BufReader<File>,
    decoder: JxlDecoder<states::WithImageInfo>,
    /// Time the file handle took, which is the two probes below.
    open: Duration,
    /// Time the file header took, which is the size, the depth, the orientation
    /// and the color profile.
    metadata: Duration,
}

/// Opens `path` and reads its file header.
fn open_header(path: &Path) -> Result<Opened> {
    let open_started = Instant::now();
    let file = File::open(path).map_err(|error| image_error("open", path, error))?;
    // The decoder pulls the file in its own order, and reads ahead only as far
    // as the buffer holds, so the buffer stays the one `BufReader` comes with:
    // asking it how much input is left seeks to the end of the file and throws
    // the buffer away, and a large one would only make that cost more.
    let mut input = BufReader::new(file);
    check_signature(&mut input, path)?;
    let open = open_started.elapsed();

    let metadata_started = Instant::now();
    let decoder = JxlDecoder::<states::Initialized>::new(JxlDecoderOptions::default());
    let decoder = to_image_info(decoder, &mut input, path)?;
    let metadata = metadata_started.elapsed();

    Ok(Opened {
        input,
        decoder,
        open,
        metadata,
    })
}

/// Reads the two signatures a jpeg xl file starts with, without using the bytes
/// up: the decoder is handed the same reader below and needs the file from its
/// first byte.
fn check_signature(input: &mut BufReader<File>, path: &Path) -> Result<()> {
    let leading = input
        .fill_buf()
        .map_err(|error| image_error("read", path, error))?;
    if !leading.starts_with(&CONTAINER_SIGNATURE) && !leading.starts_with(&CODESTREAM_SIGNATURE) {
        return Err(image_error(
            "read",
            path,
            "the file starts with neither a jpeg xl codestream nor a jpeg xl container",
        ));
    }
    Ok(())
}

/// Reads the file header of a file whose decoder has just been created.
fn to_image_info(
    mut decoder: JxlDecoder<states::Initialized>,
    input: &mut BufReader<File>,
    path: &Path,
) -> Result<JxlDecoder<states::WithImageInfo>> {
    loop {
        match decoder
            .process(input, None)
            .map_err(|error| decode_error(path, error))?
        {
            ProcessingResult::Complete { result } => return Ok(result),
            ProcessingResult::NeedsMoreInput { fallback, .. } => {
                decoder = fallback;
                more_input(input, path)?;
            }
        }
    }
}

/// Reads the frame header of a file whose image info has just been read.
fn to_frame(
    mut decoder: JxlDecoder<states::WithImageInfo>,
    input: &mut BufReader<File>,
    path: &Path,
) -> Result<JxlDecoder<states::WithFrameInfo>> {
    loop {
        match decoder
            .process(input, None)
            .map_err(|error| decode_error(path, error))?
        {
            ProcessingResult::Complete { result } => return Ok(result),
            ProcessingResult::NeedsMoreInput { fallback, .. } => {
                decoder = fallback;
                more_input(input, path)?;
            }
        }
    }
}

/// Draws the pixels of a frame into `buffer`.
///
/// The decoder fills what it can from the input it has and asks for more, which
/// is why this is a loop: the frame can arrive in any number of pieces, and only
/// the last answer means the buffer holds the whole picture.
fn draw(
    mut decoder: JxlDecoder<states::WithFrameInfo>,
    input: &mut BufReader<File>,
    buffer: &mut [u8],
    rows: usize,
    row_bytes: usize,
    path: &Path,
) -> Result<()> {
    let mut output = JxlOutputBuffer::new(buffer, rows, row_bytes);
    loop {
        match decoder
            .process(&mut *input, std::slice::from_mut(&mut output), None)
            .map_err(|error| decode_error(path, error))?
        {
            ProcessingResult::Complete { .. } => return Ok(()),
            ProcessingResult::NeedsMoreInput { fallback, .. } => {
                decoder = fallback;
                more_input(input, path)?;
            }
        }
    }
}

/// Reports a file the decoder asked for more of but has none left to give.
fn more_input(input: &mut BufReader<File>, path: &Path) -> Result<()> {
    if input
        .fill_buf()
        .map_err(|error| image_error("read", path, error))?
        .is_empty()
    {
        return Err(image_error(
            "decode",
            path,
            "the file ended before the picture did",
        ));
    }
    Ok(())
}

/// Space for the samples of one image, aligned for samples of `alignment` bytes.
///
/// [`JxlOutputBuffer`] is handed a slice of bytes, and the decoder writes whole
/// samples through it: a row has to start where a two or four byte sample can be
/// written. A byte vector is aligned for whatever the allocator chose, so the
/// rare one that is not gets a buffer with room to spare and the front of it
/// trimmed to the first sample boundary.
struct Aligned {
    storage: Vec<u8>,
    start: usize,
    len: usize,
}

impl Aligned {
    fn new(len: usize, alignment: usize) -> Self {
        let storage = vec![0; len + alignment];
        let start = (alignment - storage.as_ptr() as usize % alignment) % alignment;
        Self {
            storage,
            start,
            len,
        }
    }

    fn bytes(&mut self) -> &mut [u8] {
        let start = self.start;
        &mut self.storage[start..start + self.len]
    }
}

/// The size the file header states, as the size a frame can hold.
fn size_of(size: (usize, usize), path: &Path) -> Result<(u32, u32)> {
    let width = u32::try_from(size.0);
    let height = u32::try_from(size.1);
    match (width, height) {
        (Ok(width), Ok(height)) => Ok((width, height)),
        _ => Err(ImgSeqError::new(format!(
            "image '{}' is {}x{}, which is larger than a frame can hold",
            path.display(),
            size.0,
            size.1,
        ))),
    }
}

fn decode_error(path: &Path, error: jxl::error::Error) -> ImgSeqError {
    image_error("decode", path, error)
}

fn unsupported(color_type: ColorType, path: &Path) -> ImgSeqError {
    image_error(
        "decode",
        path,
        format!("a jpeg xl image of color type {color_type:?} is not supported"),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::decoder::orientation_size;

    /// The 4x3 gray fixture that states orientation 6, whose samples are
    /// `row * 4 + column + 1` in the stored picture.
    const ORIENTED: &[u8] = include_bytes!("../../tests/fixtures/orientation-6.jxl");

    /// The 3x2 rgba fixture, which states no orientation.
    const ALPHA: &[u8] = include_bytes!("../../tests/fixtures/alpha-rgba8.jxl");

    /// The same picture as [`ORIENTED`], handed out the way the code describes
    /// it: a quarter turn clockwise, so the stored bottom row is the display
    /// first column, read from the bottom.
    const SHOWN: [u8; 12] = [9, 5, 1, 10, 6, 2, 11, 7, 3, 12, 8, 4];

    /// The same picture as [`ALPHA`]: six rgb pixels, each with its alpha.
    const ALPHA_BUFFER: [u8; 24] = [
        1, 2, 3, 9, 4, 5, 6, 8, 7, 8, 9, 7, 10, 11, 12, 6, 13, 14, 15, 5, 16, 17, 18, 4,
    ];

    /// Writes bytes to a temp file named for this test process.
    fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("imgseqs-jxl-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).expect("a writable image");
        path
    }

    /// The interleaved buffer of a decode, which is what the frame writer reads.
    fn buffer(decoded: &DecodedImage) -> &[u8] {
        match &decoded.pixels {
            Pixels::Interleaved { buffer, .. } => buffer,
            other => panic!("a jpeg xl decodes into an interleaved buffer, got {other:?}"),
        }
    }

    #[test]
    fn only_jxl_extensions_are_taken_over() {
        assert!(has_jxl_extension(Path::new("a.jxl")));
        assert!(has_jxl_extension(Path::new("a.JXL")));
        assert!(!has_jxl_extension(Path::new("a.jxls")));
        assert!(!has_jxl_extension(Path::new("jxl")));
        assert!(!has_jxl_extension(Path::new("a.png")));
    }

    #[test]
    fn every_code_keeps_the_number_the_exif_tag_uses() {
        // The codestream numbers the eight transformations the way an exif tag
        // does, so the mapping has to agree with `image`'s own code table; a
        // disagreement would show up as a page turned the wrong way.
        for code in 1..=8u8 {
            let exif = Orientation::from_exif(code).expect("a known exif code");
            assert_eq!(orientation_of(jxl_orientation(code)), exif, "code {code}");
        }
    }

    /// The codestream's own enum value of `code`, by the name of the
    /// transformation it describes.
    fn jxl_orientation(code: u8) -> JxlOrientation {
        match code {
            1 => JxlOrientation::Identity,
            2 => JxlOrientation::FlipHorizontal,
            3 => JxlOrientation::Rotate180,
            4 => JxlOrientation::FlipVertical,
            5 => JxlOrientation::Transpose,
            6 => JxlOrientation::Rotate90Cw,
            7 => JxlOrientation::AntiTranspose,
            _ => JxlOrientation::Rotate90Ccw,
        }
    }

    #[test]
    fn an_oriented_file_states_its_code_and_hands_out_the_display_picture() {
        let path = write_temp("oriented.jxl", ORIENTED);
        let info = image_info(&path, true).expect("the fixture to probe");
        assert_eq!((info.width, info.height), (3, 4), "the display size");
        assert_eq!(info.orientation, Orientation::Rotate90, "the code");
        assert_eq!(info.format, PixelFormat::Gray8);
        assert_eq!(
            orientation_size(info.transform, info.width, info.height),
            (3, 4)
        );
        // The decoder applied the code itself, so the writer rearranges nothing.
        assert_eq!(info.transform, Transform::IDENTITY);

        let decoded = decode(&info).expect("the fixture to decode");
        assert_eq!((decoded.width, decoded.height), (3, 4));
        assert_eq!(buffer(&decoded), SHOWN);

        // Turning rotation off undoes it instead, and the frame is the stored
        // picture: same code, the other size, the other rearrange.
        let stored = image_info(&path, false).expect("the fixture to probe");
        assert_eq!(stored.orientation, Orientation::Rotate90);
        assert_eq!(
            (stored.width, stored.height),
            (3, 4),
            "the decoder's raster"
        );
        assert_eq!(
            stored.transform,
            Transform::from_orientation(Orientation::Rotate270)
        );
        assert_eq!(
            orientation_size(stored.transform, stored.width, stored.height),
            (4, 3),
            "the stored size"
        );
        let decoded = decode(&stored).expect("the fixture to decode");
        assert_eq!(
            buffer(&decoded),
            SHOWN,
            "the decoder still holds the display"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_alpha_channel_comes_through_the_color_buffer() {
        let path = write_temp("alpha.jxl", ALPHA);
        let info = image_info(&path, true).expect("the fixture to probe");
        assert_eq!((info.width, info.height), (3, 2));
        assert_eq!(info.color_type, ColorType::Rgba8);
        assert_eq!(info.orientation, Orientation::NoTransforms);
        assert_eq!(info.transform, Transform::IDENTITY);

        let decoded = decode(&info).expect("the fixture to decode");
        assert_eq!(buffer(&decoded), ALPHA_BUFFER);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_that_is_not_jpeg_xl_is_refused() {
        let path = write_temp("not-jxl.jxl", b"not a jpeg xl image at all");
        let error = image_info(&path, true).expect_err("a file without a signature");
        assert!(error.to_string().contains("jpeg xl"), "{error}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_truncated_file_is_reported() {
        let path = write_temp("truncated.jxl", &ORIENTED[..40]);
        let error = image_info(&path, true).expect_err("a file that ends early");
        assert!(error.to_string().contains("ended before"), "{error}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_layouts_the_depths_pick() {
        let cases = [
            (
                JxlBitDepth::Int { bits_per_sample: 8 },
                false,
                false,
                ColorType::Rgb8,
            ),
            (
                JxlBitDepth::Int { bits_per_sample: 8 },
                true,
                true,
                ColorType::La8,
            ),
            (
                JxlBitDepth::Int {
                    bits_per_sample: 10,
                },
                false,
                false,
                ColorType::Rgb16,
            ),
            (
                JxlBitDepth::Int {
                    bits_per_sample: 12,
                },
                true,
                false,
                ColorType::L16,
            ),
            (
                JxlBitDepth::Int {
                    bits_per_sample: 16,
                },
                false,
                true,
                ColorType::Rgba16,
            ),
            (
                JxlBitDepth::Float {
                    bits_per_sample: 16,
                    exponent_bits_per_sample: 5,
                },
                false,
                false,
                ColorType::Rgb32F,
            ),
        ];
        for (depth, grayscale, alpha, expected) in cases {
            let (color_type, _, format) = output_format(&depth, grayscale, alpha);
            assert_eq!(color_type, expected, "{depth:?} {grayscale} {alpha}");
            assert!(format.bytes_per_sample() <= 4);
        }
    }

    #[test]
    fn an_aligned_buffer_trims_to_the_sample_size() {
        for alignment in [1, 2, 4] {
            let mut aligned = Aligned::new(24, alignment);
            let bytes = aligned.bytes();
            assert_eq!(bytes.len(), 24);
            assert_eq!(bytes.as_ptr() as usize % alignment, 0, "{alignment}");
        }
    }

    #[test]
    fn the_primaries_code_pairs_a_white_point_with_its_chromaticities() {
        use JxlPrimaries as P;
        use JxlWhitePoint as W;
        let own_chromaticities = P::Chromaticities {
            rx: 0.64,
            ry: 0.33,
            gx: 0.3,
            gy: 0.6,
            bx: 0.15,
            by: 0.06,
        };
        let cases = [
            (P::SRGB, W::D65, 1),
            (P::BT2100, W::D65, 9),
            (P::P3, W::DCI, 11),
            (P::P3, W::D65, 12),
            // A white point the code does not name, and chromaticities the file
            // states of its own, name no code.
            (P::SRGB, W::DCI, UNSPECIFIED),
            (P::SRGB, W::E, UNSPECIFIED),
            (P::BT2100, W::E, UNSPECIFIED),
            (P::P3, W::E, UNSPECIFIED),
            (own_chromaticities, W::D65, UNSPECIFIED),
        ];
        for (primaries, white_point, expected) in cases {
            assert_eq!(
                primaries_of(&primaries, &white_point),
                expected,
                "{primaries:?} with {white_point:?}"
            );
        }
    }

    #[test]
    fn the_transfer_codes_are_the_ones_vapoursynth_has() {
        use JxlTransferFunction as T;
        let cases = [
            (T::BT709, 1),
            (T::Linear, 8),
            (T::SRGB, 13),
            (T::PQ, 16),
            (T::HLG, 18),
            // The dci transfer function is code point 17, which has no property,
            // and a gamma of the file's own names no code point at all.
            (T::DCI, UNSPECIFIED),
            (T::Gamma(2.2), UNSPECIFIED),
        ];
        for (transfer, expected) in cases {
            assert_eq!(transfer_of(&transfer), expected, "{transfer:?}");
        }
    }

    #[test]
    fn a_file_that_carries_an_icc_profile_states_no_codes() {
        assert!(cicp_of(&JxlColorProfile::Icc(vec![0; 128])).is_none());
    }

    #[test]
    fn a_codestream_that_states_codes_states_them() {
        // Both jxl fixtures state the sRGB primaries with the sRGB transfer
        // function, which is primaries 1 and transfer 13, and neither carries an
        // icc profile: a codestream states one or the other, not both.
        let color =
            image_info(Path::new("tests/fixtures/alpha-rgba8.jxl"), true).expect("the fixture");
        assert_eq!(
            color.cicp,
            Some(Cicp {
                primaries: 1,
                transfer: 13,
                matrix: UNSPECIFIED,
                full_range: true
            })
        );
        assert!(!color.has_icc_profile);

        // A gray colour space is a transfer function and a white point, so it
        // has no primaries to state.
        let gray =
            image_info(Path::new("tests/fixtures/orientation-6.jxl"), true).expect("the fixture");
        assert_eq!(
            gray.cicp,
            Some(Cicp {
                primaries: UNSPECIFIED,
                transfer: 13,
                matrix: UNSPECIFIED,
                full_range: true
            })
        );
        assert!(!gray.has_icc_profile);
    }
}
