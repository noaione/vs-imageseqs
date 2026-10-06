//! webp decoding through libwebp.
//!
//! `image`'s `webp` feature is `image-webp`: pure rust, single threaded, and
//! it decodes into a canvas of its own which it then copies into the caller's
//! buffer. libwebp is the reference decoder for the format, it is vectorised,
//! and its decode entry points write into a buffer and a stride the caller
//! picks, so this path drops the canvas and the copy along with it.
//!
//! The probe lives here too: [`image_info`] reads the same container this module
//! already walks and reports the canvas, the alpha flag, the exif orientation and
//! the icc profile the reader being replaced reported. The size libwebp reads
//! from the bitstream is still checked against it at decode time, the way the
//! `image` path checked its own decoder.
//!
//! Lossy webp is yuv 4:2:0 and libwebp decodes it either way. A file with no
//! alpha channel is decoded into its own planes and comes out as `YUV420P8`:
//! half the bytes per image, no yuv to rgb conversion that the graph consuming
//! the frames would only undo, and twice as many frames in the lookahead
//! budget. Everything else keeps the interleaved layout the `image` path
//! produced, because lossless webp is rgb by definition and a file with an
//! alpha channel needs the buffer its alpha plane is read from.
//!
//! # Where the four facts a probe reports live
//!
//! The container states all four of them, in a chunk rather than in the image
//! header, which is why a probe has to walk it:
//!
//! - **The size.** A `VP8X` payload is ten bytes: `[0]` is the flags byte, whose
//!   `VP8X_ALPHA_FLAG` bit is already read below, then three reserved bytes, then
//!   the canvas width and height each less one, three bytes apiece, little
//!   endian, at `[4..7]` and `[7..10]`. A file with no `VP8X` keeps its size in
//!   the image chunk instead: `VP8 ` states a fourteen bit width and height at
//!   payload bytes 6..8 and 8..10, and `VP8L` states them less one in one little
//!   endian `u32` at 1..5, width in bits 0..13, height in 14..27, with
//!   **`alpha_is_used` in bit 28**.
//! - **The colour type**, `Rgb8` or `Rgba8`, from the alpha flag or an `ALPH`
//!   chunk, which [`BitstreamHeader`] already answers.
//! - **The profile**, the `ICCP` chunk payload. This is the `ImgSeqHasICC` fact
//!   and the `ICCProfile` property, and it is the branch a probe is most likely
//!   to miss because it is a chunk the walk has to *find*, not a number it can
//!   read where it stands.
//! - **The orientation**, the `EXIF` chunk payload through
//!   [`crate::exif::orientation_of`], as `png.rs` does. The reported size is the
//!   *stored* one and the transform does the swap, so `orientation-6` is stated
//!   16x12 and handed out 12x16.
//!
//! That is why there are two walks rather than one. [`BitstreamHeader`] returns as
//! soon as it reaches a `VP8 ` or `VP8L` chunk, without reading that chunk's
//! payload, and it returns `None` for `ANIM` or `ANMF`. Both are deliberate,
//! because `None` is how [`output_format`] learns a file is not a plain still.
//! `probe_segment` calls `describe` *before* it asks the animation adapter, so an
//! animated webp has to be describable from its `VP8X` too, and deleting the
//! refusal would take `output_format`'s answer away. [`probe_header`] walks the
//! whole container instead. The two read the same chunks and answer different
//! questions, so neither can become the other's copy of a decision.
//!
//! The size, colour type and orientation branches all have fixtures.
//! `webp-icc.webp`, `webp-icc-lossless.webp` and `webp-icc-alpha.webp` were added
//! for the profile branch, which had none at all, and `tests/readalpha.vpy`
//! asserts all of them end to end.

use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::layout::{ColorType, Orientation, SourceColorType};

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    exif::orientation_of,
    formats::identify,
    pixel::{PixelFormat, Transform},
};

/// File extension that holds a webp image.
const EXTENSION: &str = "webp";

/// Alpha flag of the `VP8X` container header.
const VP8X_ALPHA_FLAG: u8 = 0x10;

/// What the container header of a webp file says about its first image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BitstreamHeader {
    /// True when the pixels are stored losslessly, which means rgb.
    lossless: bool,
    /// True when the file carries an alpha channel.
    has_alpha: bool,
}

/// Format this module decodes the image into, when it is not the one the probed
/// color type suggests.
///
/// Lossy webp stores yuv 4:2:0, so a file with no alpha channel is decoded into
/// its own planes and written as `YUV420P8`: that is half the bytes per frame
/// and it skips the yuv to rgb conversion nothing asked for, which is also what
/// a graph consuming the frames would undo again. Lossless webp is rgb by
/// definition and keeps its color type, and so does a file with an alpha
/// channel, which needs the interleaved buffer the alpha plane is read from.
pub fn output_format(path: &Path, color_type: ColorType) -> Option<PixelFormat> {
    if !has_webp_extension(path) || color_type != ColorType::Rgb8 {
        return None;
    }
    let header = bitstream_header(path)?;
    (!header.lossless && !header.has_alpha).then_some(PixelFormat::Yuv420P8)
}

/// The byte a webp container's chunk walk ends at, from its `RIFF` header.
///
/// The size field counts the body and the four byte form type, so the container
/// ends eight bytes in. Both walks below bound themselves by it rather than by
/// the file: a file that does not hold everything its header declares is cut
/// short, and a chunk that is not there is not something to describe or to read
/// a coding out of.
fn container_end(riff: &[u8; 12], length: u64) -> Option<u64> {
    if &riff[..4] != b"RIFF" || &riff[8..] != b"WEBP" {
        return None;
    }
    let size = u64::from(u32::from_le_bytes(riff[4..8].try_into().ok()?));
    let end = 8_u64.checked_add(size)?;
    (end >= 12 && end <= length).then_some(end)
}

/// Walks the chunk headers of a webp file up to its first image chunk.
///
/// Payloads are skipped by seeking, so an embedded icc profile or exif block
/// costs nothing to walk past. Anything this does not understand - an animated
/// file, whose frames nest their own chunks, or a truncated one - returns
/// `None`, which leaves the image on the color type the probe reported.
fn bitstream_header(path: &Path) -> Option<BitstreamHeader> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let mut riff = [0; 12];
    file.read_exact(&mut riff).ok()?;
    let end = container_end(&riff, length)?;

    let mut header = BitstreamHeader {
        lossless: false,
        has_alpha: false,
    };
    // Chunk payloads are skipped by seeking, and every declared size has to fit
    // in the file: a file cut short is not something to guess a coding from.
    let mut offset = 12_u64;
    let mut chunk = [0; 8];
    loop {
        if offset + 8 > end {
            return None;
        }
        file.read_exact(&mut chunk).ok()?;
        offset += 8;
        let id = &chunk[..4];
        let size = u64::from(u32::from_le_bytes(chunk[4..].try_into().ok()?));
        // Every chunk payload is padded to an even size.
        let padded = size + (size & 1);
        if offset + padded > end {
            return None;
        }
        match id {
            b"VP8 " => {
                return Some(BitstreamHeader {
                    lossless: false,
                    ..header
                });
            }
            b"VP8L" => {
                return Some(BitstreamHeader {
                    lossless: true,
                    ..header
                });
            }
            b"VP8X" => {
                // Only the flags byte is this walk's question; a payload too
                // short to hold one is a container that is not chunked the way
                // the format says it is.
                if size < 10 {
                    return None;
                }
                let mut flags = [0; 10];
                file.read_exact(&mut flags).ok()?;
                header.has_alpha = flags[0] & VP8X_ALPHA_FLAG != 0;
            }
            b"ALPH" => header.has_alpha = true,
            b"ANIM" | b"ANMF" => return None,
            _ => {}
        }
        file.seek(SeekFrom::Start(offset + padded)).ok()?;
        offset += padded;
    }
}

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    identify::owns(identify::Format::Webp, path)
}

/// What a webp's container says about its first image.
///
/// Separate from [`BitstreamHeader`], which answers a different question: that one
/// stops at the first image chunk and refuses an animated file, because
/// [`output_format`] reads its `None` as "not a plain still". A probe has to
/// describe an animated webp as well -- `probe_segment` asks `describe` before it
/// asks the animation adapter -- so this walks the whole container instead and
/// answers what a description needs.
#[derive(Clone, Debug, Default)]
struct ProbeHeader {
    width: u32,
    height: u32,
    has_alpha: bool,
    icc_profile: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
}

/// Three bytes of little endian, which is how `VP8X` states a canvas.
fn u24(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16)
}

/// One chunk payload, which the caller has already checked fits in the file.
fn payload(file: &mut impl Read, size: u64) -> Option<Vec<u8>> {
    let mut bytes = vec![0; usize::try_from(size).ok()?];
    file.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

/// Reads a webp's container to its end, or `None` when it states no size.
///
/// The size is the stored one, so an oriented file reports the size it holds and
/// hands out the swap through its transform.
fn probe_header(file: &mut BufReader<File>) -> Option<ProbeHeader> {
    file.rewind().ok()?;
    // The container walk reads from the front whatever the caller left the reader
    // at, because a chunk list that starts in the middle of one is not one.
    let length = file.get_ref().metadata().ok()?.len();
    let mut riff = [0; 12];
    file.read_exact(&mut riff).ok()?;
    let end = container_end(&riff, length)?;

    let mut header = ProbeHeader::default();
    let mut offset = 12_u64;
    let mut chunk = [0; 8];
    while offset + 8 <= end {
        file.read_exact(&mut chunk).ok()?;
        offset += 8;
        let size = u64::from(u32::from_le_bytes(chunk[4..].try_into().ok()?));
        // Every chunk payload is padded to an even size, and every size has to
        // fit in the file: one that does not is not something to guess at.
        let padded = size + (size & 1);
        if offset + padded > end {
            return None;
        }
        match &chunk[..4] {
            // The canvas, which is the size of an animated file and of any
            // extended one. Width and height are each stated less one.
            b"VP8X" => {
                if size < 10 {
                    return None;
                }
                let mut flags = [0; 10];
                file.read_exact(&mut flags).ok()?;
                header.has_alpha = flags[0] & VP8X_ALPHA_FLAG != 0;
                header.width = u24(&flags[4..7]) + 1;
                header.height = u24(&flags[7..10]) + 1;
            }
            // A file with no `VP8X` keeps its size in the image chunk: fourteen
            // bits each, after the three byte frame tag and the start code.
            b"VP8 " => {
                if size < 10 {
                    return None;
                }
                let mut head = [0; 10];
                file.read_exact(&mut head).ok()?;
                if header.width == 0 {
                    header.width = u32::from(u16::from_le_bytes([head[6], head[7]]) & 0x3fff);
                    header.height = u32::from(u16::from_le_bytes([head[8], head[9]]) & 0x3fff);
                }
            }
            // Lossless states both less one in one word, with the alpha flag
            // beside them rather than in a container header.
            b"VP8L" => {
                if size < 5 {
                    return None;
                }
                let mut head = [0; 5];
                file.read_exact(&mut head).ok()?;
                let word = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
                if header.width == 0 {
                    header.width = (word & 0x3fff) + 1;
                    header.height = ((word >> 14) & 0x3fff) + 1;
                }
                if word & (1 << 28) != 0 {
                    header.has_alpha = true;
                }
            }
            b"ALPH" => header.has_alpha = true,
            b"ICCP" => header.icc_profile = payload(file, size),
            b"EXIF" => header.exif = payload(file, size),
            _ => {}
        }
        // Absolute rather than relative: a chunk whose payload was read is
        // already past it, and one that was not is not.
        file.seek(SeekFrom::Start(offset + padded)).ok()?;
        offset += padded;
    }
    // An animated file nests its frames inside `ANMF`, so a walk that reached the
    // end without a size at the top level has nothing to describe.
    (header.width != 0 && header.height != 0).then_some(header)
}

/// What a webp states, when this module reads the file.
///
/// The facts are the container's own chunks: the size from `VP8X` or from the
/// image chunk when there is none, the colour type from the alpha flag, and the
/// profile and the orientation from `ICCP` and `EXIF`. The reader being replaced
/// took them from the same places.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info(
    path: &Path,
    apply_rotation: bool,
    route: Option<crate::formats::identify::Format>,
    file: &mut BufReader<File>,
) -> Result<Option<ImageInfo>> {
    if !route.map_or_else(
        || owns(path),
        |saved| saved == crate::formats::identify::Format::Webp,
    ) {
        return Ok(None);
    }
    let Some(header) = probe_header(file) else {
        return Ok(None);
    };
    let color_type = if header.has_alpha {
        ColorType::Rgba8
    } else {
        ColorType::Rgb8
    };
    let orientation = header
        .exif
        .as_deref()
        .and_then(orientation_of)
        .unwrap_or(Orientation::NoTransforms);
    Ok(Some(ImageInfo {
        route: None,
        subimage: None,
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type,
        original_color_type: SourceColorType::from(color_type),
        has_icc_profile: header.icc_profile.is_some(),
        icc_profile: header.icc_profile.map(Arc::from),
        // A webp states no colour code of its own: a lossy one is bt.601 yuv,
        // which the frame's format says rather than a container field.
        cicp: None,
        chroma_location: None,
        orientation,
        transform: if apply_rotation {
            Transform::from_orientation(orientation)
        } else {
            Transform::IDENTITY
        },
        format: output_format(path, color_type)
            .or_else(|| PixelFormat::from_color_type(color_type))
            .ok_or_else(|| {
                ImgSeqError::new(format!(
                    "image '{}' has a colour type this plugin has no format for",
                    path.display()
                ))
            })?,
    }))
}

fn has_webp_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(EXTENSION))
}

/// The libwebp entry points, from `webp/decode.h` and `webp/encode.h`.
///
/// Declared by hand rather than through a `*-sys` crate: the subset used has
/// kept its signature since libwebp 0.4, so a binding generator would add a
/// build dependency and a header search path for nothing.
///
/// The encode entry points and the release they need are used by the tests'
/// round trip only, which is why they are allowed to be unused in a build
/// without them.
#[allow(non_snake_case, dead_code)]
pub(crate) mod libwebp {
    use std::ffi::{c_int, c_uchar};

    /// The signature `WebPEncodeLosslessRGB` and `WebPEncodeLosslessRGBA`
    /// share.
    pub(super) type EncodeEntryPoint = unsafe extern "C" fn(
        pixels: *const c_uchar,
        width: c_int,
        height: c_int,
        stride: c_int,
        output: *mut *mut c_uchar,
    ) -> usize;

    unsafe extern "C" {
        /// Width and height of a bitstream, read from its header.
        ///
        /// Returns 0 for data that is not a webp bitstream, in which case the
        /// outputs are left untouched. Unlike `WebPGetFeatures` this does not
        /// take a decoder abi version, so it stays callable whichever libwebp
        /// release the build links.
        pub(super) fn WebPGetInfo(
            data: *const c_uchar,
            data_size: usize,
            width: *mut c_int,
            height: *mut c_int,
        ) -> c_int;

        /// Decodes into `output_buffer` as interleaved rgb, one row every
        /// `output_stride` bytes.
        pub(crate) fn WebPDecodeRGBInto(
            data: *const c_uchar,
            data_size: usize,
            output_buffer: *mut c_uchar,
            output_buffer_size: usize,
            output_stride: c_int,
        ) -> *mut c_uchar;

        /// Decodes into `output_buffer` as interleaved rgba, one row every
        /// `output_stride` bytes.
        pub(super) fn WebPDecodeRGBAInto(
            data: *const c_uchar,
            data_size: usize,
            output_buffer: *mut c_uchar,
            output_buffer_size: usize,
            output_stride: c_int,
        ) -> *mut c_uchar;

        /// Decodes into one buffer per yuv plane, luma first, one row every
        /// `luma_stride` and `uv_stride` bytes.
        ///
        /// The chroma planes are half the size of the luma plane in each
        /// direction, which is the 4:2:0 layout `YUV420P8` uses.
        #[allow(clippy::too_many_arguments)]
        pub(super) fn WebPDecodeYUVInto(
            data: *const c_uchar,
            data_size: usize,
            luma: *mut c_uchar,
            luma_size: usize,
            luma_stride: c_int,
            u: *mut c_uchar,
            u_size: usize,
            u_stride: c_int,
            v: *mut c_uchar,
            v_size: usize,
            v_stride: c_int,
        ) -> *mut c_uchar;

        /// Encodes interleaved rgb as a lossless bitstream.
        ///
        /// The returned buffer is allocated by libwebp and is the caller's to
        /// release with [`WebPFree`]. A null answer means the picture could
        /// not be encoded, which for a small test picture does not happen.
        pub(super) fn WebPEncodeLosslessRGB(
            rgb: *const c_uchar,
            width: c_int,
            height: c_int,
            stride: c_int,
            output: *mut *mut c_uchar,
        ) -> usize;

        /// Encodes interleaved rgba as a lossless bitstream, which keeps the
        /// alpha channel the rgb entry point would drop.
        pub(super) fn WebPEncodeLosslessRGBA(
            rgba: *const c_uchar,
            width: c_int,
            height: c_int,
            stride: c_int,
            output: *mut *mut c_uchar,
        ) -> usize;

        /// Releases a buffer libwebp allocated.
        pub(super) fn WebPFree(pointer: *mut std::ffi::c_void);
    }
}

/// The signature `WebPDecodeRGBInto` and `WebPDecodeRGBAInto` share.
type EntryPoint = unsafe extern "C" fn(
    data: *const std::ffi::c_uchar,
    data_size: usize,
    output_buffer: *mut std::ffi::c_uchar,
    output_buffer_size: usize,
    output_stride: std::ffi::c_int,
) -> *mut std::ffi::c_uchar;

/// Decodes one standalone webp bitstream into interleaved RGBA.
///
/// The animation reader hands this the payload of one `ANMF` frame -- an
/// optional `ALPH` chunk followed by `VP8`/`VP8L`, wrapped by the caller into
/// the smallest container libwebp will accept -- and gets back the rectangle
/// that frame draws. RGBA is always asked for, because the canvas it is
/// composed onto is RGBA whatever the frame itself carries.
///
/// # Errors
///
/// Returns a message naming what libwebp refused, for the caller to qualify
/// with the path it was reading.
pub(crate) fn decode_rgba(data: &[u8]) -> std::result::Result<(u32, u32, Vec<u8>), String> {
    let mut width: std::ffi::c_int = 0;
    let mut height: std::ffi::c_int = 0;
    // SAFETY: the buffer and its length describe the same allocation, and the
    // two out parameters are writable locals.
    let known =
        unsafe { libwebp::WebPGetInfo(data.as_ptr(), data.len(), &raw mut width, &raw mut height) };
    if known == 0 || width <= 0 || height <= 0 {
        return Err("the frame is not a webp bitstream".to_string());
    }
    let (width, height) = (width as u32, height as u32);
    let row = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or_else(|| "the frame is too wide to decode".to_string())?;
    let size = row
        .checked_mul(height as usize)
        .ok_or_else(|| "the frame is too large to decode".to_string())?;
    let stride = std::ffi::c_int::try_from(row)
        .map_err(|_| "the frame is too wide for libwebp".to_string())?;
    let mut buffer = vec![0u8; size];
    // SAFETY: `buffer` is `size` bytes and the stride is its row length, so
    // libwebp writes exactly the allocation it was given.
    let decoded = unsafe {
        libwebp::WebPDecodeRGBAInto(data.as_ptr(), data.len(), buffer.as_mut_ptr(), size, stride)
    };
    if decoded.is_null() {
        return Err("libwebp could not decode the frame".to_string());
    }
    Ok((width, height, buffer))
}

/// Decodes one webp image into an interleaved buffer.
#[inline(never)]
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = Instant::now();
    // libwebp decodes from memory, so the file is read once here and the decode
    // below never goes back to the disk.
    let data = std::fs::read(&info.path).map_err(|error| image_error("open", &info.path, error))?;
    let open = open_started.elapsed();

    // libwebp's simple entry points read one image and refuse a container of
    // them, so a webp that holds an animation is answered with the picture its
    // timeline starts with. One reaches this module only when its name hid the
    // animation from `probe_segment`, which picks an animation adapter from the
    // file's route, and the timeline is read here rather than by a decoder of
    // its own.
    if let Some(first) =
        crate::animation::webp::first_picture(&info.path, &data, info.transform, info.format)?
    {
        return Ok(first);
    }

    let metadata_started = Instant::now();
    let (width, height) = header_dimensions(&data).ok_or_else(|| {
        image_error(
            "decode",
            &info.path,
            "libwebp did not recognise the bitstream",
        )
    })?;
    if (width, height) != (info.width, info.height) {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {}x{}, now {width}x{height})",
            info.path.display(),
            info.width,
            info.height,
        )));
    }
    let metadata = metadata_started.elapsed();
    let source = Source {
        data: &data,
        open,
        metadata,
    };

    if info.format == PixelFormat::Yuv420P8 {
        return decode_yuv(info, &source);
    }
    decode_rgb(info, &source)
}

/// The file bytes and header timings every decode path starts from.
struct Source<'a> {
    data: &'a [u8],
    open: Duration,
    metadata: Duration,
}

impl Source<'_> {
    /// Records what the decode of one image cost and what it produced.
    fn decoded(
        &self,
        info: &ImageInfo,
        pixels: Pixels,
        buffer: Duration,
        read: Duration,
    ) -> DecodedImage {
        DecodedImage {
            width: info.width,
            height: info.height,
            format: info.format,
            transform: info.transform,
            pixels,
            timings: DecodeTimings {
                open: self.open,
                metadata: self.metadata,
                buffer,
                read,
            },
        }
    }
}

/// Decodes one webp image into the interleaved buffer its color type needs.
fn decode_rgb(info: &ImageInfo, source: &Source<'_>) -> Result<DecodedImage> {
    // The buffer the frame writer reads holds every channel of the color type,
    // so the layout libwebp writes and the row size below come from the same
    // decision and cannot disagree.
    let (entry_point, channels) = layout(info)?;
    let row_bytes = row_bytes(info.width, channels)?;
    let stride = stride_of(row_bytes, info)?;
    let height = usize::try_from(info.height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;

    let buffer_started = Instant::now();
    let size = row_bytes
        .checked_mul(height)
        .ok_or_else(|| ImgSeqError::new("image is too large"))?;
    let mut pixels = vec![0; size];
    let buffer = buffer_started.elapsed();

    let read_started = Instant::now();
    // SAFETY: `data` holds the whole file, `pixels` is exactly `stride * height`
    // bytes, and `stride` is the row size of the layout libwebp was asked for,
    // so the decode writes inside `pixels` or fails.
    let decoded = unsafe {
        entry_point(
            source.data.as_ptr(),
            source.data.len(),
            pixels.as_mut_ptr(),
            pixels.len(),
            stride,
        )
    };
    if decoded.is_null() {
        return Err(image_error(
            "decode",
            &info.path,
            "libwebp rejected the bitstream",
        ));
    }

    Ok(source.decoded(
        info,
        Pixels::Interleaved {
            color_type: info.color_type,
            buffer: pixels,
        },
        buffer,
        read_started.elapsed(),
    ))
}

/// Decodes one lossy webp image into its own yuv planes.
fn decode_yuv(info: &ImageInfo, source: &Source<'_>) -> Result<DecodedImage> {
    let format = info.format;
    let width = usize::try_from(info.width)
        .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
    let height = usize::try_from(info.height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    let stride = |plane: usize| -> Result<i32> {
        let (plane_width, _) = format.plane_dimensions(plane, width, height);
        stride_of(plane_width * format.bytes_per_sample(), info)
    };

    let buffer_started = Instant::now();
    let mut planes = Vec::with_capacity(format.plane_count());
    for plane in 0..format.plane_count() {
        planes.push(vec![0; format.plane_bytes(plane, width, height)]);
    }
    let buffer = buffer_started.elapsed();
    let [luma, u, v] = planes.as_mut_slice() else {
        return Err(ImgSeqError::new(format!(
            "{} needs three planes, got {}",
            format.name(),
            planes.len(),
        )));
    };

    let read_started = Instant::now();
    // SAFETY: every plane holds exactly the stride times height bytes libwebp
    // is told about, so the decode writes inside the three buffers or fails.
    let decoded = unsafe {
        libwebp::WebPDecodeYUVInto(
            source.data.as_ptr(),
            source.data.len(),
            luma.as_mut_ptr(),
            luma.len(),
            stride(0)?,
            u.as_mut_ptr(),
            u.len(),
            stride(1)?,
            v.as_mut_ptr(),
            v.len(),
            stride(2)?,
        )
    };
    if decoded.is_null() {
        return Err(image_error(
            "decode",
            &info.path,
            "libwebp rejected the bitstream",
        ));
    }

    Ok(source.decoded(
        info,
        Pixels::Planar {
            planes,
            alpha: None,
        },
        buffer,
        read_started.elapsed(),
    ))
}

/// Size libwebp reads from the header of `data`.
fn header_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let mut width = 0;
    let mut height = 0;
    // SAFETY: `data` is a live slice, and both outputs point at initialised
    // locals that outlive the call. libwebp only writes them when the data is a
    // webp bitstream, which is what the return value below checks before they
    // are read.
    let is_webp =
        unsafe { libwebp::WebPGetInfo(data.as_ptr(), data.len(), &raw mut width, &raw mut height) };
    if is_webp == 0 {
        return None;
    }
    Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?))
}

/// The libwebp entry point for the probed color type, and the channels per
/// pixel it writes.
///
/// `image-webp` reports `Rgb8` for a webp file without an alpha channel and
/// `Rgba8` with one. Asking libwebp for the layout that matches keeps the
/// decoded buffer byte for byte what the `image` path handed back, alpha
/// channel included.
fn layout(info: &ImageInfo) -> Result<(EntryPoint, usize)> {
    match info.color_type {
        ColorType::Rgb8 => Ok((libwebp::WebPDecodeRGBInto, 3)),
        ColorType::Rgba8 => Ok((libwebp::WebPDecodeRGBAInto, 4)),
        other => Err(ImgSeqError::new(format!(
            "image '{}' was probed as {other:?}, which is not a webp layout",
            info.path.display(),
        ))),
    }
}

/// Bytes one packed row of the decoded image occupies.
fn row_bytes(width: u32, channels: usize) -> Result<usize> {
    usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(channels))
        .ok_or_else(|| ImgSeqError::new("image row is too large"))
}

/// Row stride to hand libwebp for a row of `row_bytes` bytes.
fn stride_of(row_bytes: usize, info: &ImageInfo) -> Result<i32> {
    i32::try_from(row_bytes).map_err(|_| {
        ImgSeqError::new(format!(
            "image '{}' is too wide for libwebp",
            info.path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::layout::SourceColorType;

    use super::*;
    use crate::decoder::probe;

    /// The 3x2 rgb image the round trip decodes.
    const RGB: [u8; 18] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 250, 251, 252, 253, 254, 255, 0, 128, 64,
    ];

    /// The 3x2 rgba image the round trip decodes, one pixel per alpha value a
    /// plane can hold.
    const RGBA: [u8; 24] = [
        1, 2, 3, 0, 4, 5, 6, 128, 7, 8, 9, 255, 250, 251, 252, 1, 253, 254, 255, 127, 0, 128, 64,
        192,
    ];

    fn info(path: &Path, color_type: ColorType, width: u32, height: u32) -> ImageInfo {
        ImageInfo {
            route: None,
            subimage: None,
            path: path.to_path_buf(),
            width,
            height,
            color_type,
            original_color_type: SourceColorType::Rgb8,
            has_icc_profile: false,
            icc_profile: None,
            cicp: None,
            chroma_location: None,
            orientation: crate::layout::Orientation::NoTransforms,
            transform: crate::pixel::Transform::IDENTITY,
            format: crate::pixel::PixelFormat::from_color_type(color_type)
                .expect("a supported color type"),
        }
    }

    /// Writes a lossless webp stream for `pixels` and probes the file the way
    /// the plugin does, so `decode` sees the `ImageInfo` a real read produces.
    fn write_and_probe(
        name: &str,
        width: u32,
        height: u32,
        color_type: SourceColorType,
        pixels: &[u8],
    ) -> (PathBuf, ImageInfo) {
        let encoded = encode_lossless(pixels, width, height, color_type);
        let path = write_temp(&format!("{name}.webp"), &encoded);
        let probed = probe(&path, true, false).expect("the image to probe");
        (path, probed)
    }

    /// Encodes one picture as a lossless webp through libwebp's own encoder.
    ///
    /// The round trip is what these tests are about, so the stream is written
    /// by the same library that reads it back. The `image` crate used to write
    /// it here, which was the last thing this crate's production code needed it
    /// for.
    fn encode_lossless(
        pixels: &[u8],
        width: u32,
        height: u32,
        color_type: SourceColorType,
    ) -> Vec<u8> {
        let width = i32::try_from(width).expect("a test width");
        let height = i32::try_from(height).expect("a test height");
        let (channels, encode) = match color_type {
            SourceColorType::Rgb8 => (
                3,
                libwebp::WebPEncodeLosslessRGB as libwebp::EncodeEntryPoint,
            ),
            SourceColorType::Rgba8 => (
                4,
                libwebp::WebPEncodeLosslessRGBA as libwebp::EncodeEntryPoint,
            ),
            other => panic!("a webp round trip is rgb or rgba, not {other:?}"),
        };
        let stride = width * channels;
        let mut output: *mut u8 = std::ptr::null_mut();
        // SAFETY: the buffer holds one packed row per row of the picture, the
        // dimensions are positive, and libwebp writes to the out pointer it
        // was given. The returned buffer is released below.
        let size = unsafe { encode(pixels.as_ptr(), width, height, stride, &raw mut output) };
        assert!(size > 0, "libwebp encoded the picture");
        // SAFETY: libwebp returned this buffer with this length, and it is
        // copied out before it is released.
        let encoded = unsafe { std::slice::from_raw_parts(output, size).to_vec() };
        // SAFETY: the buffer came from libwebp's encoder and is released once.
        unsafe { libwebp::WebPFree(output.cast()) };
        encoded
    }

    /// The container walk through an open of its own, which is what a probe does
    /// with the reader it already holds.
    fn header_at(path: &Path) -> Option<ProbeHeader> {
        let file = File::open(path).ok()?;
        probe_header(&mut BufReader::new(file))
    }

    /// The walk reads from the front of the open it is handed, not from wherever
    /// the caller left it.
    ///
    /// Nothing in this probe has read yet, so a walk that forgot to rewind would
    /// still pass every other test here; this moves the reader first, which is
    /// what a probe that had already grown its window hands over.
    #[test]
    fn the_container_walk_starts_at_the_front_of_the_open_it_is_given() {
        let (path, _) = lossy_fixture();
        let file = File::open(&path).expect("the fixture opens");
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(32)).expect("the reader moves");
        let header = probe_header(&mut reader).expect("the container to describe");
        assert!(
            header.width > 0 && header.height > 0,
            "the size is the one the container states, not the one found by accident"
        );
    }

    /// Writes bytes to a temp file named for this test process.
    fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("imgseqs-webp-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).expect("a writable image");
        path
    }

    /// A minimal RIFF container around `chunks`, padded the way the container
    /// pads an odd payload.
    fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, payload) in chunks {
            body.extend_from_slice(*id);
            body.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
            body.extend_from_slice(payload);
            if payload.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut file = Vec::from(*b"RIFF");
        // The size field counts the body *and* the four byte form type, which
        // is what a real container writes and what [`container_end`] reads
        // back. A helper that wrote the body alone would build files no reader
        // has to accept.
        file.extend_from_slice(&u32::try_from(body.len() + 4).unwrap().to_le_bytes());
        file.extend_from_slice(b"WEBP");
        file.extend_from_slice(&body);
        file
    }

    /// A `VP8X` payload: the feature flags, three reserved bytes, then the
    /// canvas size.
    fn vp8x(flags: u8) -> Vec<u8> {
        vec![flags, 0, 0, 0, 15, 0, 0, 15, 0, 0]
    }

    /// The 16x16 lossy fixture, and the `ImageInfo` the plugin probes for it.
    fn lossy_fixture() -> (PathBuf, ImageInfo) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("lossy.webp");
        let probed = probe(&path, true, false).expect("the fixture to probe");
        (path, probed)
    }

    /// An interleaved decode result, the layout every color type but yuv uses.
    fn interleaved(color_type: ColorType, buffer: &[u8]) -> Pixels {
        Pixels::Interleaved {
            color_type,
            buffer: buffer.to_vec(),
        }
    }

    /// One chunk of a container, as the id and payload a case is built from.
    type Chunk = (&'static [u8; 4], Vec<u8>);

    /// A coding, as the `lossless`/`has_alpha` pair a case is written with.
    type Coding = Option<(bool, bool)>;

    #[test]
    fn only_webp_extensions_are_taken_over() {
        assert!(has_webp_extension(Path::new("a.webp")));
        assert!(has_webp_extension(Path::new("a.WEBP")));
        assert!(!has_webp_extension(Path::new("a.webpx")));
        assert!(!has_webp_extension(Path::new("webp")));
        assert!(!has_webp_extension(Path::new("a.png")));
    }

    /// A webp is this module's whatever its frame holds. The gate this used to
    /// be is [`identify::route`] now, which every module reads, so this pins
    /// the route rather than a second copy of it.
    #[test]
    fn every_webp_file_routes_to_this_module() {
        let route = crate::formats::identify::route;
        let webp = Some(crate::formats::identify::Format::Webp);
        for path in ["page.webp", "page.WEBP", "page.WebP"] {
            assert_eq!(route(Path::new(path)), webp, "{path}");
        }
        assert_ne!(route(Path::new("page.png")), webp);
        assert_ne!(route(Path::new("page.webpx")), webp);
    }

    /// The probe answers from the container, and it carries four facts: the
    /// size, the alpha flag, the profile and the orientation. Each lives in a
    /// different chunk, so each is written alone here -- a fact read from the
    /// wrong place would come back absent or zero and this would catch it.
    #[test]
    fn the_probe_reads_its_four_facts_out_of_the_container() {
        // The canvas, with an odd sized profile and exif block in front of the
        // image data the walk has to step over.
        let path = write_temp(
            "probe-extended.webp",
            &riff(&[
                (b"VP8X", vp8x(0x10)),
                (b"ICCP", vec![1, 2, 3]),
                (b"EXIF", vec![4, 5]),
                (b"ANIM", vec![0; 6]),
            ]),
        );
        let header = header_at(&path).expect("an extended container to describe");
        assert_eq!((header.width, header.height), (16, 16));
        assert!(header.has_alpha, "the VP8X alpha bit");
        assert_eq!(header.icc_profile.as_deref(), Some(&[1, 2, 3][..]));
        assert_eq!(header.exif.as_deref(), Some(&[4, 5][..]));
        let _ = std::fs::remove_file(&path);

        // A file with no `VP8X` keeps its size in the image chunk: fourteen
        // bits each, after the three byte frame tag and the start code.
        let mut lossy = vec![0_u8; 10];
        lossy[6..8].copy_from_slice(&20_u16.to_le_bytes());
        lossy[8..10].copy_from_slice(&12_u16.to_le_bytes());
        let path = write_temp("probe-lossy.webp", &riff(&[(b"VP8 ", lossy)]));
        let header = header_at(&path).expect("a plain lossy still to describe");
        assert_eq!((header.width, header.height), (20, 12));
        assert!(!header.has_alpha);
        assert!(header.icc_profile.is_none() && header.exif.is_none());
        let _ = std::fs::remove_file(&path);

        // Lossless states both less one in one word, with the alpha flag
        // beside them rather than in a container header.
        let word = 4_u32 | (6_u32 << 14) | (1 << 28);
        let mut lossless = vec![0x2f];
        lossless.extend_from_slice(&word.to_le_bytes());
        let path = write_temp("probe-lossless.webp", &riff(&[(b"VP8L", lossless)]));
        let header = header_at(&path).expect("a plain lossless still to describe");
        assert_eq!((header.width, header.height), (5, 7));
        assert!(header.has_alpha, "the VP8L alpha bit");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_container_that_states_no_canvas_is_not_described() {
        // Every cut of a container, so a walk that reads past its own end has
        // nowhere to hide.
        let full = riff(&[
            (b"VP8X", vp8x(0)),
            (b"ICCP", vec![0; 5]),
            (b"VP8 ", vec![0; 10]),
        ]);
        for length in 0..full.len() {
            let path = write_temp("probe-truncated.webp", &full[..length]);
            assert!(
                header_at(&path).is_none(),
                "a cut at {length} bytes still describes"
            );
            let _ = std::fs::remove_file(&path);
        }

        // An animated file nests its frames, so the only canvas is inside an
        // `ANMF` payload this walk does not descend into.
        let path = write_temp("probe-animated.webp", &riff(&[(b"ANIM", vec![0; 6])]));
        assert!(header_at(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn maps_the_color_types_the_probe_reports() {
        let rgb = layout(&info(Path::new("a.webp"), ColorType::Rgb8, 3, 2)).unwrap();
        assert_eq!(rgb.1, 3);
        let rgba = layout(&info(Path::new("a.webp"), ColorType::Rgba8, 3, 2)).unwrap();
        assert_eq!(rgba.1, 4);
        assert!(
            layout(&info(Path::new("a.webp"), ColorType::L8, 3, 2)).is_err(),
            "a layout libwebp cannot write has to be refused"
        );
    }

    #[test]
    fn rows_are_checked_against_the_width() {
        assert_eq!(row_bytes(3, 3).unwrap(), 9);
        assert_eq!(row_bytes(3, 4).unwrap(), 12);
        // The widest row a `u32` width can ask for only overflows where a
        // `usize` is not wider than the channels it is multiplied by.
        assert_eq!(row_bytes(u32::MAX, 4).is_ok(), usize::BITS > 34);
    }

    #[test]
    fn data_that_is_not_a_bitstream_has_no_dimensions() {
        assert_eq!(header_dimensions(&[]), None);
        assert_eq!(header_dimensions(b"not a webp image"), None);
        // A lossless stream is short enough to build by hand: the signature,
        // then the VP8L payload the encoder writes.
        let (path, _) = write_and_probe("header", 3, 2, SourceColorType::Rgb8, &RGB);
        let encoded = std::fs::read(&path).unwrap();
        assert_eq!(header_dimensions(&encoded), Some((3, 2)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_interleaved_result_is_what_the_color_type_describes() {
        assert_eq!(
            interleaved(ColorType::Rgb8, &RGB),
            Pixels::Interleaved {
                color_type: ColorType::Rgb8,
                buffer: RGB.to_vec(),
            }
        );
    }

    #[test]
    fn reads_the_coding_out_of_the_container() {
        let cases: [(&str, Vec<Chunk>, Coding); 7] = [
            (
                "still-lossy",
                vec![(b"VP8 ", vec![0; 8])],
                Some((false, false)),
            ),
            (
                "still-lossless",
                vec![(b"VP8L", vec![0; 8])],
                Some((true, false)),
            ),
            // A `VP8X` header without alpha, with an odd sized icc profile in
            // front of the image data that the walk has to seek past.
            (
                "extended-lossy",
                vec![
                    (b"VP8X", vp8x(0x24)),
                    (b"ICCP", vec![0; 11]),
                    (b"VP8 ", vec![0; 8]),
                ],
                Some((false, false)),
            ),
            // The alpha chunk is what says a lossy file has an alpha channel.
            (
                "extended-alpha",
                vec![
                    (b"VP8X", vp8x(0x10)),
                    (b"ALPH", vec![0; 3]),
                    (b"VP8 ", vec![0; 8]),
                ],
                Some((false, true)),
            ),
            (
                "extended-lossless",
                vec![(b"VP8X", vp8x(0)), (b"VP8L", vec![0; 8])],
                Some((true, false)),
            ),
            // An animation nests its frames, which this does not walk.
            (
                "animated",
                vec![(b"VP8X", vp8x(0x02)), (b"ANIM", vec![0; 6])],
                None,
            ),
            ("no-magic", vec![], None),
        ];

        for (name, chunks, expected) in cases {
            let path = write_temp(&format!("coding-{name}.webp"), &riff(&chunks));
            let expected = expected.map(|(lossless, has_alpha)| BitstreamHeader {
                lossless,
                has_alpha,
            });
            assert_eq!(bitstream_header(&path), expected, "{name}");
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn a_truncated_container_has_no_coding() {
        let full = riff(&[(b"VP8 ", vec![0; 8])]);
        for length in 0..=full.len() {
            let path = write_temp("coding-truncated.webp", &full[..length]);
            let header = bitstream_header(&path);
            if length == full.len() {
                assert!(header.is_some());
            } else {
                assert_eq!(header, None, "a cut at {length} bytes claims a coding");
            }
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn only_lossy_files_without_alpha_decode_to_yuv() {
        let lossy = riff(&[(b"VP8 ", vec![0; 8])]);
        let path = write_temp("format-lossy.webp", &lossy);
        assert_eq!(
            output_format(&path, ColorType::Rgb8),
            Some(PixelFormat::Yuv420P8)
        );
        // The probe reports `Rgba8` for a file with an alpha channel, and an
        // alpha plane needs the interleaved buffer this module writes for rgb.
        assert_eq!(output_format(&path, ColorType::Rgba8), None);
        let _ = std::fs::remove_file(&path);

        // The extension decides which module decodes the file, so a copy under
        // another name keeps the color type the probe reported.
        let path = write_temp("format-lossy.png", &lossy);
        assert_eq!(output_format(&path, ColorType::Rgb8), None);
        let _ = std::fs::remove_file(&path);

        let path = write_temp("format-lossless.webp", &riff(&[(b"VP8L", vec![0; 8])]));
        assert_eq!(output_format(&path, ColorType::Rgb8), None);
        let _ = std::fs::remove_file(&path);

        let path = write_temp(
            "format-alpha.webp",
            &riff(&[(b"VP8X", vp8x(0x10)), (b"VP8 ", vec![0; 8])]),
        );
        assert_eq!(output_format(&path, ColorType::Rgb8), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_lossy_file_is_probed_as_yuv_and_decodes_to_planes() {
        let (_, probed) = lossy_fixture();
        assert_eq!(probed.color_type, ColorType::Rgb8);
        assert_eq!(probed.format, PixelFormat::Yuv420P8);
        assert_eq!((probed.width, probed.height), (16, 16));

        let decoded = decode(&probed).expect("the fixture to decode");
        assert_eq!(decoded.format, PixelFormat::Yuv420P8);
        let Pixels::Planar { planes, alpha } = &decoded.pixels else {
            panic!("a lossy webp decodes into planes");
        };
        assert!(alpha.is_none(), "a lossy webp has no alpha plane");
        assert_eq!(
            planes.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![16 * 16, 8 * 8, 8 * 8]
        );
        // The fixture is a solid `#C83C28`, and the planes are the bt.601
        // limited range pair libwebp converts to rgb with.
        let mean = |plane: &[u8]| {
            plane.iter().map(|s| u32::from(*s)).sum::<u32>() as f64 / plane.len() as f64
        };
        assert!(
            (mean(&planes[0]) - 101.0).abs() < 6.0,
            "luma {}",
            mean(&planes[0])
        );
        assert!(
            (mean(&planes[1]) - 98.0).abs() < 6.0,
            "u {}",
            mean(&planes[1])
        );
        assert!(
            (mean(&planes[2]) - 191.0).abs() < 6.0,
            "v {}",
            mean(&planes[2])
        );
    }
    #[test]
    fn the_planes_rebuild_the_rgb_libwebp_decodes() {
        let (path, probed) = lossy_fixture();
        let decoded = decode(&probed).expect("the fixture to decode");
        let Pixels::Planar { planes, .. } = &decoded.pixels else {
            panic!("a lossy webp decodes into planes");
        };
        let (width, height) = (16_usize, 16_usize);

        // libwebp converts yuv to rgb with the bt.601 matrix for the limited
        // range, which is the pair the plugin tags the planes with, so the same
        // conversion here has to land on the rgb libwebp itself hands back.
        let data = std::fs::read(&path).unwrap();
        let stride = width * 3;
        let mut rgb = vec![0; stride * height];
        // SAFETY: `rgb` holds exactly `stride * height` bytes and `data` holds
        // the whole file, so the decode writes inside `rgb` or fails.
        let written = unsafe {
            libwebp::WebPDecodeRGBInto(
                data.as_ptr(),
                data.len(),
                rgb.as_mut_ptr(),
                rgb.len(),
                i32::try_from(stride).unwrap(),
            )
        };
        assert!(!written.is_null(), "the fixture has to decode as rgb too");

        let mut worst = 0.0_f32;
        let mut total = 0.0_f32;
        for y in 0..height {
            for x in 0..width {
                let luma = f32::from(planes[0][y * width + x]) - 16.0;
                let u = f32::from(planes[1][(y / 2) * (width / 2) + x / 2]) - 128.0;
                let v = f32::from(planes[2][(y / 2) * (width / 2) + x / 2]) - 128.0;
                let channels = [
                    1.164 * luma + 1.596 * v,
                    1.164 * luma - 0.392 * u - 0.813 * v,
                    1.164 * luma + 2.017 * u,
                ];
                for (channel, value) in channels.into_iter().enumerate() {
                    let expected = f32::from(rgb[(y * width + x) * 3 + channel]);
                    let delta = (value.clamp(0.0, 255.0) - expected).abs();
                    worst = worst.max(delta);
                    total += delta;
                }
            }
        }
        let mean = total / (width * height * 3) as f32;
        assert!(
            worst <= 6.0 && mean <= 2.0,
            "the bt.601 limited range conversion is off by {worst} at worst, {mean} on average"
        );
    }

    #[test]
    fn decodes_rgb_back_to_the_source_pixels() {
        let (path, probed) = write_and_probe("rgb", 3, 2, SourceColorType::Rgb8, &RGB);
        assert_eq!(probed.color_type, ColorType::Rgb8);

        let decoded = decode(&probed).expect("the stream to decode");
        assert_eq!((decoded.width, decoded.height), (3, 2));
        assert_eq!(decoded.format, PixelFormat::Rgb8);
        assert_eq!(decoded.pixels, interleaved(ColorType::Rgb8, &RGB));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn decodes_alpha_untouched() {
        let (path, probed) = write_and_probe("rgba", 3, 2, SourceColorType::Rgba8, &RGBA);
        assert_eq!(probed.color_type, ColorType::Rgba8);

        let decoded = decode(&probed).expect("the stream to decode");
        assert_eq!(decoded.format, PixelFormat::Rgb8);
        // libwebp's lossless encoder is free to clear the rgb of a fully
        // transparent pixel, because no reader can see it; the first pixel of
        // the fixture is exactly that case. Every other sample, alpha included,
        // has to come back untouched, so the two are checked apart rather than
        // by relaxing the whole buffer.
        let Pixels::Interleaved {
            color_type: decoded_type,
            buffer,
        } = &decoded.pixels
        else {
            panic!("a webp decodes into an interleaved buffer");
        };
        assert_eq!(*decoded_type, ColorType::Rgba8);
        let transparent = buffer[..4] == [0, 0, 0, 0];
        assert!(
            transparent || buffer[..4] == RGBA[..4],
            "{:?}",
            &buffer[..4]
        );
        assert_eq!(buffer[3], RGBA[3], "the transparent alpha");
        assert_eq!(buffer[4..], RGBA[4..]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_size_that_changed_after_probing_is_reported() {
        let (path, probed) = write_and_probe("resized", 3, 2, SourceColorType::Rgb8, &RGB);
        assert_eq!((probed.width, probed.height), (3, 2));
        let stale = info(&path, ColorType::Rgb8, 4, 2);

        let error = decode(&stale).expect_err("a stale probe to be caught");
        assert!(
            error.to_string().contains("changed after probing"),
            "{error}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_that_is_not_a_bitstream_is_reported() {
        let path =
            std::env::temp_dir().join(format!("imgseqs-webp-{}-broken.webp", std::process::id()));
        std::fs::write(&path, b"not a webp image").unwrap();

        let error = decode(&info(&path, ColorType::Rgb8, 3, 2)).expect_err("an error");
        assert!(error.to_string().contains("did not recognise"), "{error}");
        let _ = std::fs::remove_file(&path);
    }

    /// A webp whose name hid its animation from the probe is still read as a
    /// still, and what it contributes is the picture its timeline starts with.
    /// `image`'s webp reader used to answer this and the `webp` feature is gone
    /// from `Cargo.toml`, so this is the only reader such a file has: if the
    /// branch that finds the animation went away, this would fail with libwebp
    /// refusing a container of frames rather than with a wrong picture.
    #[test]
    fn a_webp_that_hides_its_animation_is_read_as_the_picture_it_starts_with() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("animation.webp");
        let bytes = std::fs::read(&fixture).expect("the animation fixture");
        // The name is what makes it a still: `probe_segment` picks the
        // animation adapter from the route, and the route is the bytes.
        let path = write_temp("hides-its-animation.png", &bytes);

        let probed = probe(&path, true, false).expect("the renamed fixture to probe");
        assert_eq!((probed.width, probed.height), (16, 12));
        assert_eq!(probed.color_type, ColorType::Rgba8);

        let decoded = decode(&probed).expect("the timeline's first picture");
        assert_eq!((decoded.width, decoded.height), (16, 12));
        assert_eq!(decoded.format, PixelFormat::Rgb8);
        // The same picture the timeline hands a segment, which is the one the
        // `image` reader handed back for a file like this.
        let expected =
            crate::animation::webp::first_picture(&path, &bytes, probed.transform, probed.format)
                .expect("the container to be readable")
                .expect("an animated container to be found");
        assert_eq!(decoded.pixels, expected.pixels);
        let _ = std::fs::remove_file(&path);
    }
}
