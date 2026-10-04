//! The `image` crate's still decoder, behind this plugin's own types.
//!
//! Every remaining use of the `image` crate in production code is in this
//! module, and it exists so that removing the crate later is a change to one
//! file rather than to the decoder, the frame writer and every format adapter.
//! A caller here never names an `image` type: [`Metadata`] and [`ColorType`] are
//! the plugin's own, and the crate's enums are translated at this boundary.
//!
//! What still reaches it, and what would have to exist before it could go:
//!
//! - **gif** — a one frame gif. `probe_segment` asks the animation adapter
//!   first and `animation::gif` declines anything under two frames, so a still
//!   gif falls through to here and is read by the crate. The feature is `gif`.
//!   **This is the one that blocks dropping the crate**: there is no
//!   `formats/gif.rs`, and no single frame gif fixture either, so writing one
//!   starts by adding the fixture to verify it against.
//! - **png** — a png whose rows [`crate::formats::png`] will not walk into the
//!   frame, which it hands on rather than failing. The feature is `png`.
//! - **webp** — the probe of every webp, because the header read is the
//!   crate's, and a renamed animated webp, whose simple libwebp entry points
//!   refuse a container of frames. The feature is `webp`.
//! - **avif** — one [`crate::formats::avif`] will not describe from its own
//!   boxes. The feature is `avif-native`, which is the crate's own av1 reading:
//!   `dav1d`, the same decoder [`crate::formats::avif`] calls directly, plus
//!   `mp4parse` for the boxes. It is a second adapter in front of one decoder,
//!   the way `jpeg` was before that feature went.
//! - **a file no format names** — [`crate::formats::identify::route`] answers
//!   `None`, which is the last resort by design.
//!
//! `rayon` is the crate's own parallel decoding for the formats above. `jpeg`
//! is deliberately not one of its features; see `Cargo.toml`.
//!
//! The format is chosen here rather than left to the crate, which is how the
//! old `ImageReader::with_guessed_format` behaved and how it stays: a signature
//! read from the file's first bytes wins and the extension is the fallback, so
//! a file whose name lies about it decodes as what its bytes are. The header is
//! read from its own handle, so the decoder that reads the pixels still starts
//! at the beginning of the file.
//!
//! A decoder is built when it is read from rather than held open. The crate's
//! `into_decoder` hands back a type that borrows the reader it was built from,
//! and what a caller here holds is the reader and the format that was already
//! decided, so the decoder is built once for the probe and once for the read.
//! This is the same shape the plugin had before: the probe asked a decoder for
//! dimensions and dropped it, and the read built another.
//!
//! [`ColorType`]: crate::layout::ColorType

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use image::{ExtendedColorType, ImageDecoder, ImageFormat, ImageReader};

use crate::{
    format::{Format, SIGNATURE_BYTES},
    layout::{ColorType, Orientation, SourceColorType},
};

/// What a decoder says about a file before it reads a pixel of it.
#[derive(Clone, Debug)]
pub struct Metadata {
    /// The stored size, before any orientation is applied.
    pub width: u32,
    /// The stored height, before any orientation is applied.
    pub height: u32,
    /// The layout the pixels decode into.
    pub color_type: ColorType,
    /// The encoding the file holds, for the source-label property.
    pub original_color_type: SourceColorType,
    /// The exif orientation the file states.
    pub orientation: Orientation,
    /// The format the file was identified as.
    #[allow(dead_code, reason = "the probe re-identifies on every read")]
    pub format: Format,
    /// The bytes one decoded picture occupies.
    pub total_bytes: u64,
}

/// What an open failed at, so the caller can word the error the way it always
/// has: `failed to {action} image '{path}': {detail}`.
#[derive(Clone, Debug)]
pub struct OpenError {
    /// The verb the message starts with, without `failed to`.
    pub action: &'static str,
    /// What went wrong.
    pub detail: String,
}

impl OpenError {
    fn new(action: &'static str, detail: impl std::fmt::Display) -> Self {
        Self {
            action,
            detail: detail.to_string(),
        }
    }
}

/// An open still file: the path the pixels come from and what was read about
/// them.
pub struct Decoder {
    /// The file, kept rather than a reader, because the crate's reader is what
    /// carries the format and it is built once for the probe and once per read.
    path: PathBuf,
    metadata: Metadata,
    icc_profile: Option<Vec<u8>>,
}

impl Decoder {
    /// Opens `path` and reads everything but its pixels.
    ///
    /// # Errors
    ///
    /// Returns what the probe failed at, so the caller can report it with the
    /// same action prefix every other decoder path uses.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        // The identification is read for the fallback in [`decode_reader`], which
        // is where a container the crate cannot name is refused as this plugin's
        // own format rather than as an unknown. It is *not* refused here, because
        // a heif is one of those containers and is perfectly readable.
        let format = identify(path)?;

        let (metadata, icc_profile) = probe(path, format)?;
        Ok(Self {
            path: path.to_path_buf(),
            metadata,
            icc_profile,
        })
    }

    /// What the decoder says about the file.
    #[must_use]
    pub const fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// The embedded ICC profile's bytes.
    #[must_use]
    pub fn icc_profile(&self) -> Option<&[u8]> {
        self.icc_profile.as_deref()
    }

    /// Reads the whole picture into `buffer`.
    ///
    /// # Errors
    ///
    /// Returns what the decode failed at.
    pub fn read(self, buffer: &mut [u8]) -> Result<(), OpenError> {
        let decoder = decode_reader(&self.path)?;
        decoder
            .read_image_boxed(buffer)
            .map_err(|error| OpenError::new("decode", error))
    }
}

/// The decoder the crate builds for a reader, in the order the old reader used.
///
/// Three things decide the format, and the order between them is the whole
/// content of this function:
///
/// 1. The extension seeds a hint, which is what `ImageReader::open` did before
///    the hooks existed and is what keeps a targa -- the one format with no
///    signature at all -- readable.
/// 2. `with_guessed_format` lets a registered hook replace it. The hooks are
///    consulted only for a guess, so this has to run before the fallback seeds
///    a format.
/// 3. This plugin's identification is the last resort, for a path whose
///    extension names nothing and whose bytes no hook matched.
///
/// The hint from step 1 is only applied when a hook claimed the file, and that
/// is the part worth reading twice. `require_format` keeps whatever format the
/// reader already holds and sniffs only when it holds none, so a hint that is
/// set before `into_decoder` runs *suppresses* the crate's own signature table --
/// which is exactly what reads an `ftyp` file as its avif and so reaches the
/// libheif hooks. A targa has no signature, so for it the hint is the only
/// answer there is; for everything else the crate's own sniffing is better
fn decode_reader(path: &Path) -> Result<Box<dyn ImageDecoder>, OpenError> {
    // `ImageReader::open` is what seeds the format from the extension, and that
    // seeding is what `require_format` needs: `into_decoder` does not sniff on
    // its own, it only uses what the reader already holds.
    let reader = ImageReader::open(path).map_err(|error| OpenError::new("open", error))?;
    // `.with_guessed_format()` now, before anything else seeds a format, so the
    // registered hooks get to replace the extension. A hook is what hands a
    // heic to libheif, and its brand -- this tree's `animation.heic` states
    // `hevx` -- is one the crate's own signature table does not know.
    let reader = reader
        .with_guessed_format()
        .map_err(|error| OpenError::new("identify", error))?;
    reader
        .into_decoder()
        .map(|decoder| Box::new(decoder) as Box<dyn ImageDecoder>)
        .map_err(|error| OpenError::new("create decoder for", error))
}

/// Reads everything but the pixels of the decoder for `path`.
fn probe(path: &Path, format: Format) -> Result<(Metadata, Option<Vec<u8>>), OpenError> {
    let mut decoder = decode_reader(path)?;

    let (width, height) = decoder.dimensions();
    let decoded = decoder.color_type();
    let color_type = ColorType::from_image(decoded).ok_or_else(|| {
        OpenError::new(
            "create decoder for",
            format!(
                "unsupported color type {}",
                source_color_type(decoded.into()).label()
            ),
        )
    })?;
    let original_color_type = source_color_type(decoder.original_color_type());
    let icc_profile = decoder
        .icc_profile()
        .map_err(|error| OpenError::new("read metadata from", error))?;
    let orientation = decoder
        .orientation()
        .map_err(|error| OpenError::new("read orientation from", error))?;
    let total_bytes = decoder.total_bytes();

    Ok((
        Metadata {
            width,
            height,
            color_type,
            original_color_type,
            orientation: orientation_from_image(orientation),
            format,
            total_bytes,
        },
        icc_profile,
    ))
}

/// The format a path's first bytes name, or the one its extension claims.
fn identify(path: &Path) -> Result<Format, OpenError> {
    let mut header = [0u8; SIGNATURE_BYTES];
    let mut file = File::open(path).map_err(|error| OpenError::new("open", error))?;
    let read = file
        .read(&mut header)
        .map_err(|error| OpenError::new("identify", error))?;
    Ok(Format::sniff(&header[..read]))
}

/// The `image` crate's format for one of ours, for the formats it decodes.
///
/// `None` is the answer for a container whose own adapter owns it. A file that
/// reaches this function with one of those is a file no adapter claimed, so it
/// is refused as what it is rather than decoded through a crate that cannot
/// describe it.
#[allow(
    dead_code,
    reason = "the per-container migrators still open their own files"
)]
const fn image_format(format: Format) -> Option<ImageFormat> {
    Some(match format {
        Format::Png => ImageFormat::Png,
        Format::Jpeg => ImageFormat::Jpeg,
        Format::Gif => ImageFormat::Gif,
        Format::Webp => ImageFormat::WebP,
        Format::Tiff => ImageFormat::Tiff,
        Format::Dds => ImageFormat::Dds,
        Format::Bmp => ImageFormat::Bmp,
        Format::Ico => ImageFormat::Ico,
        Format::Hdr => ImageFormat::Hdr,
        Format::Exr => ImageFormat::OpenExr,
        Format::Qoi => ImageFormat::Qoi,
        Format::Pnm => ImageFormat::Pnm,
        Format::Farbfeld => ImageFormat::Farbfeld,
        // A file that reaches this function with an avif, a heif, a jxl or a jpeg
        // 2000 is therefore one no adapter claimed, and it is refused as what it
        // is rather than handed on: `image` cannot describe any of the four.
        Format::Jxl
        | Format::Jpeg2000
        | Format::Avif
        | Format::Heif
        | Format::Tga
        | Format::Other => return None,
    })
}

/// The plugin's source label for one the crate reported.
fn source_color_type(value: ExtendedColorType) -> SourceColorType {
    use SourceColorType as Label;
    match value {
        ExtendedColorType::A8 => Label::A8,
        ExtendedColorType::L1 => Label::L1,
        ExtendedColorType::La1 => Label::La1,
        ExtendedColorType::Rgb1 => Label::Rgb1,
        ExtendedColorType::Rgba1 => Label::Rgba1,
        ExtendedColorType::L2 => Label::L2,
        ExtendedColorType::La2 => Label::La2,
        ExtendedColorType::Rgb2 => Label::Rgb2,
        ExtendedColorType::Rgba2 => Label::Rgba2,
        ExtendedColorType::L4 => Label::L4,
        ExtendedColorType::La4 => Label::La4,
        ExtendedColorType::Rgb4 => Label::Rgb4,
        ExtendedColorType::Rgba4 => Label::Rgba4,
        ExtendedColorType::Rgb5x1 => Label::Rgb5x1,
        ExtendedColorType::L8 => Label::L8,
        ExtendedColorType::La8 => Label::La8,
        ExtendedColorType::Rgb8 => Label::Rgb8,
        ExtendedColorType::Rgba8 => Label::Rgba8,
        ExtendedColorType::L16 => Label::L16,
        ExtendedColorType::La16 => Label::La16,
        ExtendedColorType::Rgb16 => Label::Rgb16,
        ExtendedColorType::Rgba16 => Label::Rgba16,
        ExtendedColorType::Bgr8 => Label::Bgr8,
        ExtendedColorType::Bgra8 => Label::Bgra8,
        ExtendedColorType::Rgb32F => Label::Rgb32F,
        ExtendedColorType::Rgba32F => Label::Rgba32F,
        ExtendedColorType::Cmyk8 => Label::Cmyk8,
        ExtendedColorType::Cmyk16 => Label::Cmyk16,
        // Every variant the pinned crate has is named above, so a value that
        // reaches this arm belongs to a layout this build does not know. It is
        // reported as itself rather than as rgb, which is the one default that
        // would be a wrong answer rather than a missing one.
        _ => Label::Unknown,
    }
}

/// The plugin's orientation for one the crate reported.
const fn orientation_from_image(value: image::metadata::Orientation) -> Orientation {
    match value {
        image::metadata::Orientation::NoTransforms => Orientation::NoTransforms,
        image::metadata::Orientation::Rotate90 => Orientation::Rotate90,
        image::metadata::Orientation::Rotate180 => Orientation::Rotate180,
        image::metadata::Orientation::Rotate270 => Orientation::Rotate270,
        image::metadata::Orientation::FlipHorizontal => Orientation::FlipHorizontal,
        image::metadata::Orientation::FlipVertical => Orientation::FlipVertical,
        image::metadata::Orientation::Rotate90FlipH => Orientation::Rotate90FlipH,
        image::metadata::Orientation::Rotate270FlipH => Orientation::Rotate270FlipH,
    }
}
