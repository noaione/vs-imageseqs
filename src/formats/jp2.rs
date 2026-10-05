//! JPEG 2000 decoding through the `jpeg2k` crate and its vendored OpenJPEG.
//!
//! JPEG 2000 has two useful forms: a bare codestream (`.j2k`/`.j2c`) and a
//! JP2-family box container (`.jp2`, `.jpf`, and `.jpx`). The `image` crate
//! does not describe either one, so this module reads the SIZ and JP2 header
//! records directly during probing. OpenJPEG is only entered when a frame is
//! requested.
//!
//! JPEG 2000 has no alpha convention this source can preserve. One or three
//! components are accepted; a two-component gray-plus-alpha image and images
//! with more than three components are rejected rather than silently losing a
//! channel.

use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::layout::ColorType;
use jpeg2k::{ColorSpace, Image, ImagePixelData};

use crate::{
    color::{Cicp, UNSPECIFIED},
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    pixel::{PixelFormat, Transform},
};

const JP2_SIGNATURE: [u8; 12] = [0, 0, 0, 12, b'j', b'P', b' ', b' ', 0x0d, 0x0a, 0x87, 0x0a];
const J2K_SIGNATURE: [u8; 4] = [0xff, 0x4f, 0xff, 0x51];

/// Bytes of the front of a file a probe reads before it grows the window.
///
/// Every fact a description reads lives in the signature, the `jp2h` superbox or
/// the `SIZ` at the start of the first codestream box, and a JP2 file writes
/// those first -- so a window this size holds the header of every file whose
/// colour profile is of an ordinary size.
const PROBE_WINDOW: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EnumeratedColor {
    Srgb,
    Gray,
    Sycc,
    Other,
    #[default]
    Unspecified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ComponentHeader {
    precision: u32,
    signed: bool,
    dx: u8,
    dy: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Header {
    width: u32,
    height: u32,
    components: Vec<ComponentHeader>,
    color: EnumeratedColor,
    has_icc_profile: bool,
    icc_profile: Option<Arc<[u8]>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SizHeader {
    width: u32,
    height: u32,
    components: Vec<ComponentHeader>,
}

/// Probe one JPEG 2000 image without asking OpenJPEG to decode its pixels.
pub fn image_info(
    path: &Path,
    _apply_rotation: bool,
    file: &mut BufReader<File>,
) -> Result<ImageInfo> {
    let header = read_description(file, path)?;
    let (color_type, format) = output_format(&header, path)?;

    Ok(ImageInfo {
        route: None,
        subimage: None,
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type,
        original_color_type: color_type.into(),
        has_icc_profile: header.has_icc_profile,
        icc_profile: header.icc_profile,
        cicp: cicp(header.color),
        chroma_location: None,
        orientation: crate::layout::Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format,
    })
}

/// The same, from an open of its own, which is what a caller that did not come
/// through a probe has.
#[cfg(test)]
pub fn image_info_at(path: &Path, apply_rotation: bool) -> Result<ImageInfo> {
    let file = File::open(path).map_err(|error| image_error("open", path, error))?;
    image_info(path, apply_rotation, &mut BufReader::new(file))
}

/// Decode a JPEG 2000 image into an interleaved buffer or its coded yuv planes.
#[inline(never)]
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = Instant::now();
    let data = std::fs::read(&info.path).map_err(|error| image_error("open", &info.path, error))?;
    let open = open_started.elapsed();

    let metadata_started = Instant::now();
    let header = parse_header(&data, &info.path)?;
    let (color_type, format) = output_format(&header, &info.path)?;
    if (header.width, header.height, color_type, format)
        != (info.width, info.height, info.color_type, info.format)
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
            color_type,
            format.name(),
        )));
    }
    let metadata = metadata_started.elapsed();

    let read_started = Instant::now();
    let image =
        Image::from_bytes(&data).map_err(|error| image_error("decode", &info.path, error))?;
    check_decoded_header(&image, &header, &info.path)?;
    let pixels = if format.color_family() == vapoursynth4_rs::ColorFamily::YUV {
        decode_yuv(&image, format, &info.path)?
    } else {
        decode_interleaved(&image, color_type, &info.path)?
    };
    let read = read_started.elapsed();

    Ok(DecodedImage {
        width: header.width,
        height: header.height,
        format: info.format,
        transform: info.transform,
        pixels,
        timings: DecodeTimings {
            open,
            metadata,
            buffer: Duration::ZERO,
            read,
        },
    })
}

fn output_format(header: &Header, path: &Path) -> Result<(ColorType, PixelFormat)> {
    let depth = header
        .components
        .first()
        .map_or(0, |component| component.precision);
    if depth == 0 {
        return Err(ImgSeqError::new(format!(
            "image '{}' states no JPEG 2000 component precision",
            path.display()
        )));
    }
    // VapourSynth has no signed integer format, so a signed component cannot be
    // handed out as the samples it holds: a frame that carried them would be
    // saying the numbers are unsigned, which is a different picture rather than a
    // shifted one. This is a refusal at the probe, so a clip is never created for
    // a file no frame could be produced from.
    if let Some((index, component)) = header
        .components
        .iter()
        .enumerate()
        .find(|(_, component)| component.signed)
    {
        return Err(ImgSeqError::new(format!(
            "image '{}' has a signed JPEG 2000 component (component {} of {}, {} bits signed), and VapourSynth has no signed integer format",
            path.display(),
            index + 1,
            header.components.len(),
            component.precision
        )));
    }
    // A frame's format names one depth, so components of two widths would have to
    // be widened into it. That is exact but it is not what the file states, and
    // nothing has asked for it, so the file is refused by name rather than
    // converted silently.
    if let Some((index, component)) = header
        .components
        .iter()
        .enumerate()
        .find(|(_, component)| component.precision != depth)
    {
        return Err(ImgSeqError::new(format!(
            "image '{}' has JPEG 2000 components of different precision (component 1 is {depth} bits, component {} is {} bits)",
            path.display(),
            index + 1,
            component.precision
        )));
    }

    match header.components.len() {
        1 => {
            if header.color == EnumeratedColor::Sycc {
                return Err(ImgSeqError::new(format!(
                    "image '{}' marks a single JPEG 2000 component as sYCC",
                    path.display()
                )));
            }
            let color_type = if depth <= 8 {
                ColorType::L8
            } else {
                ColorType::L16
            };
            let format = PixelFormat::from_color_type(color_type)
                .expect("JPEG 2000 gray color type is supported")
                .at_depth(depth);
            Ok((color_type, format))
        }
        3 => {
            if header.color == EnumeratedColor::Gray {
                return Err(ImgSeqError::new(format!(
                    "image '{}' marks three JPEG 2000 components as grayscale",
                    path.display()
                )));
            }
            if header.color == EnumeratedColor::Sycc
                && let Some(format) = sycc_format(&header.components, depth)
            {
                let color_type = if depth <= 8 {
                    ColorType::Rgb8
                } else {
                    ColorType::Rgb16
                };
                return Ok((color_type, format));
            }
            if header.color != EnumeratedColor::Sycc
                && header
                    .components
                    .iter()
                    .any(|component| (component.dx, component.dy) != (1, 1))
            {
                return Err(ImgSeqError::new(format!(
                    "image '{}' has subsampled JPEG 2000 components without an sYCC color declaration",
                    path.display()
                )));
            }
            let color_type = if depth <= 8 {
                ColorType::Rgb8
            } else {
                ColorType::Rgb16
            };
            let format = PixelFormat::from_color_type(color_type)
                .expect("JPEG 2000 rgb color type is supported")
                .at_depth(depth);
            Ok((color_type, format))
        }
        count => Err(ImgSeqError::new(format!(
            "image '{}' has {count} JPEG 2000 components; only gray and RGB are supported",
            path.display()
        ))),
    }
}

fn sycc_format(components: &[ComponentHeader], depth: u32) -> Option<PixelFormat> {
    let [luma, cb, cr] = components else {
        return None;
    };
    match ((luma.dx, luma.dy), (cb.dx, cb.dy), (cr.dx, cr.dy), depth) {
        ((1, 1), (2, 2), (2, 2), 8) => Some(PixelFormat::Yuv420P8),
        ((1, 1), (2, 2), (2, 2), 10) => Some(PixelFormat::Yuv420P10),
        ((1, 1), (2, 1), (2, 1), 8) => Some(PixelFormat::Yuv422P8),
        ((1, 1), (2, 1), (2, 1), 10) => Some(PixelFormat::Yuv422P10),
        ((1, 1), (1, 1), (1, 1), 8) => Some(PixelFormat::Yuv444P8),
        ((1, 1), (1, 1), (1, 1), 10) => Some(PixelFormat::Yuv444P10),
        ((1, 1), (1, 1), (1, 1), 12) => Some(PixelFormat::Yuv444P12),
        ((1, 1), (1, 1), (1, 1), 16) => Some(PixelFormat::Yuv444P16),
        _ => None,
    }
}

fn check_decoded_header(image: &Image, header: &Header, path: &Path) -> Result<()> {
    if (image.orig_width(), image.orig_height()) != (header.width, header.height)
        || usize::try_from(image.num_components()).ok() != Some(header.components.len())
    {
        return Err(image_error(
            "decode",
            path,
            "OpenJPEG returned dimensions or components different from the header",
        ));
    }
    let components = image.components();
    if components
        .iter()
        .zip(&header.components)
        .any(|(decoded, expected)| {
            decoded.precision() != expected.precision
                || decoded.is_signed() != expected.signed
                || decoded.width() == 0
                || decoded.height() == 0
        })
    {
        return Err(image_error(
            "decode",
            path,
            "OpenJPEG returned component metadata different from the header",
        ));
    }
    Ok(())
}

fn decode_interleaved(image: &Image, color_type: ColorType, path: &Path) -> Result<Pixels> {
    if matches!(image.color_space(), ColorSpace::SYCC) {
        return decode_sycc_rgb(image, color_type, path);
    }
    let data = image
        .get_pixels(None)
        .map_err(|error| image_error("decode", path, error))?;
    let buffer = match (color_type, data.data) {
        (ColorType::L8, ImagePixelData::L8(buffer))
        | (ColorType::Rgb8, ImagePixelData::Rgb8(buffer)) => buffer,
        (ColorType::L16, ImagePixelData::L16(buffer))
        | (ColorType::Rgb16, ImagePixelData::Rgb16(buffer)) => u16_bytes(buffer),
        (expected, actual) => {
            return Err(image_error(
                "decode",
                path,
                format!("OpenJPEG returned {actual:?}, expected {expected:?}"),
            ));
        }
    };
    Ok(Pixels::Interleaved { color_type, buffer })
}

/// Convert an sYCC page to RGB when its SIZ sampling or depth is not one this
/// plugin can hand out as a VapourSynth yuv format. The JP2 enumerated colour
/// space uses full-range sRGB/BT.601 coefficients; supported layouts stay
/// planar and never enter this conversion.
fn decode_sycc_rgb(image: &Image, color_type: ColorType, path: &Path) -> Result<Pixels> {
    let components = image.components();
    if components.len() != 3 {
        return Err(image_error(
            "decode",
            path,
            "OpenJPEG returned an invalid sYCC component count",
        ));
    }
    let width = usize::try_from(image.orig_width())
        .map_err(|_| image_error("decode", path, "image width does not fit in memory"))?;
    let height = usize::try_from(image.orig_height())
        .map_err(|_| image_error("decode", path, "image height does not fit in memory"))?;
    let wide = matches!(color_type, ColorType::Rgb16);
    let planes = components
        .iter()
        .map(|component| {
            let values = if wide {
                component
                    .data_u16()
                    .map(|value| value as f32 / f32::from(u16::MAX))
                    .collect::<Vec<_>>()
            } else {
                component
                    .data_u8()
                    .map(|value| f32::from(value) / f32::from(u8::MAX))
                    .collect::<Vec<_>>()
            };
            (
                usize::try_from(component.width()).unwrap_or(0),
                usize::try_from(component.height()).unwrap_or(0),
                values,
            )
        })
        .collect::<Vec<_>>();
    if planes.iter().any(|(plane_width, plane_height, values)| {
        *plane_width == 0 || *plane_height == 0 || values.len() != plane_width * plane_height
    }) {
        return Err(image_error(
            "decode",
            path,
            "OpenJPEG returned an invalid sYCC plane",
        ));
    }

    if wide {
        let mut buffer = Vec::with_capacity(width.saturating_mul(height).saturating_mul(6));
        for y in 0..height {
            for x in 0..width {
                let luma = sycc_sample(&planes[0], x, y, width, height);
                let cb = sycc_sample(&planes[1], x, y, width, height);
                let cr = sycc_sample(&planes[2], x, y, width, height);
                let r = luma + 1.402 * (cr - 0.5);
                let g = luma - 0.344_136 * (cb - 0.5) - 0.714_136 * (cr - 0.5);
                let b = luma + 1.772 * (cb - 0.5);
                for value in [r, g, b] {
                    buffer.extend_from_slice(
                        &((value.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16)
                            .to_ne_bytes(),
                    );
                }
            }
        }
        return Ok(Pixels::Interleaved { color_type, buffer });
    }

    let mut buffer = Vec::with_capacity(width.saturating_mul(height).saturating_mul(3));
    for y in 0..height {
        for x in 0..width {
            let luma = sycc_sample(&planes[0], x, y, width, height);
            let cb = sycc_sample(&planes[1], x, y, width, height);
            let cr = sycc_sample(&planes[2], x, y, width, height);
            let r = luma + 1.402 * (cr - 0.5);
            let g = luma - 0.344_136 * (cb - 0.5) - 0.714_136 * (cr - 0.5);
            let b = luma + 1.772 * (cb - 0.5);
            buffer.extend([
                (r.clamp(0.0, 1.0) * f32::from(u8::MAX)).round() as u8,
                (g.clamp(0.0, 1.0) * f32::from(u8::MAX)).round() as u8,
                (b.clamp(0.0, 1.0) * f32::from(u8::MAX)).round() as u8,
            ]);
        }
    }
    Ok(Pixels::Interleaved { color_type, buffer })
}

fn sycc_sample(
    plane: &(usize, usize, Vec<f32>),
    x: usize,
    y: usize,
    full_width: usize,
    full_height: usize,
) -> f32 {
    let (width, height, values) = plane;
    let x = (x * width / full_width).min(width.saturating_sub(1));
    let y = (y * height / full_height).min(height.saturating_sub(1));
    values[y * width + x]
}

fn decode_yuv(image: &Image, format: PixelFormat, path: &Path) -> Result<Pixels> {
    if !matches!(image.color_space(), ColorSpace::SYCC) {
        return Err(image_error(
            "decode",
            path,
            "OpenJPEG did not preserve the declared sYCC color space",
        ));
    }
    let components = image.components();
    if components.len() != 3 {
        return Err(image_error(
            "decode",
            path,
            "OpenJPEG returned a non-RGB component count",
        ));
    }
    let planes = components
        .iter()
        .map(|component| {
            if format.bytes_per_sample() == 1 {
                component.data_u8().collect()
            } else {
                u16_bytes(component.data_u16().collect())
            }
        })
        .collect();
    Ok(Pixels::Planar {
        planes,
        alpha: None,
    })
}

fn u16_bytes(values: Vec<u16>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 2);
    for value in values {
        bytes.extend_from_slice(&value.to_ne_bytes());
    }
    bytes
}

fn cicp(color: EnumeratedColor) -> Option<Cicp> {
    match color {
        EnumeratedColor::Srgb => Some(Cicp {
            primaries: 1,
            transfer: 13,
            matrix: 0,
            full_range: true,
        }),
        EnumeratedColor::Gray => Some(Cicp {
            primaries: UNSPECIFIED,
            transfer: 13,
            matrix: UNSPECIFIED,
            full_range: true,
        }),
        EnumeratedColor::Sycc => Some(Cicp {
            primaries: 1,
            transfer: 13,
            matrix: 6,
            full_range: true,
        }),
        EnumeratedColor::Other | EnumeratedColor::Unspecified => None,
    }
}

fn parse_header(data: &[u8], path: &Path) -> Result<Header> {
    if data.starts_with(&JP2_SIGNATURE) {
        parse_jp2(data, path)
    } else if data.starts_with(&J2K_SIGNATURE) {
        let siz = parse_siz(data, path)?;
        header_from_siz(siz, None, EnumeratedColor::Unspecified, false, None, path)
    } else {
        Err(image_error(
            "identify",
            path,
            "the file starts with neither a JPEG 2000 codestream nor a JP2 signature",
        ))
    }
}

/// Reads what describing `path` needs, without reading its picture.
///
/// A JP2 file's header boxes come before its codestream, so this reads a window
/// over the front and grows it while the header is incomplete. An incomplete
/// header fails the walk with an error, and that error cannot be told apart from
/// a real one, so the window only ever grows: the last attempt covers the whole
/// file and reports exactly what the single whole-file read reported before.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or its header cannot be
/// described.
fn read_description(file: &mut BufReader<File>, path: &Path) -> Result<Header> {
    let length = file
        .seek(SeekFrom::End(0))
        .map_err(|error| image_error("open", path, error))?;
    let mut window = PROBE_WINDOW.min(length);
    loop {
        // Each attempt reads the front of the file again out of the one open, so
        // growing the window costs a seek and a read rather than another open.
        // This loop used to reopen the file every time it grew, which is what
        // made a header that outruns the first window cost a chain of opens.
        file.seek(SeekFrom::Start(0))
            .map_err(|error| image_error("open", path, error))?;
        let mut data = Vec::new();
        file.by_ref()
            .take(window)
            .read_to_end(&mut data)
            .map_err(|error| image_error("read", path, error))?;
        match parse_header(&data, path) {
            Ok(header) => return Ok(header),
            Err(error) if window >= length => return Err(error),
            Err(_) => window = window.saturating_mul(2).min(length),
        }
    }
}

fn parse_jp2(data: &[u8], path: &Path) -> Result<Header> {
    let mut state = Jp2State::default();
    walk_boxes(data, 0, data.len(), &mut state, path)?;
    let siz = state
        .siz
        .ok_or_else(|| image_error("identify", path, "the JP2 file has no SIZ marker"))?;
    header_from_siz(
        siz,
        state.ihdr,
        state.color,
        state.has_icc_profile,
        state.icc_profile,
        path,
    )
}

#[derive(Default)]
struct Jp2State {
    ihdr: Option<(u32, u32, u16, Option<u32>)>,
    siz: Option<SizHeader>,
    color: EnumeratedColor,
    has_icc_profile: bool,
    icc_profile: Option<Arc<[u8]>>,
}

fn walk_boxes(
    data: &[u8],
    mut offset: usize,
    end: usize,
    state: &mut Jp2State,
    path: &Path,
) -> Result<()> {
    while offset < end {
        if end - offset < 8 {
            return Err(image_error(
                "identify",
                path,
                "a JP2 box header is truncated",
            ));
        }
        let length = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap());
        let box_type = &data[offset + 4..offset + 8];
        let (header_size, box_len) = match length {
            0 => (8, end - offset),
            1 => {
                if end - offset < 16 {
                    return Err(image_error(
                        "identify",
                        path,
                        "a JP2 extended box header is truncated",
                    ));
                }
                let length = u64::from_be_bytes(data[offset + 8..offset + 16].try_into().unwrap());
                let length = usize::try_from(length)
                    .map_err(|_| image_error("identify", path, "a JP2 box is too large"))?;
                (16, length)
            }
            length => (8, usize::try_from(length).unwrap()),
        };
        if box_len < header_size {
            return Err(image_error(
                "identify",
                path,
                "a JP2 box extends past the file",
            ));
        }
        let payload_start = offset + header_size;
        // A box that runs past the bytes read so far is either a header the
        // caller has to grow its window for, or the codestream box: a probe reads
        // only the `SIZ` at the start of that one and nothing after it, so a
        // codestream whose own length runs past the data still describes the
        // file. The codestream is the last box a JP2 file writes, so nothing a
        // description needs is left behind by stopping here.
        if box_len > end - offset {
            if box_type == b"jp2c" && state.siz.is_none() {
                state.siz = Some(parse_siz(&data[payload_start..end], path)?);
                return Ok(());
            }
            return Err(image_error(
                "identify",
                path,
                "a JP2 box extends past the file",
            ));
        }
        let payload_start = offset + header_size;
        let box_end = offset + box_len;
        let payload = &data[payload_start..box_end];
        match box_type {
            b"jp2h" => walk_boxes(data, payload_start, box_end, state, path)?,
            b"ihdr" => parse_ihdr(payload, state, path)?,
            b"colr" => parse_colr(payload, state, path)?,
            b"jp2c" if state.siz.is_none() => state.siz = Some(parse_siz(payload, path)?),
            b"jp2c" => {}
            _ => {}
        }
        offset = box_end;
    }
    Ok(())
}

fn parse_ihdr(payload: &[u8], state: &mut Jp2State, path: &Path) -> Result<()> {
    if payload.len() < 14 {
        return Err(image_error(
            "identify",
            path,
            "the JP2 ihdr box is truncated",
        ));
    }
    let height = u32::from_be_bytes(payload[..4].try_into().unwrap());
    let width = u32::from_be_bytes(payload[4..8].try_into().unwrap());
    let components = u16::from_be_bytes(payload[8..10].try_into().unwrap());
    let depth = match payload[10] {
        0xff => None,
        value => Some(u32::from(value & 0x7f) + 1),
    };
    state.ihdr = Some((width, height, components, depth));
    Ok(())
}

fn parse_colr(payload: &[u8], state: &mut Jp2State, path: &Path) -> Result<()> {
    if payload.len() < 3 {
        return Err(image_error(
            "identify",
            path,
            "the JP2 colr box is truncated",
        ));
    }
    match payload[0] {
        1 if payload.len() >= 7 => {
            let enumerated = u32::from_be_bytes(payload[3..7].try_into().unwrap());
            state.color = match enumerated {
                16 => EnumeratedColor::Srgb,
                17 => EnumeratedColor::Gray,
                18 => EnumeratedColor::Sycc,
                _ => EnumeratedColor::Other,
            };
        }
        2 => {
            state.has_icc_profile = true;
            if state.icc_profile.is_none() {
                state.icc_profile = Some(Arc::from(&payload[3..]));
            }
        }
        _ => state.color = EnumeratedColor::Unspecified,
    }
    Ok(())
}

fn parse_siz(data: &[u8], path: &Path) -> Result<SizHeader> {
    let marker = data
        .windows(2)
        .position(|bytes| bytes == [0xff, 0x51])
        .ok_or_else(|| image_error("identify", path, "the JPEG 2000 file has no SIZ marker"))?;
    if data.len() - marker < 40 {
        return Err(image_error(
            "identify",
            path,
            "the JPEG 2000 SIZ marker is truncated",
        ));
    }
    let length = usize::from(u16::from_be_bytes(
        data[marker + 2..marker + 4].try_into().unwrap(),
    ));
    if length < 38 || marker + 2 + length > data.len() {
        return Err(image_error(
            "identify",
            path,
            "the JPEG 2000 SIZ marker has an invalid length",
        ));
    }
    let x_size = u32::from_be_bytes(data[marker + 6..marker + 10].try_into().unwrap());
    let y_size = u32::from_be_bytes(data[marker + 10..marker + 14].try_into().unwrap());
    let x_offset = u32::from_be_bytes(data[marker + 14..marker + 18].try_into().unwrap());
    let y_offset = u32::from_be_bytes(data[marker + 18..marker + 22].try_into().unwrap());
    if x_size <= x_offset || y_size <= y_offset {
        return Err(image_error(
            "identify",
            path,
            "the JPEG 2000 SIZ dimensions are invalid",
        ));
    }
    let component_count = u16::from_be_bytes(data[marker + 38..marker + 40].try_into().unwrap());
    if component_count == 0 || component_count > 3 {
        return Err(ImgSeqError::new(format!(
            "image '{}' has {component_count} JPEG 2000 components; only gray and RGB are supported",
            path.display()
        )));
    }
    let component_start = marker + 40;
    let component_bytes = usize::from(component_count).checked_mul(3).ok_or_else(|| {
        image_error(
            "identify",
            path,
            "the JPEG 2000 component list is too large",
        )
    })?;
    if component_start + component_bytes > marker + 2 + length {
        return Err(image_error(
            "identify",
            path,
            "the JPEG 2000 component list is truncated",
        ));
    }
    let components: Vec<ComponentHeader> = (0..usize::from(component_count))
        .map(|index| {
            let offset = component_start + index * 3;
            let ssiz = data[offset];
            ComponentHeader {
                precision: u32::from(ssiz & 0x7f) + 1,
                signed: ssiz & 0x80 != 0,
                dx: data[offset + 1],
                dy: data[offset + 2],
            }
        })
        .collect();
    if components
        .iter()
        .any(|component| component.dx == 0 || component.dy == 0)
    {
        return Err(image_error(
            "identify",
            path,
            "the JPEG 2000 component sampling is invalid",
        ));
    }
    Ok(SizHeader {
        width: x_size - x_offset,
        height: y_size - y_offset,
        components,
    })
}

fn header_from_siz(
    siz: SizHeader,
    ihdr: Option<(u32, u32, u16, Option<u32>)>,
    color: EnumeratedColor,
    has_icc_profile: bool,
    icc_profile: Option<Arc<[u8]>>,
    path: &Path,
) -> Result<Header> {
    if let Some((width, height, components, depth)) = ihdr {
        if (width, height, usize::from(components)) != (siz.width, siz.height, siz.components.len())
        {
            return Err(image_error(
                "identify",
                path,
                "the JP2 ihdr and codestream dimensions disagree",
            ));
        }
        if depth.is_some_and(|depth| {
            siz.components
                .iter()
                .any(|component| component.precision != depth)
        }) {
            return Err(image_error(
                "identify",
                path,
                "the JP2 ihdr and codestream precision disagree",
            ));
        }
    }
    let color = if color == EnumeratedColor::Unspecified {
        if siz.components.len() == 1 {
            EnumeratedColor::Gray
        } else {
            EnumeratedColor::Other
        }
    } else {
        color
    };
    Ok(Header {
        width: siz.width,
        height: siz.height,
        components: siz.components,
        color,
        has_icc_profile,
        icc_profile,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn siz(width: u32, height: u32, components: &[(u8, u8, u8)]) -> Vec<u8> {
        let length = 38 + components.len() * 3;
        let mut data = vec![0xff, 0x4f, 0xff, 0x51];
        data.extend_from_slice(&(u16::try_from(length).unwrap()).to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&(u16::try_from(components.len()).unwrap()).to_be_bytes());
        for component in components {
            data.extend_from_slice(&[component.0, component.1, component.2]);
        }
        data
    }

    #[test]
    fn reads_a_bare_codestream_header() {
        let data = siz(640, 480, &[(7, 1, 1), (7, 1, 1), (7, 1, 1)]);
        let header = parse_header(&data, Path::new("image.j2k")).unwrap();
        assert_eq!((header.width, header.height), (640, 480));
        assert_eq!(header.components.len(), 3);
        assert_eq!(header.components[0].precision, 8);
    }

    /// What this module answered for itself is [`identify::route`] now, so this
    /// pins the rule every module routes by rather than a copy of it.
    #[test]
    fn accepts_the_supported_extensions_case_insensitively() {
        let route = crate::formats::identify::route;
        let jp2 = Some(crate::formats::identify::Format::Jp2);
        for extension in crate::formats::identify::Format::Jp2.extensions() {
            assert_eq!(route(Path::new(&format!("page.{extension}"))), jp2);
            assert_eq!(
                route(Path::new(&format!("page.{}", extension.to_uppercase()))),
                jp2
            );
        }
        // A name that only looks like one, and a name that is another format.
        assert_eq!(route(Path::new("page.jp2.zip")), None);
        assert_ne!(route(Path::new("page.png")), jp2);
    }
    #[test]
    fn maps_srgb_and_s_ycc_to_their_frame_formats() {
        let rgb = Header {
            width: 2,
            height: 2,
            components: vec![
                ComponentHeader {
                    precision: 8,
                    signed: false,
                    dx: 1,
                    dy: 1,
                },
                ComponentHeader {
                    precision: 8,
                    signed: false,
                    dx: 1,
                    dy: 1,
                },
                ComponentHeader {
                    precision: 8,
                    signed: false,
                    dx: 1,
                    dy: 1,
                },
            ],
            color: EnumeratedColor::Srgb,
            has_icc_profile: false,
            icc_profile: None,
        };
        assert_eq!(
            output_format(&rgb, Path::new("rgb.jp2")).unwrap().1,
            PixelFormat::Rgb8
        );

        for (depth, chroma, expected) in [
            (8, (2, 2), PixelFormat::Yuv420P8),
            (10, (2, 2), PixelFormat::Yuv420P10),
            (8, (2, 1), PixelFormat::Yuv422P8),
            (10, (2, 1), PixelFormat::Yuv422P10),
            (8, (1, 1), PixelFormat::Yuv444P8),
            (10, (1, 1), PixelFormat::Yuv444P10),
            (12, (1, 1), PixelFormat::Yuv444P12),
            (16, (1, 1), PixelFormat::Yuv444P16),
        ] {
            let components = [(1, 1), chroma, chroma]
                .into_iter()
                .map(|(dx, dy)| ComponentHeader {
                    precision: depth,
                    signed: false,
                    dx,
                    dy,
                })
                .collect();
            let yuv = Header {
                color: EnumeratedColor::Sycc,
                components,
                ..rgb.clone()
            };
            assert_eq!(
                output_format(&yuv, Path::new("yuv.jp2")).unwrap().1,
                expected,
                "depth={depth}, chroma={chroma:?}"
            );
        }
    }

    /// A signed component, and components of two widths, are refused by name.
    ///
    /// VapourSynth has no signed integer format, so a signed component cannot be
    /// handed out as the numbers it holds, and a frame's format names one depth,
    /// so components of two widths would have to be widened into it. Both are
    /// refusals at the probe, and both messages say which component and which
    /// width, because one sentence covering both leaves a user no way to tell
    /// them apart or to find the component.
    #[test]
    fn a_signed_or_mixed_precision_page_is_refused_by_name() {
        let base = Header {
            width: 2,
            height: 2,
            components: vec![
                ComponentHeader {
                    precision: 8,
                    signed: false,
                    dx: 1,
                    dy: 1,
                },
                ComponentHeader {
                    precision: 8,
                    signed: false,
                    dx: 1,
                    dy: 1,
                },
                ComponentHeader {
                    precision: 8,
                    signed: false,
                    dx: 1,
                    dy: 1,
                },
            ],
            color: EnumeratedColor::Srgb,
            has_icc_profile: false,
            icc_profile: None,
        };

        let signed = Header {
            components: base
                .components
                .iter()
                .enumerate()
                .map(|(index, component)| ComponentHeader {
                    signed: index == 1,
                    ..*component
                })
                .collect(),
            ..base.clone()
        };
        let said = output_format(&signed, Path::new("signed.jp2"))
            .expect_err("a signed component has no VapourSynth format")
            .to_string();
        assert!(said.contains("component 2 of 3"), "{said}");
        assert!(said.contains("signed"), "{said}");
        assert!(said.contains("no signed integer format"), "{said}");

        let mixed = Header {
            components: base
                .components
                .iter()
                .enumerate()
                .map(|(index, component)| ComponentHeader {
                    precision: if index == 2 { 12 } else { 8 },
                    ..*component
                })
                .collect(),
            ..base.clone()
        };
        let said = output_format(&mixed, Path::new("mixed.jp2"))
            .expect_err("components of two widths are refused")
            .to_string();
        assert!(said.contains("different precision"), "{said}");
        assert!(said.contains("component 1 is 8 bits"), "{said}");
        assert!(said.contains("component 3 is 12 bits"), "{said}");
    }

    /// A box with `kind` and `payload`, as the container writes one.
    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&u32::try_from(payload.len() + 8).unwrap().to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    /// An `ihdr` payload for a three component image of `width` by `height`.
    fn ihdr_payload(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&3u16.to_be_bytes());
        out.push(7);
        out.extend_from_slice(&[7, 0, 0]);
        out
    }

    /// Writes bytes to a temp file named for this test process.
    fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("imgseqs-jp2-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).expect("a writable image");
        path
    }

    /// The header boxes of a JP2 file, followed by a codestream box that states
    /// `width` by `height`, with `padding` written before the header.
    fn jp2_file(width: u32, height: u32, padding: usize) -> Vec<u8> {
        let components = [(7, 1, 1), (7, 1, 1), (7, 1, 1)];
        let header = boxed(
            b"jp2h",
            &[
                boxed(b"ihdr", &ihdr_payload(width, height)),
                boxed(b"colr", &[1, 0, 0, 0, 0, 0, 16]),
            ]
            .concat(),
        );
        let mut file = Vec::from(JP2_SIGNATURE);
        file.extend_from_slice(&boxed(b"free", &vec![0; padding]));
        file.extend_from_slice(&header);
        file.extend_from_slice(&boxed(b"jp2c", &siz(width, height, &components)));
        file
    }

    /// A header past the first window is still described: the window grows until
    /// the walk completes, and the last attempt is the whole file.
    #[test]
    fn a_header_past_the_probe_window_is_still_read() {
        // A `free` box larger than the window pushes `jp2h` past it, which is
        // what a file padded before its header looks like.
        let past = jp2_file(64, 32, usize::try_from(PROBE_WINDOW).unwrap() + 1024);
        let path = write_temp("past-window.jp2", &past);
        let info = image_info_at(&path, true).expect("a padded header to describe");
        assert_eq!((info.width, info.height), (64, 32));
        let _ = std::fs::remove_file(&path);

        // The same file with its header inside the window, so both sides of the
        // growth are covered.
        let inside = jp2_file(64, 32, 16);
        let path = write_temp("inside-window.jp2", &inside);
        let info = image_info_at(&path, true).expect("a header in the window to describe");
        assert_eq!((info.width, info.height), (64, 32));
        let _ = std::fs::remove_file(&path);
    }
}
