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
    path::Path,
    time::{Duration, Instant},
};

use image::ColorType;
use jpeg2k::{ColorSpace, Image, ImagePixelData};

use crate::{
    color::{Cicp, UNSPECIFIED},
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    pixel::{PixelFormat, Transform},
};

const JP2_SIGNATURE: [u8; 12] = [0, 0, 0, 12, b'j', b'P', b' ', b' ', 0x0d, 0x0a, 0x87, 0x0a];
const J2K_SIGNATURE: [u8; 4] = [0xff, 0x4f, 0xff, 0x51];

/// Extensions whose contents this module owns.
const EXTENSIONS: [&str; 5] = ["jp2", "j2k", "jpf", "jpx", "j2c"];

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
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SizHeader {
    width: u32,
    height: u32,
    components: Vec<ComponentHeader>,
}

/// Whether a path has a JPEG 2000 extension and should be validated by this
/// module before the general image decoder gets a chance to identify it.
pub fn owns(path: &Path) -> bool {
    has_extension(path)
}

/// Whether this module decodes the probed image.
pub fn handles(info: &ImageInfo) -> bool {
    has_extension(&info.path)
}

fn has_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// Probe one JPEG 2000 image without asking OpenJPEG to decode its pixels.
pub fn image_info(path: &Path, _apply_rotation: bool) -> Result<ImageInfo> {
    let data = std::fs::read(path).map_err(|error| image_error("open", path, error))?;
    let header = parse_header(&data, path)?;
    let (color_type, format) = output_format(&header, path)?;

    Ok(ImageInfo {
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type,
        original_color_type: color_type.into(),
        has_icc_profile: header.has_icc_profile,
        cicp: cicp(header.color),
        chroma_location: None,
        orientation: image::metadata::Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format,
    })
}

/// Decode a JPEG 2000 image into an interleaved buffer or its coded yuv planes.
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
    if depth == 0
        || header
            .components
            .iter()
            .any(|component| component.precision != depth || component.signed)
    {
        return Err(ImgSeqError::new(format!(
            "image '{}' has unsupported JPEG 2000 component precision or signed samples",
            path.display()
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
        header_from_siz(siz, None, EnumeratedColor::Unspecified, false, path)
    } else {
        Err(image_error(
            "identify",
            path,
            "the file starts with neither a JPEG 2000 codestream nor a JP2 signature",
        ))
    }
}

fn parse_jp2(data: &[u8], path: &Path) -> Result<Header> {
    let mut state = Jp2State::default();
    walk_boxes(data, 0, data.len(), &mut state, path)?;
    let siz = state
        .siz
        .ok_or_else(|| image_error("identify", path, "the JP2 file has no SIZ marker"))?;
    header_from_siz(siz, state.ihdr, state.color, state.has_icc_profile, path)
}

#[derive(Default)]
struct Jp2State {
    ihdr: Option<(u32, u32, u16, Option<u32>)>,
    siz: Option<SizHeader>,
    color: EnumeratedColor,
    has_icc_profile: bool,
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
        if box_len < header_size || box_len > end - offset {
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
        2 => state.has_icc_profile = true,
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

    #[test]
    fn accepts_the_supported_extensions_case_insensitively() {
        for extension in EXTENSIONS {
            assert!(has_extension(Path::new(&format!("page.{extension}"))));
            assert!(has_extension(Path::new(&format!(
                "page.{}",
                extension.to_uppercase()
            ))));
        }
        assert!(!has_extension(Path::new("page.jp2.zip")));
        assert!(!has_extension(Path::new("page.png")));
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
                ..rgb
            };
            assert_eq!(
                output_format(&yuv, Path::new("yuv.jp2")).unwrap().1,
                expected,
                "depth={depth}, chroma={chroma:?}"
            );
        }
    }
}
