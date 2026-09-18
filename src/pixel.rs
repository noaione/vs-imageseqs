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
    Gray32F,
    Rgb8,
    Rgb16,
    Rgb32F,
    /// Planar 4:2:0 8 bit, the format lossy webp is decoded into.
    Yuv420P8,
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

    /// Whether one decoded frame of this format is laid out as one buffer per
    /// plane instead of one interleaved buffer.
    pub const fn decodes_to_planes(self) -> bool {
        matches!(self, Self::Yuv420P8)
    }

    /// Gray format that carries the alpha channel of this format.
    pub const fn alpha_format(self) -> Self {
        match self {
            Self::Gray8 | Self::Rgb8 | Self::Yuv420P8 => Self::Gray8,
            Self::Gray16 | Self::Rgb16 => Self::Gray16,
            Self::Gray32F | Self::Rgb32F => Self::Gray32F,
        }
    }

    pub const fn color_family(self) -> ColorFamily {
        match self {
            Self::Gray8 | Self::Gray16 | Self::Gray32F => ColorFamily::Gray,
            Self::Rgb8 | Self::Rgb16 | Self::Rgb32F => ColorFamily::RGB,
            Self::Yuv420P8 => ColorFamily::YUV,
        }
    }

    pub const fn sample_type(self) -> SampleType {
        match self {
            Self::Gray8 | Self::Gray16 | Self::Rgb8 | Self::Rgb16 | Self::Yuv420P8 => {
                SampleType::Integer
            }
            Self::Gray32F | Self::Rgb32F => SampleType::Float,
        }
    }

    pub const fn bits_per_sample(self) -> i32 {
        match self {
            Self::Gray8 | Self::Rgb8 | Self::Yuv420P8 => 8,
            Self::Gray16 | Self::Rgb16 => 16,
            Self::Gray32F | Self::Rgb32F => 32,
        }
    }

    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::Gray8 | Self::Rgb8 | Self::Yuv420P8 => 1,
            Self::Gray16 | Self::Rgb16 => 2,
            Self::Gray32F | Self::Rgb32F => 4,
        }
    }

    pub const fn plane_count(self) -> usize {
        match self {
            Self::Gray8 | Self::Gray16 | Self::Gray32F => 1,
            Self::Rgb8 | Self::Rgb16 | Self::Rgb32F | Self::Yuv420P8 => 3,
        }
    }

    /// Chroma subsampling of this format, as VapourSynth reports it.
    pub const fn sub_sampling(self) -> (i32, i32) {
        match self {
            Self::Yuv420P8 => (1, 1),
            _ => (0, 0),
        }
    }

    /// Width and height of one plane of a `width` x `height` frame as a
    /// decoder lays it out, which rounds a half size plane up.
    ///
    /// VapourSynth rounds the other way, so on an odd size the two disagree by
    /// one row or column; [`Self::frame_plane_dimensions`] is the size a frame
    /// actually holds.
    pub fn plane_dimensions(self, plane: usize, width: usize, height: usize) -> (usize, usize) {
        match self {
            Self::Yuv420P8 if plane > 0 => (width.div_ceil(2), height.div_ceil(2)),
            _ => (width, height),
        }
    }

    /// Width and height of one plane of a `width` x `height` VapourSynth frame.
    ///
    /// A subsampled plane is half the size of the frame, truncated, whatever
    /// the decoder produced, because that is what the frame holds.
    pub fn frame_plane_dimensions(
        self,
        plane: usize,
        width: usize,
        height: usize,
    ) -> (usize, usize) {
        match self {
            Self::Yuv420P8 if plane > 0 => (width / 2, height / 2),
            _ => (width, height),
        }
    }

    /// Bytes one plane of a `width` x `height` frame occupies when it is
    /// tightly packed, which is how the decoder buffers are laid out.
    pub fn plane_bytes(self, plane: usize, width: usize, height: usize) -> usize {
        if plane >= self.plane_count() {
            return 0;
        }
        let (width, height) = self.plane_dimensions(plane, width, height);
        width
            .saturating_mul(height)
            .saturating_mul(self.bytes_per_sample())
    }

    /// Bytes every plane of a `width` x `height` frame occupies together.
    pub fn planes_bytes(self, width: usize, height: usize) -> usize {
        (0..self.plane_count())
            .map(|plane| self.plane_bytes(plane, width, height))
            .fold(0, usize::saturating_add)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Gray8 => "Gray8",
            Self::Gray16 => "Gray16",
            Self::Gray32F => "GrayS",
            Self::Rgb8 => "RGB24",
            Self::Rgb16 => "RGB48",
            Self::Rgb32F => "RGBS",
            Self::Yuv420P8 => "YUV420P8",
        }
    }
}

/// Interleaved channel that carries alpha when the color type has one.
pub const fn alpha_channel(color_type: ColorType) -> Option<usize> {
    match color_type {
        ColorType::La8 | ColorType::La16 => Some(1),
        ColorType::Rgba8 | ColorType::Rgba16 | ColorType::Rgba32F => Some(3),
        _ => None,
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

pub(crate) fn channel_count(color_type: ColorType) -> Result<usize> {
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
    // The destination row holds exactly one sample per source group, so
    // `chunks_mut` never yields a partial sample.
    for (group, sample) in source
        .chunks_exact(source_stride)
        .zip(destination.chunks_mut(T::SIZE))
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

/// Copy one tightly packed decoder buffer per plane into the VapourSynth
/// planes.
///
/// Unlike [`write_planar`] the buffers already have the plane layout, so this
/// only moves whole rows around the frame padding, which is what makes the
/// planar formats cheaper to write: there is no per sample work at all.
pub fn write_decoded_planes(
    frame: &mut VideoFrame,
    format: PixelFormat,
    width: u32,
    height: u32,
    planes: &[Vec<u8>],
) -> Result<WriteTimings> {
    if planes.len() != format.plane_count() {
        return Err(ImgSeqError::new(format!(
            "decoder returned {} planes, {} has {}",
            planes.len(),
            format.name(),
            format.plane_count(),
        )));
    }
    let width = usize::try_from(width)
        .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
    let height = usize::try_from(height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    let sample_bytes = format.bytes_per_sample();
    let started = Instant::now();

    for (index, buffer) in planes.iter().enumerate() {
        let (decoded_width, decoded_height) = format.plane_dimensions(index, width, height);
        let decoded_row = decoded_width
            .checked_mul(sample_bytes)
            .ok_or_else(|| ImgSeqError::new("image plane row is too large"))?;
        let expected = decoded_row
            .checked_mul(decoded_height)
            .ok_or_else(|| ImgSeqError::new("image plane is too large"))?;
        if buffer.len() != expected {
            return Err(ImgSeqError::new(format!(
                "decoder returned {actual} bytes for plane {index}, expected {expected}",
                actual = buffer.len(),
            )));
        }

        // The frame can be a row or a column smaller than the decoder's plane
        // on an odd size, and the extra samples have nowhere to go.
        let (frame_width, frame_height) = format.frame_plane_dimensions(index, width, height);
        let row_bytes = frame_width
            .checked_mul(sample_bytes)
            .ok_or_else(|| ImgSeqError::new("image plane row is too large"))?;
        if decoded_height < frame_height || decoded_row < row_bytes {
            return Err(ImgSeqError::new(format!(
                "decoder returned a {decoded_width}x{decoded_height} plane {index}, which is smaller than the {frame_width}x{frame_height} the frame holds"
            )));
        }

        let plane = i32::try_from(index).expect("the plane count fits in i32");
        let stride = frame.stride(plane);
        let destination = frame.plane_mut(plane);
        if destination.is_null() {
            return Err(ImgSeqError::new(format!(
                "VapourSynth returned a null pointer for plane {index}"
            )));
        }
        if stride < row_bytes as isize {
            return Err(ImgSeqError::new(format!(
                "VapourSynth plane {index} stride {stride} is smaller than row size {row_bytes}"
            )));
        }

        for row in 0..frame_height {
            // Every row of the decoder buffer is `decoded_row` bytes wide, and
            // the frame keeps the first `row_bytes` of it.
            let source = &buffer[row * decoded_row..row * decoded_row + row_bytes];
            // Safety: the plane holds at least `stride * frame_height` bytes,
            // so every active row fits in it.
            let target = unsafe {
                slice::from_raw_parts_mut(destination.offset(stride * row as isize), row_bytes)
            };
            target.copy_from_slice(source);
        }
    }

    Ok(WriteTimings {
        deinterleave: started.elapsed(),
    })
}

/// Fill one gray plane of `height` rows of `row_bytes` bytes with `value`.
fn fill_plane<T: Sample>(
    mut destination: *mut u8,
    stride: isize,
    row_bytes: usize,
    height: usize,
    value: T,
) {
    for _ in 0..height {
        // Safety: the plane holds at least `stride * height` bytes, so every
        // active row fits in it, and `row_bytes` is a whole number of samples.
        let target = unsafe { slice::from_raw_parts_mut(destination, row_bytes) };
        for sample in target.chunks_mut(T::SIZE) {
            value.store(sample);
        }
        destination = unsafe { destination.offset(stride) };
    }
}

/// Fill the single plane of a gray frame with the opaque value.
///
/// Sources that decode to planes have no alpha channel to write, so their
/// alpha clip is opaque everywhere.
pub fn write_opaque_alpha(
    frame: &mut VideoFrame,
    format: PixelFormat,
    width: u32,
    height: u32,
) -> Result<WriteTimings> {
    let width = usize::try_from(width)
        .map_err(|_| ImgSeqError::new("image width does not fit in memory"))?;
    let height = usize::try_from(height)
        .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
    let row_bytes = width
        .checked_mul(format.bytes_per_sample())
        .ok_or_else(|| ImgSeqError::new("image plane row is too large"))?;
    let stride = frame.stride(0);
    let destination = frame.plane_mut(0);
    if destination.is_null() {
        return Err(ImgSeqError::new(
            "VapourSynth returned a null pointer for the alpha plane",
        ));
    }
    if stride < row_bytes as isize {
        return Err(ImgSeqError::new(format!(
            "VapourSynth alpha plane stride {stride} is smaller than row size {row_bytes}"
        )));
    }
    let started = Instant::now();
    match format.bytes_per_sample() {
        1 => fill_plane::<u8>(destination, stride, row_bytes, height, u8::MAX),
        2 => fill_plane::<u16>(destination, stride, row_bytes, height, u16::MAX),
        4 => fill_plane::<f32>(destination, stride, row_bytes, height, 1.0),
        bytes => {
            return Err(ImgSeqError::new(format!(
                "unsupported alpha sample size {bytes}"
            )));
        }
    }

    Ok(WriteTimings {
        deinterleave: started.elapsed(),
    })
}

/// Write the alpha channel of one decoded image into a gray VapourSynth frame.
///
/// Sources without an alpha channel produce an opaque plane, so an alpha clip
/// always has a meaningful value for every frame.
pub fn write_alpha(
    frame: &mut VideoFrame,
    color_type: ColorType,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<WriteTimings> {
    let layout = image_layout(color_type, width, height, pixels)?;
    let started = Instant::now();
    match (layout.format.bytes_per_sample(), alpha_channel(color_type)) {
        (1, Some(1)) => write_alpha_plane::<u8, 2, 1>(frame, &layout, pixels)?,
        (2, Some(1)) => write_alpha_plane::<u16, 2, 1>(frame, &layout, pixels)?,
        (1, Some(3)) => write_alpha_plane::<u8, 4, 3>(frame, &layout, pixels)?,
        (2, Some(3)) => write_alpha_plane::<u16, 4, 3>(frame, &layout, pixels)?,
        (4, Some(3)) => write_alpha_plane::<f32, 4, 3>(frame, &layout, pixels)?,
        (1, None) => fill_alpha_plane::<u8>(frame, &layout, u8::MAX)?,
        (2, None) => fill_alpha_plane::<u16>(frame, &layout, u16::MAX)?,
        (4, None) => fill_alpha_plane::<f32>(frame, &layout, 1.0)?,
        (bytes_per_sample, channel) => {
            return Err(ImgSeqError::new(format!(
                "unsupported alpha source for {bytes_per_sample}-byte samples with channel {channel:?}"
            )));
        }
    }

    Ok(WriteTimings {
        deinterleave: started.elapsed(),
    })
}

fn alpha_plane_row_bytes<T: Sample>(layout: &ImageLayout) -> Result<usize> {
    layout
        .width
        .checked_mul(T::SIZE)
        .ok_or_else(|| ImgSeqError::new("image plane row is too large"))
}

/// Write the alpha channel into the single plane of a gray frame.
fn write_alpha_plane<T: Sample, const CHANNELS: usize, const CHANNEL: usize>(
    frame: &mut VideoFrame,
    layout: &ImageLayout,
    pixels: &[u8],
) -> Result<()> {
    let plane_row_bytes = alpha_plane_row_bytes::<T>(layout)?;
    write_channel::<T, CHANNELS, CHANNEL>(
        frame.plane_mut(0),
        frame.stride(0),
        pixels,
        layout,
        plane_row_bytes,
    )
}

/// Fill the single plane of a gray frame with one value.
fn fill_alpha_plane<T: Sample>(
    frame: &mut VideoFrame,
    layout: &ImageLayout,
    value: T,
) -> Result<()> {
    let plane_row_bytes = alpha_plane_row_bytes::<T>(layout)?;
    let stride = frame.stride(0);
    let mut destination = frame.plane_mut(0);
    if destination.is_null() {
        return Err(ImgSeqError::new(
            "VapourSynth returned a null pointer for the alpha plane",
        ));
    }
    if stride < plane_row_bytes as isize {
        return Err(ImgSeqError::new(format!(
            "VapourSynth alpha plane stride {stride} is smaller than row size {plane_row_bytes}"
        )));
    }

    for _ in 0..layout.height {
        // Safety: `new_video_frame` returns writable planes with at least
        // `stride * height` bytes, so every active row fits in the plane.
        let target = unsafe { slice::from_raw_parts_mut(destination, plane_row_bytes) };
        // `plane_row_bytes` is a whole number of samples, so no partial chunk
        // is ever handed to `Sample::store`.
        for sample in target.chunks_mut(T::SIZE) {
            value.store(sample);
        }
        destination = unsafe { destination.offset(stride) };
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PixelFormat, alpha_channel, extract_channel, image_layout};
    use image::ColorType;
    use vapoursynth4_rs::{ColorFamily, SampleType};

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

    #[test]
    fn maps_alpha_formats() {
        assert_eq!(PixelFormat::Gray8.alpha_format(), PixelFormat::Gray8);
        assert_eq!(PixelFormat::Gray16.alpha_format(), PixelFormat::Gray16);
        assert_eq!(PixelFormat::Rgb8.alpha_format(), PixelFormat::Gray8);
        assert_eq!(PixelFormat::Rgb16.alpha_format(), PixelFormat::Gray16);
        assert_eq!(PixelFormat::Rgb32F.alpha_format(), PixelFormat::Gray32F);
        assert_eq!(PixelFormat::Gray32F.color_family(), ColorFamily::Gray);
        assert_eq!(PixelFormat::Gray32F.sample_type(), SampleType::Float);
        assert_eq!(PixelFormat::Gray32F.bits_per_sample(), 32);
        assert_eq!(PixelFormat::Gray32F.plane_count(), 1);
        assert_eq!(PixelFormat::Gray32F.name(), "GrayS");
    }

    #[test]
    fn locates_alpha_channels() {
        assert_eq!(alpha_channel(ColorType::L8), None);
        assert_eq!(alpha_channel(ColorType::L16), None);
        assert_eq!(alpha_channel(ColorType::La8), Some(1));
        assert_eq!(alpha_channel(ColorType::La16), Some(1));
        assert_eq!(alpha_channel(ColorType::Rgb8), None);
        assert_eq!(alpha_channel(ColorType::Rgb16), None);
        assert_eq!(alpha_channel(ColorType::Rgb32F), None);
        assert_eq!(alpha_channel(ColorType::Rgba8), Some(3));
        assert_eq!(alpha_channel(ColorType::Rgba16), Some(3));
        assert_eq!(alpha_channel(ColorType::Rgba32F), Some(3));
    }

    #[test]
    fn extracts_alpha_channels() {
        let gray_alpha = [10, 99, 20, 98];
        let mut alpha = [0; 2];
        extract_channel::<u8, 2, 1>(&gray_alpha, &mut alpha);
        assert_eq!(alpha, [99, 98]);

        let rgba = [1, 2, 3, 4, 5, 6, 7, 8];
        extract_channel::<u8, 4, 3>(&rgba, &mut alpha);
        assert_eq!(alpha, [4, 8]);

        let rgba16 = [
            0, 1, 0, 2, 0, 3, 0, 4, //
            0, 5, 0, 6, 0, 7, 0, 8,
        ];
        let mut alpha = [0; 4];
        extract_channel::<u16, 4, 3>(&rgba16, &mut alpha);
        assert_eq!(alpha, [0, 4, 0, 8]);

        let mut rgba32f = Vec::new();
        for value in [1.0_f32, 2.0, 3.0, 0.25, 4.0, 5.0, 6.0, 0.75] {
            rgba32f.extend_from_slice(&value.to_ne_bytes());
        }
        let mut alpha = [0; 8];
        extract_channel::<f32, 4, 3>(&rgba32f, &mut alpha);
        assert_eq!(
            f32::from_ne_bytes(alpha[0..4].try_into().unwrap()),
            0.25_f32
        );
        assert_eq!(
            f32::from_ne_bytes(alpha[4..8].try_into().unwrap()),
            0.75_f32
        );
    }
}
