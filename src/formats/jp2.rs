//! JPEG 2000 decoding through the `jpeg2k` crate and its vendored OpenJPEG.
//!
//! JPEG 2000 has two useful forms: a bare codestream (`.j2k`/`.j2c`) and a
//! JP2-family box container (`.jp2`, `.jpf`, and `.jpx`). The `image` crate
//! does not describe either one, so this module reads the SIZ and JP2 header
//! records directly during probing. OpenJPEG is only entered when a frame is
//! requested.
//!
//! JPEG 2000 has no alpha convention this source can preserve. One or three
//! JPEG 2000 has no alpha convention of its own: what a component means is the
//! container's statement, and a JP2 makes it in its `cdef` box. One colour
//! component with association 1 beside one opacity component is gray and alpha,
//! three colour components beside one opacity is r,g,b and alpha, and the
//! component count alone cannot tell those apart from three colour channels or a
//! colour channel beside something else. A file whose `cdef` names nothing -- a
//! bare codestream has no box to name anything in -- keeps the count rule: one or
//! three components, and a two component image with no statement of which sample
//! is alpha is refused rather than guessed at.

use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::layout::ColorType;
use jpeg2k::{ColorSpace, DecodeParameters, Image, ImagePixelData};

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

/// One channel definition of a JP2 `cdef` box.
///
/// The box assigns every component a type and an association: what the component
/// holds, and which colour channel it is or that it covers the image as a whole.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Channel {
    component: u16,
    kind: u16,
    association: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Header {
    width: u32,
    height: u32,
    components: Vec<ComponentHeader>,
    color: EnumeratedColor,
    has_icc_profile: bool,
    icc_profile: Option<Arc<[u8]>>,
    /// The container's channel definitions, when it states them.
    channels: Option<Vec<Channel>>,
    /// What the container's `pclr` box states, when it states a palette.
    palette: Option<Palette>,
    /// Whether the container states a component mapping beside its palette.
    palette_mapping: bool,
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
    // A palette page is decoded with its palette left alone: the codestream holds
    // indices and the colour is in the container, so this reader asks for the
    // codestream's own components and expands them itself.
    let image = if header.palette.is_some() {
        Image::from_bytes_with(&data, DecodeParameters::new().ignore_pclr_cmap_cdef())
    } else {
        Image::from_bytes(&data)
    }
    .map_err(|error| image_error("decode", &info.path, error))?;
    check_decoded_header(&image, &header, &info.path)?;
    let pixels = if let Some(palette) = &header.palette {
        palette_buffer(&image, palette, &info.path)?
    } else if format.color_family() == vapoursynth4_rs::ColorFamily::YUV {
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

    // The container's own statement comes first. A `cdef` box names which
    // A palette is expanded here rather than by the codec: the codestream holds
    // indices and the colour is in the `pclr` box beside them, so the shape the
    // frame is handed out as is the palette's own. The decode reads the indices with
    // the palette left alone and expands them itself; see [`palette_buffer`].
    if let Some(palette) = &header.palette {
        if !header.palette_mapping {
            return Err(ImgSeqError::new(format!(
                "image '{}' states a JP2 palette with no component mapping, which this reader cannot expand",
                path.display()
            )));
        }
        let color_type = match (palette.components, palette.bits) {
            (3, 8) => ColorType::Rgb8,
            (3, 16) => ColorType::Rgb16,
            (4, 8) => ColorType::Rgba8,
            (4, 16) => ColorType::Rgba16,
            (components, bits) => {
                return Err(ImgSeqError::new(format!(
                    "image '{}' states a JP2 palette of {components} components of {bits} bits, which this reader does not expand",
                    path.display()
                )));
            }
        };
        let format = PixelFormat::from_color_type(color_type)
            .expect("a JPEG 2000 palette color type is supported");
        return Ok((color_type, format));
    }
    // component is colour and which is opacity, and the component count cannot:
    // two components might be gray and alpha, or a colour channel beside
    // something else. A file that states no `cdef` -- a bare codestream has no
    // box to state one in -- keeps the count rule below rather than being
    // guessed at.
    if let Some(channels) = &header.channels
        && channels.len() == header.components.len()
    {
        let color = channels
            .iter()
            .filter(|channel| channel.kind == CHANNEL_COLOR)
            .count();
        let opacity = channels
            .iter()
            .filter(|channel| matches!(channel.kind, CHANNEL_OPACITY | CHANNEL_PREMULTIPLIED))
            .count();
        match (color, opacity) {
            (1, 1) => return Ok(gray_alpha(depth)),
            (3, 1) => return Ok(rgba(depth)),
            _ => {}
        }
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
            "image '{}' has {count} JPEG 2000 components; only gray, gray+alpha, RGB and RGBA are supported",
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

/// The layout and format of a gray page with an opacity channel.
fn gray_alpha(depth: u32) -> (ColorType, PixelFormat) {
    let color_type = if depth <= 8 {
        ColorType::La8
    } else {
        ColorType::La16
    };
    let format = PixelFormat::from_color_type(color_type)
        .expect("JPEG 2000 gray+alpha color type is supported")
        .at_depth(depth);
    (color_type, format)
}

/// The layout and format of an r,g,b page with an opacity channel.
fn rgba(depth: u32) -> (ColorType, PixelFormat) {
    let color_type = if depth <= 8 {
        ColorType::Rgba8
    } else {
        ColorType::Rgba16
    };
    let format = PixelFormat::from_color_type(color_type)
        .expect("JPEG 2000 rgba color type is supported")
        .at_depth(depth);
    (color_type, format)
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

/// Expands a palette page's indices into the colour its `pclr` box states.
///
/// The codestream holds one index a pixel, which is why the decode asks OpenJPEG
/// for the codestream's own components rather than for the picture it would build
/// from the palette itself. What is left is the expansion the TIFF and png palette
/// paths already make: one entry looked up per index, into a frame of the palette's
/// own shape.
///
/// # Errors
///
/// Returns [`ImgSeqError`] for a page whose index component is missing or empty and
/// for an index past the end of the palette.
fn palette_buffer(image: &Image, palette: &Palette, path: &Path) -> Result<Pixels> {
    let components = image.components();
    let Some(index) = components.first() else {
        return Err(image_error(
            "decode",
            path,
            "the palette page holds no index component",
        ));
    };
    let width = usize::try_from(index.width()).unwrap_or(0);
    let height = usize::try_from(index.height()).unwrap_or(0);
    if width == 0 || height == 0 {
        return Err(image_error(
            "decode",
            path,
            "the palette page has no pixels",
        ));
    }
    let channels = usize::from(palette.components);
    let wide = palette.bits == 16;
    // The crate hands a component over scaled into the word it fits, so the index is
    // scaled back to the precision the codestream states before it is looked up.
    let scale = if palette.bits >= 16 {
        1.0
    } else {
        f64::from((1u32 << u32::from(palette.bits)) - 1) / f64::from(u16::MAX)
    };
    let mut buffer = Vec::with_capacity(width * height * channels * if wide { 2 } else { 1 });
    for value in index.data_u16() {
        let index = ((f64::from(value) * scale).round() as u32) as usize;
        let at = index * channels;
        let Some(entry) = palette.values.get(at..at + channels) else {
            return Err(image_error(
                "decode",
                path,
                "a palette index is past the end of the palette",
            ));
        };
        for channel in entry {
            if wide {
                buffer
                    .extend_from_slice(&u16::try_from(*channel).unwrap_or(u16::MAX).to_ne_bytes());
            } else {
                buffer.push(u8::try_from(*channel).unwrap_or(u8::MAX));
            }
        }
    }
    let color_type = match (channels, wide) {
        (3, false) => ColorType::Rgb8,
        (3, true) => ColorType::Rgb16,
        (4, false) => ColorType::Rgba8,
        _ => ColorType::Rgba16,
    };
    Ok(Pixels::Interleaved { color_type, buffer })
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
        | (ColorType::La8, ImagePixelData::La8(buffer))
        | (ColorType::Rgb8, ImagePixelData::Rgb8(buffer))
        | (ColorType::Rgba8, ImagePixelData::Rgba8(buffer)) => buffer,
        (ColorType::L16, ImagePixelData::L16(buffer))
        | (ColorType::La16, ImagePixelData::La16(buffer))
        | (ColorType::Rgb16, ImagePixelData::Rgb16(buffer))
        | (ColorType::Rgba16, ImagePixelData::Rgba16(buffer)) => u16_bytes(buffer),
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
        header_from_siz(siz, &Jp2State::default(), path)
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
        .clone()
        .ok_or_else(|| image_error("identify", path, "the JP2 file has no SIZ marker"))?;
    header_from_siz(siz, &state, path)
}

#[derive(Default)]
struct Jp2State {
    ihdr: Option<(u32, u32, u16, Option<u32>)>,
    siz: Option<SizHeader>,
    color: EnumeratedColor,
    has_icc_profile: bool,
    icc_profile: Option<Arc<[u8]>>,
    channels: Option<Vec<Channel>>,
    palette: Option<Palette>,
    /// Whether the container states a component mapping beside its palette.
    palette_mapping: bool,
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
            b"cdef" => parse_cdef(payload, state, path)?,
            b"pclr" => parse_pclr(payload, state, path)?,
            b"cmap" => state.palette_mapping = true,
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

/// What a JP2 `cdef` box says about one component.
const CHANNEL_COLOR: u16 = 0;
/// An opacity component, which is what the alpha clip is built from.
const CHANNEL_OPACITY: u16 = 1;
/// A colour that already has its alpha folded in. This reader hands the samples
/// out as they are and does not unpremultiply, so it is an opacity component
/// like any other for the purpose of finding the clip's alpha channel.
const CHANNEL_PREMULTIPLIED: u16 = 2;

fn parse_cdef(payload: &[u8], state: &mut Jp2State, path: &Path) -> Result<()> {
    if payload.len() < 2 {
        return Err(image_error(
            "identify",
            path,
            "the JP2 cdef box is truncated",
        ));
    }
    let count = usize::from(u16::from_be_bytes(payload[..2].try_into().unwrap()));
    // Every definition is a component index, a type and an association.
    let Some(definitions) = payload.get(2..2 + count.saturating_mul(6)) else {
        return Err(image_error(
            "identify",
            path,
            "the JP2 cdef box is truncated",
        ));
    };
    state.channels = Some(
        definitions
            .as_chunks::<6>()
            .0
            .iter()
            .map(|definition| Channel {
                component: u16::from_be_bytes(definition[..2].try_into().unwrap()),
                kind: u16::from_be_bytes(definition[2..4].try_into().unwrap()),
                association: u16::from_be_bytes(definition[4..6].try_into().unwrap()),
            })
            .collect(),
    );
    Ok(())
}

/// What a JP2 `pclr` box states: how many entries a palette holds, how many
/// components each entry has, how many bits each component is, and the entries
/// themselves, entry-major.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Palette {
    entries: u16,
    components: u8,
    bits: u8,
    values: Vec<u32>,
}

/// Reads the palette a JP2 states.
///
/// The box is `NE`, `NPC`, one `Bi` for each of the `NPC` palette columns, and
/// then the entries: every entry is `NPC` values of its column's width. `Bi` counts
/// the bits *minus one*, the way a codestream's `Ssiz` states a precision, so eight
/// bits a column is a seven in the box.
fn parse_pclr(payload: &[u8], state: &mut Jp2State, path: &Path) -> Result<()> {
    if payload.len() < 3 {
        return Err(image_error(
            "identify",
            path,
            "the JP2 pclr box is truncated",
        ));
    }
    let entries = u16::from_be_bytes(payload[..2].try_into().unwrap());
    let components = payload[2];
    if entries == 0 || components == 0 {
        return Err(image_error(
            "identify",
            path,
            "the JP2 pclr box states a palette no reader can expand",
        ));
    }
    let Some(columns) = payload.get(3..3 + usize::from(components)) else {
        return Err(image_error(
            "identify",
            path,
            "the JP2 pclr box is truncated",
        ));
    };
    let bits: Vec<u32> = columns
        .iter()
        .map(|column| u32::from(column & 0x7f) + 1)
        .collect();
    // Every column's values are read at its own width, and the entries follow the
    // columns in the box.
    let mut values = Vec::with_capacity(usize::from(entries) * usize::from(components));
    let mut at = 3 + usize::from(components);
    for _ in 0..entries {
        for width in &bits {
            let bytes = usize::try_from(width.div_ceil(8)).unwrap_or(0);
            let Some(raw) = payload.get(at..at + bytes) else {
                return Err(image_error(
                    "identify",
                    path,
                    "the JP2 pclr box is truncated",
                ));
            };
            let mut value = 0u32;
            for byte in raw {
                value = (value << 8) | u32::from(*byte);
            }
            values.push(value);
            at += bytes;
        }
    }
    // One width for the whole palette is what this reader expands: a palette of two
    // widths would have to be widened into the one frame format.
    let Some(first) = bits.first().copied() else {
        return Err(image_error(
            "identify",
            path,
            "the JP2 pclr box states a palette no reader can expand",
        ));
    };
    if bits.iter().any(|width| *width != first) || !matches!(first, 8 | 16) {
        return Err(image_error(
            "identify",
            path,
            format!(
                "the JP2 palette of '{}' is {first} bits a column, which this reader does not expand",
                path.display()
            ),
        ));
    }
    state.palette = Some(Palette {
        entries,
        components,
        bits: u8::try_from(first).unwrap_or(8),
        values,
    });
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
    if component_count == 0 || component_count > 4 {
        return Err(ImgSeqError::new(format!(
            "image '{}' has {component_count} JPEG 2000 components; only gray, gray+alpha, RGB and RGBA are supported",
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

/// What a JP2's boxes state about it, which the codestream's own SIZ does not.
///
/// The walk fills one of these in as it reads the header boxes, and the header
/// the probe describes is the two of them together. A bare codestream has no
/// boxes at all, which is what [`Jp2State::default`] stands for.
fn header_from_siz(siz: SizHeader, state: &Jp2State, path: &Path) -> Result<Header> {
    if let Some((width, height, components, depth)) = state.ihdr {
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
    let color = if state.color == EnumeratedColor::Unspecified {
        if siz.components.len() == 1 {
            EnumeratedColor::Gray
        } else {
            EnumeratedColor::Other
        }
    } else {
        state.color
    };
    Ok(Header {
        width: siz.width,
        height: siz.height,
        components: siz.components,
        color,
        has_icc_profile: state.has_icc_profile,
        icc_profile: state.icc_profile.clone(),
        channels: state.channels.clone(),
        palette: state.palette.clone(),
        palette_mapping: state.palette_mapping,
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
            channels: None,
            palette: None,
            palette_mapping: false,
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
            channels: None,
            palette: None,
            palette_mapping: false,
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

    /// A `cdef` box names which component is alpha, which is what tells a
    /// two-component file apart from one whose second component means something
    /// else. The component count cannot, so a file that states no `cdef` keeps the
    /// count rule and its refusal.
    #[test]
    fn channel_definitions_name_the_alpha_component() {
        let gray = ComponentHeader {
            precision: 8,
            signed: false,
            dx: 1,
            dy: 1,
        };
        let color = |association: u16| Channel {
            component: association - 1,
            kind: CHANNEL_COLOR,
            association,
        };
        let opacity = Channel {
            component: 1,
            kind: CHANNEL_OPACITY,
            association: 0,
        };

        // The fields every header in this test shares: a size, the colour, the
        // profile, and the channel definitions each case states for itself.
        let base = Header {
            width: 2,
            height: 2,
            components: vec![gray, gray, gray],
            color: EnumeratedColor::Srgb,
            has_icc_profile: false,
            icc_profile: None,
            channels: None,
            palette: None,
            palette_mapping: false,
        };
        let gray_alpha = Header {
            components: vec![gray, gray],
            channels: Some(vec![color(1), opacity]),
            ..base.clone()
        };
        assert_eq!(
            output_format(&gray_alpha, Path::new("grayalpha.jp2")).unwrap(),
            (ColorType::La8, PixelFormat::Gray8)
        );

        // Three colour channels beside one opacity is r,g,b and alpha, and the
        // opacity channel is the fourth component rather than the second.
        let rgba = Header {
            components: vec![gray, gray, gray, gray],
            channels: Some(vec![
                color(1),
                color(2),
                color(3),
                Channel {
                    component: 3,
                    kind: CHANNEL_OPACITY,
                    association: 0,
                },
            ]),
            ..base.clone()
        };
        assert_eq!(
            output_format(&rgba, Path::new("rgba.jp2")).unwrap(),
            (ColorType::Rgba8, PixelFormat::Rgb8)
        );

        // A `cdef` that names fewer channels than the file holds is not a
        // statement about this file's components, so the count rule stands and the
        // two-component file is refused.
        let mismatched = Header {
            components: vec![gray, gray],
            channels: Some(vec![color(1)]),
            ..base.clone()
        };
        let said = output_format(&mismatched, Path::new("two.jp2"))
            .expect_err("an unlabelled two-component file is refused")
            .to_string();
        assert!(said.contains("2 JPEG 2000 components"), "{said}");
    }

    /// A palette page is expanded here rather than by the codec: the codestream holds
    /// indices and the colour is in the `pclr` box, so the frame is the palette's own
    /// shape and every sample is the entry its index names.
    #[test]
    fn a_palette_is_expanded_into_its_entries() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("alpha-jp2-palette.jp2");
        let info = image_info_at(&path, true).expect("the fixture reads");
        assert_eq!((info.width, info.height), (4, 2));
        assert_eq!(info.format, PixelFormat::Rgb8);
        let decoded = decode(&info).expect("the palette page decodes");
        let Pixels::Interleaved { buffer, .. } = decoded.pixels else {
            panic!("a palette page is handed out as one interleaved buffer");
        };
        // Black, red, green and blue, in the order the index raster names them.
        assert_eq!(
            buffer,
            [
                0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 0, 0, 0
            ]
        );
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
