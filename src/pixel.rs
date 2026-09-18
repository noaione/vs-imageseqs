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

#[derive(Debug)]
pub struct WriteTimings {
    /// Time spent converting the interleaved decoder buffer into the
    /// VapourSynth planes.
    pub deinterleave: Duration,
}

/// Sample type used by one supported image format.
trait Sample: Copy {
    const SIZE: usize;

    fn load(source: &[u8]) -> Self;
    fn store(self, destination: &mut [u8]);
}

impl Sample for u8 {
    const SIZE: usize = 1;

    #[inline(always)]
    fn load(source: &[u8]) -> Self {
        source[0]
    }

    #[inline(always)]
    fn store(self, destination: &mut [u8]) {
        destination[0] = self;
    }
}

macro_rules! native_sample {
    ($type:ty, $size:expr) => {
        impl Sample for $type {
            const SIZE: usize = $size;

            #[inline(always)]
            fn load(source: &[u8]) -> Self {
                let bytes: [u8; $size] = source[..$size]
                    .try_into()
                    .expect("the sample size is fixed");
                Self::from_ne_bytes(bytes)
            }

            #[inline(always)]
            fn store(self, destination: &mut [u8]) {
                destination[..$size].copy_from_slice(&self.to_ne_bytes());
            }
        }
    };
}

native_sample!(u16, 2);
native_sample!(f32, 4);

fn channel_count(color_type: ColorType) -> Result<usize> {
    match color_type {
        ColorType::L8 | ColorType::L16 => Ok(1),
        ColorType::La8 | ColorType::La16 => Ok(2),
        ColorType::Rgb8 | ColorType::Rgb16 | ColorType::Rgb32F => Ok(3),
        ColorType::Rgba8 | ColorType::Rgba16 | ColorType::Rgba32F => Ok(4),
        _ => Err(ImgSeqError::new(format!(
            "unsupported image color type {color_type:?}"
        ))),
    }
}

/// Validated shape of one interleaved decoder buffer.
struct ImageLayout {
    format: PixelFormat,
    width: usize,
    height: usize,
    channels: usize,
    row_bytes: usize,
}

fn image_layout(
    color_type: ColorType,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<ImageLayout> {
    let format = PixelFormat::from_color_type(color_type)
        .ok_or_else(|| ImgSeqError::new(format!("unsupported image color type {color_type:?}")))?;
    let width = usize::try_from(width)
        .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
    let height = usize::try_from(height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    let channels = channel_count(color_type)?;
    let row_bytes = width
        .checked_mul(channels)
        .and_then(|value| value.checked_mul(format.bytes_per_sample()))
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

    Ok(ImageLayout {
        format,
        width,
        height,
        channels,
        row_bytes,
    })
}

/// Extract one interleaved channel of a single row into a packed row.
#[inline(always)]
fn extract_channel<T: Sample, const CHANNELS: usize, const CHANNEL: usize>(
    source: &[u8],
    destination: &mut [u8],
) {
    let source_stride = CHANNELS * T::SIZE;
    let sample_start = CHANNEL * T::SIZE;
    for (group, sample) in source
        .chunks_exact(source_stride)
        .zip(destination.chunks_exact_mut(T::SIZE))
    {
        T::store(
            T::load(&group[sample_start..sample_start + T::SIZE]),
            sample,
        );
    }
}

/// Write one interleaved channel of every row directly into a VapourSynth
/// plane, leaving the row padding untouched.
fn write_channel<T: Sample, const CHANNELS: usize, const CHANNEL: usize>(
    mut destination: *mut u8,
    stride: isize,
    pixels: &[u8],
    layout: &ImageLayout,
    plane_row_bytes: usize,
) -> Result<()> {
    if destination.is_null() {
        return Err(ImgSeqError::new(format!(
            "VapourSynth returned a null pointer for plane {CHANNEL}"
        )));
    }
    if stride < plane_row_bytes as isize {
        return Err(ImgSeqError::new(format!(
            "VapourSynth plane {CHANNEL} stride {stride} is smaller than row size {plane_row_bytes}"
        )));
    }

    for row in 0..layout.height {
        let source_start = row * layout.row_bytes;
        let source = &pixels[source_start..source_start + layout.row_bytes];
        // Safety: `new_video_frame` returns writable planes with at least
        // `stride * height` bytes, so every active row fits in the plane.
        let target = unsafe { slice::from_raw_parts_mut(destination, plane_row_bytes) };
        if CHANNELS == 1 {
            target.copy_from_slice(source);
        } else {
            extract_channel::<T, CHANNELS, CHANNEL>(source, target);
        }
        destination = unsafe { destination.offset(stride) };
    }

    Ok(())
}

fn write_planes<T: Sample, const CHANNELS: usize>(
    frame: &mut VideoFrame,
    layout: &ImageLayout,
    pixels: &[u8],
) -> Result<()> {
    let plane_row_bytes = layout
        .width
        .checked_mul(T::SIZE)
        .ok_or_else(|| ImgSeqError::new("image plane row is too large"))?;
    for plane in 0..layout.format.plane_count() {
        let stride = frame.stride(i32::try_from(plane).expect("plane count fits in i32"));
        let destination = frame.plane_mut(i32::try_from(plane).expect("plane count fits in i32"));
        match plane {
            0 => write_channel::<T, CHANNELS, 0>(
                destination,
                stride,
                pixels,
                layout,
                plane_row_bytes,
            )?,
            1 => write_channel::<T, CHANNELS, 1>(
                destination,
                stride,
                pixels,
                layout,
                plane_row_bytes,
            )?,
            _ => write_channel::<T, CHANNELS, 2>(
                destination,
                stride,
                pixels,
                layout,
                plane_row_bytes,
            )?,
        }
    }

    Ok(())
}

/// Convert one interleaved decoded image directly into the VapourSynth planes.
pub fn write_planar(
    frame: &mut VideoFrame,
    color_type: ColorType,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<WriteTimings> {
    let layout = image_layout(color_type, width, height, pixels)?;
    let started = Instant::now();
    match (layout.format.bytes_per_sample(), layout.channels) {
        (1, 1) => write_planes::<u8, 1>(frame, &layout, pixels)?,
        (1, 2) => write_planes::<u8, 2>(frame, &layout, pixels)?,
        (1, 3) => write_planes::<u8, 3>(frame, &layout, pixels)?,
        (1, 4) => write_planes::<u8, 4>(frame, &layout, pixels)?,
        (2, 1) => write_planes::<u16, 1>(frame, &layout, pixels)?,
        (2, 2) => write_planes::<u16, 2>(frame, &layout, pixels)?,
        (2, 3) => write_planes::<u16, 3>(frame, &layout, pixels)?,
        (2, 4) => write_planes::<u16, 4>(frame, &layout, pixels)?,
        (4, 3) => write_planes::<f32, 3>(frame, &layout, pixels)?,
        (4, 4) => write_planes::<f32, 4>(frame, &layout, pixels)?,
        (bytes_per_sample, channels) => {
            return Err(ImgSeqError::new(format!(
                "unsupported sample size {bytes_per_sample} with {channels} channels"
            )));
        }
    }

    Ok(WriteTimings {
        deinterleave: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::{PixelFormat, extract_channel, image_layout};
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
    fn extracts_rgb_and_ignores_alpha() {
        let pixels = [1, 2, 3, 99, 4, 5, 6, 98];
        let mut red = [0; 2];
        let mut green = [0; 2];
        let mut blue = [0; 2];
        extract_channel::<u8, 4, 0>(&pixels, &mut red);
        extract_channel::<u8, 4, 1>(&pixels, &mut green);
        extract_channel::<u8, 4, 2>(&pixels, &mut blue);
        assert_eq!((red, green, blue), ([1, 4], [2, 5], [3, 6]));
    }

    #[test]
    fn extracts_gray16() {
        let pixels = [1, 2, 3, 4];
        let mut plane = [0; 4];
        extract_channel::<u16, 1, 0>(&pixels, &mut plane);
        assert_eq!(plane, pixels);
    }

    #[test]
    fn validates_buffer_lengths() {
        assert!(image_layout(ColorType::Rgb8, 2, 1, &[0; 5]).is_err());
        assert!(image_layout(ColorType::Rgb8, 2, 1, &[0; 6]).is_ok());
        assert!(image_layout(ColorType::Rgba8, 2, 1, &[0; 8]).is_ok());
    }
}
