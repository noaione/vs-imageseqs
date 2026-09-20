//! webp decoding through libwebp.
//!
//! `image`'s `webp` feature is `image-webp`: pure rust, single threaded, and
//! it decodes into a canvas of its own which it then copies into the caller's
//! buffer. libwebp is the reference decoder for the format, it is vectorised,
//! and its decode entry points write into a buffer and a stride the caller
//! picks, so this path drops the canvas and the copy along with it.
//!
//! Only the pixel decode moves here. The probe, the color type, the exif
//! orientation and the icc profile still come from [`crate::decoder::probe`],
//! so a webp file keeps exactly the metadata it had while `image` decoded it,
//! and the size libwebp reads from the bitstream is checked against the probe
//! the way the `image` path checks its own decoder.
//!
//! Lossy webp is yuv 4:2:0 and libwebp decodes it either way. A file with no
//! alpha channel is decoded into its own planes and comes out as `YUV420P8`:
//! half the bytes per image, no yuv to rgb conversion that the graph consuming
//! the frames would only undo, and twice as many frames in the lookahead
//! budget. Everything else keeps the interleaved layout the `image` path
//! produced, because lossless webp is rgb by definition and a file with an
//! alpha channel needs the buffer its alpha plane is read from.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::{Duration, Instant},
};

use image::ColorType;

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    pixel::PixelFormat,
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

/// Whether this module decodes `info`.
///
/// Every webp file goes through libwebp. The decoder hooks registered in
/// [`crate::decoder`] only ever report `Rgb8` or `Rgba8` for one, which is
/// exactly what the entry points below hand back, so no webp color type is
/// left for the `image` path to decode instead.
pub fn handles(info: &ImageInfo) -> bool {
    has_webp_extension(&info.path)
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
    if &riff[..4] != b"RIFF" || &riff[8..] != b"WEBP" {
        return None;
    }

    let mut header = BitstreamHeader {
        lossless: false,
        has_alpha: false,
    };
    // Chunk payloads are skipped by seeking, and every declared size has to fit
    // in the file: a file cut short is not something to guess a coding from.
    let mut offset = 12_u64;
    let mut chunk = [0; 8];
    loop {
        if offset + 8 > length {
            return None;
        }
        file.read_exact(&mut chunk).ok()?;
        offset += 8;
        let id = &chunk[..4];
        let size = u64::from(u32::from_le_bytes(chunk[4..].try_into().ok()?));
        // Every chunk payload is padded to an even size.
        let padded = size + (size & 1);
        if offset + padded > length {
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
                let mut payload = [0; 10];
                file.read_exact(&mut payload).ok()?;
                offset += 10;
                header.has_alpha = payload[0] & VP8X_ALPHA_FLAG != 0;
                continue;
            }
            b"ALPH" => header.has_alpha = true,
            b"ANIM" | b"ANMF" => return None,
            _ => {}
        }
        file.seek(SeekFrom::Current(padded as i64)).ok()?;
        offset += padded;
    }
}

fn has_webp_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(EXTENSION))
}

/// The libwebp decoder entry points, from `webp/decode.h`.
///
/// Declared by hand rather than through a `*-sys` crate: the subset used is two
/// functions that have kept their signature since libwebp 0.4, so a binding
/// generator would add a build dependency and a header search path for nothing.
#[allow(non_snake_case)]
mod libwebp {
    use std::ffi::{c_int, c_uchar};

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
        pub(super) fn WebPDecodeRGBInto(
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

/// Decodes one webp image into an interleaved buffer.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = Instant::now();
    // libwebp decodes from memory, so the file is read once here and the decode
    // below never goes back to the disk.
    let data = std::fs::read(&info.path).map_err(|error| image_error("open", &info.path, error))?;
    let open = open_started.elapsed();

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

    use image::ExtendedColorType;

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
            path: path.to_path_buf(),
            width,
            height,
            color_type,
            original_color_type: ExtendedColorType::Rgb8,
            has_icc_profile: false,
            icc_profile: None,
            cicp: None,
            chroma_location: None,
            orientation: image::metadata::Orientation::NoTransforms,
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
        color_type: ExtendedColorType,
        pixels: &[u8],
    ) -> (PathBuf, ImageInfo) {
        let mut encoded = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut encoded)
            .encode(pixels, width, height, color_type)
            .expect("a lossless webp stream");
        let path = write_temp(&format!("{name}.webp"), &encoded);
        let probed = probe(&path, true).expect("the image to probe");
        (path, probed)
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
        file.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
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
        let probed = probe(&path, true).expect("the fixture to probe");
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

    #[test]
    fn handles_every_webp_file() {
        assert!(handles(&info(
            Path::new("page.webp"),
            ColorType::Rgb8,
            3,
            2
        )));
        assert!(handles(&info(
            Path::new("page.webp"),
            ColorType::Rgba8,
            3,
            2
        )));
        assert!(!handles(&info(
            Path::new("page.png"),
            ColorType::Rgb8,
            3,
            2
        )));
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
        let (path, _) = write_and_probe("header", 3, 2, ExtendedColorType::Rgb8, &RGB);
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
        let (path, probed) = write_and_probe("rgb", 3, 2, ExtendedColorType::Rgb8, &RGB);
        assert_eq!(probed.color_type, ColorType::Rgb8);

        let decoded = decode(&probed).expect("the stream to decode");
        assert_eq!((decoded.width, decoded.height), (3, 2));
        assert_eq!(decoded.format, PixelFormat::Rgb8);
        assert_eq!(decoded.pixels, interleaved(ColorType::Rgb8, &RGB));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn decodes_alpha_untouched() {
        let (path, probed) = write_and_probe("rgba", 3, 2, ExtendedColorType::Rgba8, &RGBA);
        assert_eq!(probed.color_type, ColorType::Rgba8);

        let decoded = decode(&probed).expect("the stream to decode");
        assert_eq!(decoded.format, PixelFormat::Rgb8);
        assert_eq!(decoded.pixels, interleaved(ColorType::Rgba8, &RGBA));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_size_that_changed_after_probing_is_reported() {
        let (path, probed) = write_and_probe("resized", 3, 2, ExtendedColorType::Rgb8, &RGB);
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
}
