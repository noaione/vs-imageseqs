//! webp decoding: wpd for a still, libwebp for an animation.
//!
//! Two libraries read this format and this module uses both, because they are
//! good at different things:
//!
//! - **wpd** decodes an ordinary still. Its decode entry points hand out the
//!   borrowed rows of the picture it holds, so a still is written straight
//!   into the frames a call is filling and no intermediate buffer exists at
//!   all. `docs/improvements/35-wpd-webp-decoder.md` has the comparison that
//!   selected it and `docs/BENCH.md` has the integrated figures.
//! - **libwebp** keeps the animated container's rectangle decode, which is
//!   [`decode_rgba`]. The timeline, the canvas and this module's port of
//!   libwebp's integer blend are the pixels a graph already sees, and handing
//!   them to another compositor would change them; the plan page above records
//!   where the two differ.
//!
//! The split follows the *container* rather than a retry chain, and rather
//! than a frame count: a file whose container holds a timeline goes to
//! [`crate::animation::webp`], including one that displays a single picture,
//! and everything else is a still. A still whose file states an orientation is
//! the one case decoded into a buffer rather than streamed, because a
//! transform is the frame writer's walk.
//!
//! The probe lives here too: [`image_info`] reads the same container this module
//! already walks and reports the canvas, the alpha flag, the exif orientation and
//! the icc profile the reader being replaced reported. The size the bitstream
//! states is checked against it at decode time, from libwebp's own header reader
//! and again from the picture the decoder produced.
//!
//! Lossy webp is yuv 4:2:0 and both decoders hand it over either way. A file
//! with no alpha channel is decoded into its own planes and comes out as
//! `YUV420P8`: half the bytes per image, no yuv to rgb conversion that the
//! graph consuming the frames would only undo, and twice as many frames in the
//! lookahead budget. Everything else keeps the interleaved layout the `image`
//! path produced, because lossless webp is rgb by definition and a file with an
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
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

// The still decoder, from the module doc above. `wpd` is a git dependency
// pinned to an exact revision in `Cargo.toml`, and its decoder is deliberately
// not stored anywhere: it is not `Send`, so it is created inside the one fill
// that uses it and dropped with it. See [`Still`].
use wpd::api::{Decoder as WpdDecoder, Options as WpdOptions, Picture as WpdPicture};
use wpd::image::Format as WpdFormat;

use crate::layout::{ColorType, Orientation, SourceColorType};

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, RowSink, RowStream, image_error},
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
/// Only two of these are production entry points now. `WebPGetInfo` is the
/// second opinion on the size a still was probed at, and `WebPDecodeRGBAInto`
/// is the animation rectangle [`decode_rgba`] draws. The rgb and yuv entry
/// points are what this module's tests hold the two still decoders against, so
/// that a replaced decoder has an oracle rather than a restatement, and the
/// lossless encoders write the round trip's fixtures. No frame request reaches
/// one of those.
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

/// Decodes one webp still.
///
/// The file is read once, and its container decides how it is decoded rather
/// than a retry: a webp that holds a timeline is answered with the picture its
/// timeline starts with, through libwebp, and everything else is a still that
/// wpd reads. The one still not handed out as a row stream is the one whose
/// file states an orientation, because the transform is the frame writer's.
#[inline(never)]
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = Instant::now();
    // Both decoders work from memory, so the file is read once here and neither
    // of them goes back to the disk. The bytes are handed to the still below
    // rather than dropped, which is what keeps this one read.
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
    // The size the bitstream states is the second opinion on the one the
    // container was probed for, and it is the check the `image` path made of its
    // own decoder: a file that changed between the two reads is reported rather
    // than written into a frame of a different shape.
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

    let still = Still {
        data: Arc::from(data),
        path: info.path.clone(),
        width: info.width,
        height: info.height,
        color_type: info.color_type,
        format: info.format,
        transform: info.transform,
        open,
        metadata,
    };
    // A file that states an orientation is written by the frame writer, which is
    // where the transform lives: a transpose is read across a plane rather than
    // along it, and only a buffer can be read that way. Everything else goes
    // straight into the frame the call is filling.
    if still.transform == Transform::IDENTITY {
        return Ok(still.into_stream());
    }
    still.buffered()
}

/// One webp still, and everything a decode of it needs.
///
/// [`decode`] answers an ordinary still with this as the row stream its frames
/// are filled from, so the decoder is created inside [`RowStream::fill`] rather
/// than kept here: wpd's decoder is not `Send`, because its driver holds an
/// unconstrained `Box<dyn RowSink>`, and a row stream crosses the lookahead
/// pool's threads. A still is one picture, so nothing is lost by keeping the
/// decoder short lived: it exists for the length of one fill, and what outlives
/// the call is owned input, a size and a format.
///
/// The compressed bytes ride along rather than being read a second time,
/// because [`decode`] already read them to ask the container whether it holds
/// an animation. One read per operation, and nothing retained for the life of a
/// clip.
struct Still {
    /// The file's bytes, borrowed by the decoder [`RowStream::fill`] creates.
    data: Arc<[u8]>,
    path: PathBuf,
    /// The size the probe recorded, which the bitstream is checked against.
    width: u32,
    height: u32,
    /// The layout the buffer the decoder writes holds.
    color_type: ColorType,
    /// The format the frame is written as.
    format: PixelFormat,
    transform: Transform,
    /// What reading the file cost, which happened before this stream existed.
    open: Duration,
    /// What reading its header and checking its size cost.
    metadata: Duration,
}

/// A still prints what it is rather than the bytes it holds.
///
/// [`Pixels::Stream`] is `Debug` because the pixels a decode produced are
/// printed beside the timings they cost, and `data` here is the whole
/// compressed file: a derived print would put a sequence of a hundred megabytes
/// into a log line.
impl std::fmt::Debug for Still {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Still")
            .field("path", &self.path)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("color_type", &self.color_type)
            .field("format", &self.format)
            .field("bytes", &self.data.len())
            .finish_non_exhaustive()
    }
}

/// Channels one interleaved row of `color_type` holds.
///
/// A webp is rgb or rgba and nothing else, so this is the whole of the choice.
const fn channels_of(color_type: ColorType) -> Option<usize> {
    match color_type {
        ColorType::Rgb8 => Some(3),
        ColorType::Rgba8 => Some(4),
        _ => None,
    }
}

/// The layout wpd is asked to decode a still in.
///
/// An opaque lossy file is yuv 4:2:0 in the bitstream, so it is decoded as the
/// planes the frame already holds; everything else is packed, because lossless
/// webp is rgb by definition and a file with an alpha channel needs the fourth
/// sample the alpha clip is read from. wpd has no planar rgb output, which is
/// why a packed picture is deinterleaved on the way into the frames.
fn wpd_format(pixel_format: PixelFormat, color_type: ColorType) -> Result<WpdFormat> {
    match (pixel_format, color_type) {
        (PixelFormat::Yuv420P8, ColorType::Rgb8) => Ok(WpdFormat::Yuv420p),
        (PixelFormat::Rgb8, ColorType::Rgb8) => Ok(WpdFormat::Rgb),
        (PixelFormat::Rgb8, ColorType::Rgba8) => Ok(WpdFormat::Rgba),
        (pixel_format, color_type) => Err(ImgSeqError::new(format!(
            "a webp probed as {color_type:?} in {pixel_format:?} has no decoder layout"
        ))),
    }
}

/// A decoder set up for one still, borrowing the bytes it was read from.
///
/// One internal thread, explicitly: the plugin's parallelism is the lookahead
/// pool, and wpd's automatic setting would give every worker a pool of its own.
/// The frame size limit is the pixel count the probe recorded, so a bitstream
/// whose header asks for more than the container states is refused before
/// anything that size is allocated rather than after the decode the check below
/// would catch.
fn still_decoder(still: &Still) -> Result<WpdDecoder<'_>> {
    let mut decoder = WpdDecoder::new();
    decoder
        .set_options(WpdOptions {
            n_threads: 1,
            frame_size_limit: still.width.saturating_mul(still.height),
            ..WpdOptions::default()
        })
        .map_err(|error| image_error("decode", &still.path, error))?;
    decoder
        .set_format(wpd_format(still.format, still.color_type)?)
        .map_err(|error| image_error("decode", &still.path, error))?;
    // Borrowed rather than owned: the bytes are already an allocation this
    // operation holds, and wpd would copy them a second time for nothing.
    decoder
        .open_borrowed(&still.data)
        .map_err(|error| image_error("decode", &still.path, error))?;
    Ok(decoder)
}

/// Checks a decoded picture against what the probe described.
///
/// The decoder's own header is the second opinion on the container's size, and
/// the layout is the one this module asked for: a decoder that answered with
/// something else would be written into a frame of the wrong shape. Both are
/// errors rather than a fallback, because the file changed under the plugin or
/// the two readers of it disagree, and neither is a picture to hand out.
fn check_picture(picture: &WpdPicture<'_>, still: &Still) -> Result<()> {
    if i64::from(picture.width()) != i64::from(still.width)
        || i64::from(picture.height()) != i64::from(still.height)
    {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {}x{}, decoded as {}x{})",
            still.path.display(),
            still.width,
            still.height,
            picture.width(),
            picture.height(),
        )));
    }
    let expected = wpd_format(still.format, still.color_type)?;
    if picture.format() != expected {
        return Err(ImgSeqError::new(format!(
            "image '{}' decoded as {:?} rather than the {expected:?} it was asked for",
            still.path.display(),
            picture.format(),
        )));
    }
    Ok(())
}

impl RowStream for Still {
    fn has_alpha(&self) -> bool {
        self.color_type.has_alpha()
    }

    fn fill(&mut self, mut sink: RowSink<'_>) -> Result<DecodeTimings> {
        let read_started = Instant::now();
        let mut decoder = still_decoder(self)?;
        let picture = decoder
            .next_frame()
            .map_err(|error| image_error("decode", &self.path, error))?
            .ok_or_else(|| image_error("decode", &self.path, "the bitstream holds no picture"))?;
        check_picture(&picture, self)?;
        write_rows(
            &picture,
            self.color_type,
            self.format,
            &mut sink,
            &self.path,
        )?;
        Ok(DecodeTimings {
            open: self.open,
            metadata: self.metadata,
            // A stream owns no buffer of its own: the decoder's picture is the
            // only one, and it exists for this call.
            buffer: Duration::ZERO,
            read: read_started.elapsed(),
        })
    }

    fn duplicate(&self) -> Box<dyn RowStream> {
        Box::new(Self {
            // The bytes are shared rather than copied: a duplicate is another
            // reader of the same file, not another file.
            data: Arc::clone(&self.data),
            path: self.path.clone(),
            width: self.width,
            height: self.height,
            color_type: self.color_type,
            format: self.format,
            transform: self.transform,
            open: self.open,
            metadata: self.metadata,
        })
    }
}

/// Writes one decoded picture into the frames of the call.
///
/// The shape of the frame decides the walk: a yuv picture is already one plane
/// per plane of the frame, and an rgb or rgba one is deinterleaved here, once,
/// on the way in.
fn write_rows(
    picture: &WpdPicture<'_>,
    color_type: ColorType,
    format: PixelFormat,
    sink: &mut RowSink<'_>,
    path: &Path,
) -> Result<()> {
    if format == PixelFormat::Yuv420P8 {
        return write_planes(picture, sink, path);
    }
    write_packed(picture, color_type, sink, path)
}

/// Copies the planes of a decoded yuv picture into the frame's planes.
///
/// Whole rows move at once, and what a row keeps is the frame's own row length
/// rather than the decoder's: on an odd width or height VapourSynth's
/// subsampled plane is a column or a row smaller than the decoder's, and the
/// extra samples have nowhere to go. The decoder rounds a half size plane up,
/// so the frame's own shape is always the smaller of the two.
fn write_planes(picture: &WpdPicture<'_>, sink: &mut RowSink<'_>, path: &Path) -> Result<()> {
    if sink.colour.len() != picture.planes() {
        return Err(ImgSeqError::new(format!(
            "image '{}' decoded to {} planes, which is not the {} a yuv frame holds",
            path.display(),
            picture.planes(),
            sink.colour.len(),
        )));
    }
    for (plane, target) in sink.colour.iter_mut().enumerate() {
        let rows = target.bytes.len() / target.stride;
        for (row, source) in picture.rows_of(plane).take(rows).enumerate() {
            let len = target.row_bytes.min(source.len());
            target.row(row)[..len].copy_from_slice(&source[..len]);
        }
    }
    Ok(())
}

/// Scatters one interleaved picture's rows into the frame's planes.
///
/// A call that hands out no alpha clip reads three samples of an rgba row and
/// drops the fourth, which is what the planar writer does with a four channel
/// layout too.
fn write_packed(
    picture: &WpdPicture<'_>,
    color_type: ColorType,
    sink: &mut RowSink<'_>,
    path: &Path,
) -> Result<()> {
    let missing = || {
        ImgSeqError::new(format!(
            "the colour frame of '{}' has fewer planes than {color_type:?} needs",
            path.display(),
        ))
    };
    for (row, source) in picture.rows_of(0).enumerate() {
        let placed = match color_type {
            ColorType::Rgb8 => sink.place_rgb8(source, row),
            ColorType::Rgba8 => sink.place_rgba8(source, row),
            other => {
                return Err(ImgSeqError::new(format!(
                    "image '{}' was probed as {other:?}, which is not a webp layout",
                    path.display(),
                )));
            }
        };
        if placed.is_none() {
            return Err(missing());
        }
    }
    Ok(())
}

impl Still {
    /// The decode a still that needs no rotation is answered with.
    ///
    /// Nothing is read here: the frames are filled from this stream in the
    /// worker that asked for them, which is where the timings come from.
    fn into_stream(self) -> DecodedImage {
        DecodedImage {
            width: self.width,
            height: self.height,
            format: self.format,
            transform: self.transform,
            pixels: Pixels::Stream(Box::new(self)),
            timings: DecodeTimings::default(),
        }
    }

    /// Decodes this still into buffers, for a picture the frame writer moves.
    ///
    /// This is the path a file that states an orientation takes. A transform
    /// reads across a plane rather than along it, and that walk is
    /// [`crate::pixel`]'s, so the picture is decoded into the layout the frame
    /// takes and the writer rearranges it: one pass over the picture more than
    /// [`RowStream::fill`] costs, for the files that need it and no others.
    fn buffered(&self) -> Result<DecodedImage> {
        let buffer_started = Instant::now();
        let buffers = self.allocate()?;
        let buffer = buffer_started.elapsed();

        let read_started = Instant::now();
        let mut decoder = still_decoder(self)?;
        let picture = decoder
            .next_frame()
            .map_err(|error| image_error("decode", &self.path, error))?
            .ok_or_else(|| image_error("decode", &self.path, "the bitstream holds no picture"))?;
        check_picture(&picture, self)?;
        let pixels = buffers.scatter(&picture, self)?;
        let read = read_started.elapsed();

        Ok(DecodedImage {
            width: self.width,
            height: self.height,
            format: self.format,
            transform: self.transform,
            pixels,
            timings: DecodeTimings {
                open: self.open,
                metadata: self.metadata,
                buffer,
                read,
            },
        })
    }

    /// Buffers of the layout this still's frame takes, allocated before the
    /// decode so that the allocation and the read are timed apart.
    fn allocate(&self) -> Result<Buffers> {
        let width = usize::try_from(self.width)
            .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
        let height = usize::try_from(self.height)
            .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
        if self.format == PixelFormat::Yuv420P8 {
            let mut planes = Vec::with_capacity(self.format.plane_count());
            for plane in 0..self.format.plane_count() {
                // The decoder's own plane size, which rounds a half size plane
                // up: the writer checks a tightly packed plane against exactly
                // this before it reads it.
                let (plane_width, plane_height) =
                    self.format.plane_dimensions(plane, width, height);
                planes.push(vec![0; plane_width * plane_height]);
            }
            return Ok(Buffers::Planes(planes));
        }
        let channels = channels_of(self.color_type).ok_or_else(|| {
            ImgSeqError::new(format!(
                "image '{}' was probed as {:?}, which is not a webp layout",
                self.path.display(),
                self.color_type,
            ))
        })?;
        let row_bytes = width
            .checked_mul(channels)
            .ok_or_else(|| ImgSeqError::new("image row is too large"))?;
        let size = row_bytes
            .checked_mul(height)
            .ok_or_else(|| ImgSeqError::new("image is too large"))?;
        Ok(Buffers::Packed {
            buffer: vec![0; size],
            row_bytes,
        })
    }
}

/// Where a buffered decode puts its picture, in the layout the frame takes.
enum Buffers {
    /// One tightly packed buffer per plane, for a yuv frame.
    Planes(Vec<Vec<u8>>),
    /// One interleaved buffer, for an rgb or rgba frame.
    Packed {
        buffer: Vec<u8>,
        /// Bytes one row of the picture occupies.
        row_bytes: usize,
    },
}

impl Buffers {
    /// Copies one decoded picture into these buffers and hands them over.
    ///
    /// Every row is written whole in both shapes, and what a row keeps is the
    /// frame's own row length: on an odd size a subsampled plane is a column or
    /// a row smaller than the decoder's, and the extra samples have nowhere to
    /// go.
    fn scatter(self, picture: &WpdPicture<'_>, still: &Still) -> Result<Pixels> {
        let width = usize::try_from(still.width)
            .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
        let height = usize::try_from(still.height)
            .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
        match self {
            Self::Planes(mut planes) => {
                for (plane, buffer) in planes.iter_mut().enumerate() {
                    let (plane_width, plane_height) =
                        still.format.plane_dimensions(plane, width, height);
                    for (row, source) in picture.rows_of(plane).take(plane_height).enumerate() {
                        let start = row * plane_width;
                        let len = plane_width.min(source.len());
                        buffer[start..start + len].copy_from_slice(&source[..len]);
                    }
                }
                Ok(Pixels::Planar {
                    planes,
                    alpha: None,
                })
            }
            Self::Packed {
                mut buffer,
                row_bytes,
            } => {
                for (row, source) in picture.rows_of(0).take(height).enumerate() {
                    let start = row * row_bytes;
                    let len = row_bytes.min(source.len());
                    buffer[start..start + len].copy_from_slice(&source[..len]);
                }
                Ok(Pixels::Interleaved {
                    color_type: still.color_type,
                    buffer,
                })
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::layout::SourceColorType;

    use super::*;
    use crate::decoder::PlaneRows;
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

    /// Drives a decode's row stream into planes a test can read.
    ///
    /// The production sink is a VapourSynth frame, which a unit test has no
    /// core to allocate. This is the same walk over vectors laid out the way
    /// [`crate::clip`] lays a frame out — the frame's own plane size and no
    /// padding — so what comes back is what lands in the frame.
    fn planes_of(
        pixels: Pixels,
        format: PixelFormat,
        width: u32,
        height: u32,
    ) -> (Vec<Vec<u8>>, Option<Vec<u8>>) {
        let Pixels::Stream(mut stream) = pixels else {
            panic!("a still decodes into a row stream");
        };
        let (width, height) = (width as usize, height as usize);
        let mut colour: Vec<Vec<u8>> = (0..format.plane_count())
            .map(|plane| {
                let (plane_width, plane_height) =
                    format.frame_plane_dimensions(plane, width, height);
                vec![0; plane_width * plane_height]
            })
            .collect();
        let mut alpha = stream.has_alpha().then(|| vec![0; width * height]);
        let rows = colour
            .iter_mut()
            .enumerate()
            .map(|(plane, buffer)| {
                let row_bytes = format.frame_plane_dimensions(plane, width, height).0;
                PlaneRows {
                    bytes: buffer.as_mut_slice(),
                    stride: row_bytes,
                    row_bytes,
                }
            })
            .collect();
        let alpha_rows = alpha.as_mut().map(|buffer| {
            vec![PlaneRows {
                bytes: buffer.as_mut_slice(),
                stride: width,
                row_bytes: width,
            }]
        });
        stream
            .fill(RowSink {
                colour: rows,
                alpha: alpha_rows,
            })
            .expect("the stream to fill the planes");
        (colour, alpha)
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

    /// The layout wpd is asked for, and the pair it has no entry point for.
    ///
    /// An opaque lossy file is the only one decoded as planes, and a probe that
    /// named a layout which is neither rgb nor rgba has to be refused rather
    /// than decoded into a frame it does not fill.
    #[test]
    fn maps_the_layouts_the_decoder_is_asked_for() {
        assert_eq!(
            wpd_format(PixelFormat::Yuv420P8, ColorType::Rgb8).unwrap(),
            WpdFormat::Yuv420p
        );
        assert_eq!(
            wpd_format(PixelFormat::Rgb8, ColorType::Rgb8).unwrap(),
            WpdFormat::Rgb
        );
        assert_eq!(
            wpd_format(PixelFormat::Rgb8, ColorType::Rgba8).unwrap(),
            WpdFormat::Rgba
        );
        // A lossless file is rgb by definition and an alpha channel needs its
        // fourth sample, so neither can be asked for as yuv.
        assert!(wpd_format(PixelFormat::Rgb8, ColorType::L8).is_err());
        assert!(wpd_format(PixelFormat::Yuv420P8, ColorType::Rgba8).is_err());
    }

    #[test]
    fn a_channel_count_covers_the_two_layouts_a_webp_has() {
        assert_eq!(channels_of(ColorType::Rgb8), Some(3));
        assert_eq!(channels_of(ColorType::Rgba8), Some(4));
        assert_eq!(channels_of(ColorType::Rgb16), None);
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
        let (planes, alpha) = planes_of(
            decoded.pixels,
            decoded.format,
            decoded.width,
            decoded.height,
        );
        assert!(alpha.is_none(), "a lossy webp has no alpha plane");
        assert_eq!(
            planes.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![16 * 16, 8 * 8, 8 * 8]
        );
        // The fixture is a solid `#C83C28`, and the planes are the bt.601
        // limited range pair libwebp converts to rgb with. wpd decoded them, so
        // agreeing on the mean is the two decoders agreeing on the picture.
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

    /// The yuv planes are the ones libwebp's own yuv entry point writes.
    ///
    /// This is the comparison `docs/improvements/35-wpd-webp-decoder.md`'s
    /// parity table is: both decoders are asked for the same picture and the
    /// three planes have to be byte identical rather than merely close.
    /// libwebp is the decoder this path replaced, so it is the oracle here
    /// rather than a second reader with its own rounding.
    #[test]
    fn the_planes_are_the_ones_libwebp_writes() {
        let (path, probed) = lossy_fixture();
        let decoded = decode(&probed).expect("the fixture to decode");
        let (planes, _) = planes_of(
            decoded.pixels,
            decoded.format,
            decoded.width,
            decoded.height,
        );

        let (width, height) = (16_usize, 16_usize);
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        let data = std::fs::read(&path).unwrap();
        let mut reference = vec![
            vec![0u8; width * height],
            vec![0u8; chroma_width * chroma_height],
            vec![0u8; chroma_width * chroma_height],
        ];
        let [luma, u, v] = reference.as_mut_slice() else {
            unreachable!("three planes were allocated");
        };
        // SAFETY: every plane holds at least `stride * rows` bytes, the strides
        // are the row lengths of the 4:2:0 layout libwebp writes, and `data`
        // holds the whole file.
        let written = unsafe {
            libwebp::WebPDecodeYUVInto(
                data.as_ptr(),
                data.len(),
                luma.as_mut_ptr(),
                luma.len(),
                i32::try_from(width).unwrap(),
                u.as_mut_ptr(),
                u.len(),
                i32::try_from(chroma_width).unwrap(),
                v.as_mut_ptr(),
                v.len(),
                i32::try_from(chroma_width).unwrap(),
            )
        };
        assert!(!written.is_null(), "libwebp has to decode the fixture");
        assert_eq!(planes, reference);
    }

    #[test]
    fn the_planes_rebuild_the_rgb_libwebp_decodes() {
        let (path, probed) = lossy_fixture();
        let decoded = decode(&probed).expect("the fixture to decode");
        let (planes, _) = planes_of(
            decoded.pixels,
            decoded.format,
            decoded.width,
            decoded.height,
        );
        let (width, height) = (16_usize, 16_usize);

        // The conversion here is libwebp's bt.601 limited range pair, which is
        // what the plugin tags the planes with, and the planes are wpd's, so
        // this checks the two decoders against each other rather than
        // restating one of them.
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

    /// A file that states an orientation is decoded into buffers rather than
    /// streamed.
    ///
    /// A transform reads across a plane rather than along it, and that walk is
    /// the frame writer's, so this path cannot be a row stream. What the
    /// buffers hold is `tests/readalpha.vpy`'s yuv orientation section checked
    /// end to end; what this pins is that a rotated file does not take the
    /// stream and that its picture is the decoder's own plane layout.
    #[test]
    fn a_still_that_states_an_orientation_is_buffered() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("orientation-6.webp");
        let probed = probe(&fixture, true, false).expect("the fixture probes");
        assert_ne!(
            probed.transform,
            Transform::IDENTITY,
            "the fixture states one"
        );

        let decoded = decode(&probed).expect("the fixture to decode");
        // The stored size, with the handed-out size transposed, which is what
        // makes this the orientation path rather than the identity one.
        assert_eq!(
            (decoded.output_width(), decoded.output_height()),
            (decoded.height, decoded.width)
        );
        let Pixels::Planar { planes, alpha } = &decoded.pixels else {
            panic!("a yuv still is buffered into planes");
        };
        assert!(alpha.is_none());
        let width = decoded.width as usize;
        let height = decoded.height as usize;
        let expected: Vec<usize> = (0..decoded.format.plane_count())
            .map(|plane| {
                let (plane_width, plane_height) =
                    decoded.format.plane_dimensions(plane, width, height);
                plane_width * plane_height
            })
            .collect();
        assert_eq!(planes.iter().map(Vec::len).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn decodes_rgb_back_to_the_source_pixels() {
        let (path, probed) = write_and_probe("rgb", 3, 2, SourceColorType::Rgb8, &RGB);
        assert_eq!(probed.color_type, ColorType::Rgb8);

        let decoded = decode(&probed).expect("the stream to decode");
        assert_eq!((decoded.width, decoded.height), (3, 2));
        assert_eq!(decoded.format, PixelFormat::Rgb8);
        let (planes, alpha) = planes_of(
            decoded.pixels,
            decoded.format,
            decoded.width,
            decoded.height,
        );
        assert!(alpha.is_none(), "a lossless rgb file states no alpha");
        assert_eq!(
            planes,
            vec![
                vec![1, 4, 7, 250, 253, 0],
                vec![2, 5, 8, 251, 254, 128],
                vec![3, 6, 9, 252, 255, 64],
            ]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn decodes_alpha_untouched() {
        let (path, probed) = write_and_probe("rgba", 3, 2, SourceColorType::Rgba8, &RGBA);
        assert_eq!(probed.color_type, ColorType::Rgba8);

        let decoded = decode(&probed).expect("the stream to decode");
        assert_eq!(decoded.format, PixelFormat::Rgb8);
        let (planes, alpha) = planes_of(
            decoded.pixels,
            decoded.format,
            decoded.width,
            decoded.height,
        );
        let alpha = alpha.expect("an rgba file hands out an alpha clip");
        assert_eq!(alpha, vec![0, 128, 255, 1, 127, 192]);
        // libwebp's lossless encoder is free to clear the rgb of a fully
        // transparent pixel, because no reader can see it; the first pixel of
        // the fixture is exactly that case. Every other sample has to come
        // back untouched, so the two are checked apart rather than by relaxing
        // the whole plane.
        let transparent = (0..3).all(|plane| planes[plane][0] == 0);
        assert!(
            transparent || planes.iter().map(|plane| plane[0]).eq([1u8, 2, 3]),
            "{planes:?}"
        );
        let tails: [[u8; 5]; 3] = [
            [4, 7, 250, 253, 0],
            [5, 8, 251, 254, 128],
            [6, 9, 252, 255, 64],
        ];
        for (plane, tail) in planes.iter().zip(tails) {
            assert_eq!(plane[1..], tail);
        }
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
