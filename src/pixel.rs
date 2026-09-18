use std::{
    slice,
    time::{Duration, Instant},
};

use image::ColorType;
use vapoursynth4_rs::frame::VideoFrame;
use vapoursynth4_rs::{ColorFamily, SampleType};

use crate::error::{ImgSeqError, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    Gray8,
    Gray16,
    Rgb8,
    Rgb16,
    Rgb32F,
}

#[derive(Debug)]
pub struct WriteTimings {
    pub planarize: Duration,
    pub copy: Duration,
}

impl PixelFormat {
    pub fn from_color_type(color_type: ColorType) -> Option<Self> {
        match color_type {
            ColorType::L8 | ColorType::La8 => Some(Self::Gray8),
            ColorType::L16 | ColorType::La16 => Some(Self::Gray16),
            ColorType::Rgb8 | ColorType::Rgba8 => Some(Self::Rgb8),
            ColorType::Rgb16 | ColorType::Rgba16 => Some(Self::Rgb16),
            ColorType::Rgb32F | ColorType::Rgba32F => Some(Self::Rgb32F),
            _ => None,
        }
    }

    pub const fn color_family(self) -> ColorFamily {
        match self {
            Self::Gray8 | Self::Gray16 => ColorFamily::Gray,
            Self::Rgb8 | Self::Rgb16 | Self::Rgb32F => ColorFamily::RGB,
        }
    }

    pub const fn sample_type(self) -> SampleType {
        match self {
            Self::Gray8 | Self::Gray16 | Self::Rgb8 | Self::Rgb16 => SampleType::Integer,
            Self::Rgb32F => SampleType::Float,
        }
    }

    pub const fn bits_per_sample(self) -> i32 {
        match self {
            Self::Gray8 | Self::Rgb8 => 8,
            Self::Gray16 | Self::Rgb16 => 16,
            Self::Rgb32F => 32,
        }
    }

    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::Gray8 | Self::Rgb8 => 1,
            Self::Gray16 | Self::Rgb16 => 2,
            Self::Rgb32F => 4,
        }
    }

    pub const fn plane_count(self) -> usize {
        match self {
            Self::Gray8 | Self::Gray16 => 1,
            Self::Rgb8 | Self::Rgb16 | Self::Rgb32F => 3,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Gray8 => "Gray8",
            Self::Gray16 => "Gray16",
            Self::Rgb8 => "RGB24",
            Self::Rgb16 => "RGB48",
            Self::Rgb32F => "RGBS",
        }
    }
}

/// Split interleaved image data into tightly packed VapourSynth planes.
pub fn planarize(
    color_type: ColorType,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<Vec<Vec<u8>>> {
    let format = PixelFormat::from_color_type(color_type)
        .ok_or_else(|| ImgSeqError::new(format!("unsupported image color type {color_type:?}")))?;

    let width = usize::try_from(width)
        .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
    let height = usize::try_from(height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    let bytes_per_sample = format.bytes_per_sample();
    let channels = match color_type {
        ColorType::L8 | ColorType::L16 => 1,
        ColorType::La8 | ColorType::La16 => 2,
        ColorType::Rgb8 | ColorType::Rgb16 | ColorType::Rgb32F => 3,
        ColorType::Rgba8 | ColorType::Rgba16 | ColorType::Rgba32F => 4,
        _ => unreachable!("unsupported color type was handled above"),
    };
    let row_bytes = width
        .checked_mul(channels)
        .and_then(|value| value.checked_mul(bytes_per_sample))
        .ok_or_else(|| ImgSeqError::new("image row is too large"))?;
    let expected_len = row_bytes
        .checked_mul(height)
        .ok_or_else(|| ImgSeqError::new("image is too large"))?;
    if pixels.len() != expected_len {
        return Err(ImgSeqError::new(format!(
            "decoder returned {actual} bytes, expected {expected}",
            actual = pixels.len(),
            expected = expected_len,
        )));
    }

    let plane_len = width
        .checked_mul(height)
        .and_then(|value| value.checked_mul(bytes_per_sample))
        .ok_or_else(|| ImgSeqError::new("image plane is too large"))?;
    let mut planes = (0..format.plane_count())
        .map(|_| Vec::with_capacity(plane_len))
        .collect::<Vec<_>>();

    for row in pixels.chunks_exact(row_bytes) {
        for x in 0..width {
            let pixel = &row[x * channels * bytes_per_sample..];
            if format.plane_count() == 1 {
                planes[0].extend_from_slice(&pixel[..bytes_per_sample]);
            } else {
                for (plane, output) in planes.iter_mut().enumerate().take(3) {
                    let start = plane * bytes_per_sample;
                    output.extend_from_slice(&pixel[start..start + bytes_per_sample]);
                }
            }
        }
    }

    Ok(planes)
}

pub fn write_planar(
    frame: &mut VideoFrame,
    color_type: ColorType,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<WriteTimings> {
    let format = PixelFormat::from_color_type(color_type)
        .ok_or_else(|| ImgSeqError::new(format!("unsupported image color type {color_type:?}")))?;
    let planarize_started = Instant::now();
    let planes = planarize(color_type, width, height, pixels)?;
    let planarize = planarize_started.elapsed();
    let width = usize::try_from(width)
        .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
    let height = usize::try_from(height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    let row_bytes = width
        .checked_mul(format.bytes_per_sample())
        .ok_or_else(|| ImgSeqError::new("image row is too large"))?;

    let copy_started = Instant::now();
    for (plane, data) in planes.iter().enumerate() {
        let plane = i32::try_from(plane).expect("plane count fits in i32");
        let stride = frame.stride(plane);
        if stride < row_bytes as isize {
            return Err(ImgSeqError::new(format!(
                "VapourSynth plane {plane} stride {stride} is smaller than row size {row_bytes}"
            )));
        }
        let destination = frame.plane_mut(plane);
        if destination.is_null() {
            return Err(ImgSeqError::new(format!(
                "VapourSynth returned a null pointer for plane {plane}"
            )));
        }

        // `new_video_frame` returns writable planes with at least `stride * height`
        // bytes. Only the active row is copied; padding is left untouched.
        for row in 0..height {
            let source_start = row * row_bytes;
            let source = &data[source_start..source_start + row_bytes];
            let destination = unsafe {
                slice::from_raw_parts_mut(destination.offset(row as isize * stride), row_bytes)
            };
            destination.copy_from_slice(source);
        }
    }

    Ok(WriteTimings {
        planarize,
        copy: copy_started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::{PixelFormat, planarize};
    use image::ColorType;

    #[test]
    fn maps_supported_color_types() {
        assert_eq!(
            PixelFormat::from_color_type(ColorType::L8),
            Some(PixelFormat::Gray8)
        );
        assert_eq!(
            PixelFormat::from_color_type(ColorType::La16),
            Some(PixelFormat::Gray16)
        );
        assert_eq!(
            PixelFormat::from_color_type(ColorType::Rgba8),
            Some(PixelFormat::Rgb8)
        );
        assert_eq!(
            PixelFormat::from_color_type(ColorType::Rgb32F),
            Some(PixelFormat::Rgb32F)
        );
    }

    #[test]
    fn deinterleaves_rgb_and_ignores_alpha() {
        let pixels = [1, 2, 3, 99, 4, 5, 6, 98];
        let planes = planarize(ColorType::Rgba8, 2, 1, &pixels).unwrap();
        assert_eq!(planes, vec![vec![1, 4], vec![2, 5], vec![3, 6]]);
    }

    #[test]
    fn deinterleaves_gray16() {
        let pixels = [1, 2, 3, 4];
        let planes = planarize(ColorType::L16, 2, 1, &pixels).unwrap();
        assert_eq!(planes, vec![pixels.to_vec()]);
    }
}
