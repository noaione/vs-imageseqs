use std::{
    ops::Range,
    ptr::copy_nonoverlapping,
    slice,
    time::{Duration, Instant},
};

use image::{ColorType, metadata::Orientation};
use vapoursynth4_rs::frame::VideoFrame;
use vapoursynth4_rs::{ColorFamily, SampleType};

use crate::error::{ImgSeqError, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    Gray8,
    /// Planar 4:2:0, the format lossy webp is decoded into.
    Yuv420P8,
    /// Planar 4:2:0 at ten bits, which is what a ten bit heif or avif holds.
    Yuv420P10,
    /// Planar 4:2:2.
    Yuv422P8,
    Yuv422P10,
    /// Planar 4:4:4.
    Yuv444P8,
    Yuv444P10,
    Yuv444P12,
    Yuv444P16,
    /// Gray or r,g,b at the depth a container states, for the four depths
    /// between the eight bits and the sixteen bits a decoder's colour type
    /// knows: `Gray11` and `Rgb13` are the `Gray11` and `RGB39` a file that
    /// states them is handed out as, rather than the sixteen bit word the
    /// decoder handed over. See [`PixelFormat::at_depth`].
    Gray9,
    Gray10,
    Gray11,
    Gray12,
    Gray13,
    Gray14,
    Gray15,
    Gray16,
    Gray32F,
    Rgb8,
    Rgb9,
    Rgb10,
    Rgb11,
    Rgb12,
    Rgb13,
    Rgb14,
    Rgb15,
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

    /// Format a decoder's color type is handed out as when a container states
    /// `bits` per sample.
    ///
    /// The colour type knows eight bits, sixteen bits and float and nothing in
    /// between, so the format built from it is the word the samples are stored
    /// in rather than the depth the file states, and a ten bit file arrives
    /// left aligned in `Gray16` or `Rgb16`. A container that states a depth
    /// narrows that word here, and the writer moves the samples down into the
    /// depth the narrower format names.
    ///
    /// VapourSynth names every depth from eight to sixteen in both families, so
    /// a file that states one is always handed out as the format that says so. A
    /// depth outside that range leaves the word alone rather than rounding the
    /// samples into a format that would misstate them, and so does every other
    /// format: an eight bit word is the whole depth of the two eight bit
    /// formats, and a planar or float one is already the depth it holds, or one
    /// a container's depth says nothing about.
    #[must_use]
    pub const fn at_depth(self, bits: u32) -> Self {
        match self {
            Self::Gray16 => match gray_format(bits) {
                Some(format) => format,
                None => self,
            },
            Self::Rgb16 => match rgb_format(bits) {
                Some(format) => format,
                None => self,
            },
            other => other,
        }
    }

    /// Gray format that carries the alpha channel of this format.
    ///
    /// The alpha clip of a source is the same depth as its colour, so a ten bit
    /// file's alpha plane is a ten bit frame and is opaque at 1023 rather than
    /// at 65535.
    pub const fn alpha_format(self) -> Self {
        match self {
            Self::Gray8 | Self::Rgb8 | Self::Yuv420P8 | Self::Yuv422P8 | Self::Yuv444P8 => {
                Self::Gray8
            }
            Self::Gray9 | Self::Rgb9 => Self::Gray9,
            Self::Gray10 | Self::Rgb10 | Self::Yuv420P10 | Self::Yuv422P10 | Self::Yuv444P10 => {
                Self::Gray10
            }
            Self::Gray11 | Self::Rgb11 => Self::Gray11,
            Self::Gray12 | Self::Rgb12 | Self::Yuv444P12 => Self::Gray12,
            Self::Gray13 | Self::Rgb13 => Self::Gray13,
            Self::Gray14 | Self::Rgb14 => Self::Gray14,
            Self::Gray15 | Self::Rgb15 => Self::Gray15,
            Self::Gray16 | Self::Rgb16 | Self::Yuv444P16 => Self::Gray16,
            Self::Gray32F | Self::Rgb32F => Self::Gray32F,
        }
    }

    pub const fn color_family(self) -> ColorFamily {
        match self {
            Self::Gray8
            | Self::Gray9
            | Self::Gray10
            | Self::Gray11
            | Self::Gray12
            | Self::Gray13
            | Self::Gray14
            | Self::Gray15
            | Self::Gray16
            | Self::Gray32F => ColorFamily::Gray,
            Self::Rgb8
            | Self::Rgb9
            | Self::Rgb10
            | Self::Rgb11
            | Self::Rgb12
            | Self::Rgb13
            | Self::Rgb14
            | Self::Rgb15
            | Self::Rgb16
            | Self::Rgb32F => ColorFamily::RGB,
            Self::Yuv420P8
            | Self::Yuv420P10
            | Self::Yuv422P8
            | Self::Yuv422P10
            | Self::Yuv444P8
            | Self::Yuv444P10
            | Self::Yuv444P12
            | Self::Yuv444P16 => ColorFamily::YUV,
        }
    }

    pub const fn sample_type(self) -> SampleType {
        match self {
            Self::Gray32F | Self::Rgb32F => SampleType::Float,
            _ => SampleType::Integer,
        }
    }

    pub const fn bits_per_sample(self) -> i32 {
        match self {
            Self::Gray8 | Self::Rgb8 | Self::Yuv420P8 | Self::Yuv422P8 | Self::Yuv444P8 => 8,
            Self::Gray9 | Self::Rgb9 => 9,
            Self::Gray10 | Self::Rgb10 | Self::Yuv420P10 | Self::Yuv422P10 | Self::Yuv444P10 => 10,
            Self::Gray11 | Self::Rgb11 => 11,
            Self::Gray12 | Self::Rgb12 | Self::Yuv444P12 => 12,
            Self::Gray13 | Self::Rgb13 => 13,
            Self::Gray14 | Self::Rgb14 => 14,
            Self::Gray15 | Self::Rgb15 => 15,
            Self::Gray16 | Self::Rgb16 | Self::Yuv444P16 => 16,
            Self::Gray32F | Self::Rgb32F => 32,
        }
    }

    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::Gray8 | Self::Rgb8 | Self::Yuv420P8 | Self::Yuv422P8 | Self::Yuv444P8 => 1,
            Self::Gray32F | Self::Rgb32F => 4,
            // Every depth from nine to sixteen bits, and every depth a planar
            // yuv format names, is stored in a 16 bit word.
            _ => 2,
        }
    }

    /// Largest sample an integer format holds, which is what an opaque alpha
    /// plane is filled with.
    ///
    /// A ten or twelve bit format is not stored wider than its depth allows, so
    /// the two byte integer formats do not share one maximum. Float samples have
    /// no largest value, and nothing asks one for them.
    pub const fn integer_max(self) -> u16 {
        let bits = self.bits_per_sample();
        if bits >= 16 {
            u16::MAX
        } else {
            ((1u32 << bits) - 1) as u16
        }
    }

    pub const fn plane_count(self) -> usize {
        match self {
            Self::Gray8
            | Self::Gray9
            | Self::Gray10
            | Self::Gray11
            | Self::Gray12
            | Self::Gray13
            | Self::Gray14
            | Self::Gray15
            | Self::Gray16
            | Self::Gray32F => 1,
            _ => 3,
        }
    }

    /// Chroma subsampling of this format, as VapourSynth reports it.
    pub const fn sub_sampling(self) -> (i32, i32) {
        match self {
            Self::Yuv420P8 | Self::Yuv420P10 => (1, 1),
            Self::Yuv422P8 | Self::Yuv422P10 => (1, 0),
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
        match (self.sub_sampling(), plane) {
            (_, 0) => (width, height),
            ((1, 1), _) => (width.div_ceil(2), height.div_ceil(2)),
            ((1, 0), _) => (width.div_ceil(2), height),
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
        match (self.sub_sampling(), plane) {
            (_, 0) => (width, height),
            ((1, 1), _) => (width / 2, height / 2),
            ((1, 0), _) => (width / 2, height),
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
            Self::Gray9 => "Gray9",
            Self::Gray10 => "Gray10",
            Self::Gray11 => "Gray11",
            Self::Gray12 => "Gray12",
            Self::Gray13 => "Gray13",
            Self::Gray14 => "Gray14",
            Self::Gray15 => "Gray15",
            Self::Gray16 => "Gray16",
            Self::Gray32F => "GrayS",
            Self::Rgb8 => "RGB24",
            Self::Rgb9 => "RGB27",
            Self::Rgb10 => "RGB30",
            Self::Rgb11 => "RGB33",
            Self::Rgb12 => "RGB36",
            Self::Rgb13 => "RGB39",
            Self::Rgb14 => "RGB42",
            Self::Rgb15 => "RGB45",
            Self::Rgb16 => "RGB48",
            Self::Rgb32F => "RGBS",
            Self::Yuv420P8 => "YUV420P8",
            Self::Yuv420P10 => "YUV420P10",
            Self::Yuv422P8 => "YUV422P8",
            Self::Yuv422P10 => "YUV422P10",
            Self::Yuv444P8 => "YUV444P8",
            Self::Yuv444P10 => "YUV444P10",
            Self::Yuv444P12 => "YUV444P12",
            Self::Yuv444P16 => "YUV444P16",
        }
    }
}

/// The gray format that names `bits` per sample, which VapourSynth has for every
/// depth from eight to sixteen.
///
/// A depth outside that range answers `None`, which leaves a caller the format
/// it started from: there is no gray format a file of such a depth is handed out
/// as, and the word it is stored in is what is left.
const fn gray_format(bits: u32) -> Option<PixelFormat> {
    match bits {
        8 => Some(PixelFormat::Gray8),
        9 => Some(PixelFormat::Gray9),
        10 => Some(PixelFormat::Gray10),
        11 => Some(PixelFormat::Gray11),
        12 => Some(PixelFormat::Gray12),
        13 => Some(PixelFormat::Gray13),
        14 => Some(PixelFormat::Gray14),
        15 => Some(PixelFormat::Gray15),
        16 => Some(PixelFormat::Gray16),
        _ => None,
    }
}

/// The r,g,b format that names `bits` per sample, which VapourSynth has for
/// every depth from eight to sixteen; see [`gray_format`].
const fn rgb_format(bits: u32) -> Option<PixelFormat> {
    match bits {
        8 => Some(PixelFormat::Rgb8),
        9 => Some(PixelFormat::Rgb9),
        10 => Some(PixelFormat::Rgb10),
        11 => Some(PixelFormat::Rgb11),
        12 => Some(PixelFormat::Rgb12),
        13 => Some(PixelFormat::Rgb13),
        14 => Some(PixelFormat::Rgb14),
        15 => Some(PixelFormat::Rgb15),
        16 => Some(PixelFormat::Rgb16),
        _ => None,
    }
}

/// How the samples of a decoded image are rearranged as they are written.
///
/// The eight exif orientations are the identity, a mirror in one direction or
/// both, a transpose, and the two mirrors composed with it, so three flags hold
/// all of them. The mirrors are in the frame's own coordinates, which is why a
/// transpose swaps the size the frame is built with: [`Self::output_size`] is
/// what a clip is sized from, and [`Self::source_of`] is what the writer reads
/// for each destination sample.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Transform {
    transpose: bool,
    flip_x: bool,
    flip_y: bool,
}

impl Transform {
    /// Transform that rearranges nothing, which is what a file without an exif
    /// orientation, and every file when the caller turns rotation off, gets.
    pub const IDENTITY: Self = Self {
        transpose: false,
        flip_x: false,
        flip_y: false,
    };

    /// Transform that hands out `orientation` the way a viewer shows it.
    ///
    /// [`Orientation`](image::metadata::Orientation) names the same eight
    /// transformations the exif tag does, so this is a rename and not a
    /// conversion, down to the order of the composed ones: `Rotate90FlipH`
    /// rotates the stored picture 90 degrees clockwise and then mirrors it,
    /// which comes out as the plain transpose, while `Rotate90` on its own does
    /// not.
    #[must_use]
    pub const fn from_orientation(orientation: Orientation) -> Self {
        match orientation {
            Orientation::NoTransforms => Self::IDENTITY,
            Orientation::FlipHorizontal => Self {
                flip_x: true,
                ..Self::IDENTITY
            },
            Orientation::Rotate180 => Self {
                flip_x: true,
                flip_y: true,
                ..Self::IDENTITY
            },
            Orientation::FlipVertical => Self {
                flip_y: true,
                ..Self::IDENTITY
            },
            // A quarter turn clockwise reads the source column `y` for
            // destination row `y`, and its row `output_width - 1 - x`.
            Orientation::Rotate90 => Self {
                transpose: true,
                flip_x: true,
                ..Self::IDENTITY
            },
            // The quarter turn followed by a horizontal mirror, which is the
            // transpose of the stored picture and not a rotation at all.
            Orientation::Rotate90FlipH => Self {
                transpose: true,
                ..Self::IDENTITY
            },
            // The anti-diagonal twin, which is a quarter turn the other way
            // followed by the same mirror.
            Orientation::Rotate270FlipH => Self {
                transpose: true,
                flip_x: true,
                flip_y: true,
            },
            // A quarter turn counter-clockwise, which is the anti-diagonal
            // transpose with its row mirrored.
            Orientation::Rotate270 => Self {
                transpose: true,
                flip_y: true,
                ..Self::IDENTITY
            },
        }
    }

    /// Whether it swaps width and height.
    #[must_use]
    pub const fn transposes(self) -> bool {
        self.transpose
    }

    /// Size this transform hands an image of `width` x `height` out as.
    #[must_use]
    pub const fn output_size(self, width: usize, height: usize) -> (usize, usize) {
        if self.transpose {
            (height, width)
        } else {
            (width, height)
        }
    }

    /// Source sample of the destination sample `(x, y)` of an `output_width` x
    /// `output_height` frame.
    ///
    /// The mirrors are applied first, in the frame's coordinates, and the
    /// transpose swaps what is left: a mirror and a transpose do not commute,
    /// and this order is the one that makes exif 5 the plain transpose and exif
    /// 7 its anti-diagonal twin.
    #[must_use]
    pub const fn source_of(
        self,
        x: usize,
        y: usize,
        output_width: usize,
        output_height: usize,
    ) -> (usize, usize) {
        let x = if self.flip_x { output_width - 1 - x } else { x };
        let y = if self.flip_y {
            output_height - 1 - y
        } else {
            y
        };
        if self.transpose { (y, x) } else { (x, y) }
    }
}

/// Code that undoes `orientation`, for a decoder that has already handed the
/// picture out the way the file describes it.
///
/// The eight codes are the symmetry group of the square, in which the two
/// quarter turns are each other's inverse and every other code is its own. A
/// stored picture is the displayed one with the inverse applied; see
/// [`crate::formats::jxl`], which is the one decoder here that applies the code
/// itself.
#[must_use]
pub const fn inverse_orientation(orientation: Orientation) -> Orientation {
    match orientation {
        Orientation::Rotate90 => Orientation::Rotate270,
        Orientation::Rotate270 => Orientation::Rotate90,
        other => other,
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

#[derive(Clone, Copy, Debug)]
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
    /// This sample moved down out of the word it was scaled into, which is how
    /// a frame that names a depth below its word holds it. A float sample is
    /// never scaled into anything and is returned unchanged.
    fn shifted(self, bits: u32) -> Self;
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

    #[inline(always)]
    fn shifted(self, bits: u32) -> Self {
        // The one byte formats are the eight bit ones, whose word is the whole
        // depth they name, so nothing is ever scaled into them.
        debug_assert_eq!(bits, 0);
        self
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

            #[inline(always)]
            fn shifted(self, bits: u32) -> Self {
                self >> bits
            }
        }
    };
}

native_sample!(u16, 2);

impl Sample for f32 {
    const SIZE: usize = 4;

    #[inline(always)]
    fn load(source: &[u8]) -> Self {
        let bytes: [u8; 4] = source[..4].try_into().expect("the sample size is fixed");
        Self::from_ne_bytes(bytes)
    }

    #[inline(always)]
    fn store(self, destination: &mut [u8]) {
        destination[..4].copy_from_slice(&self.to_ne_bytes());
    }

    #[inline(always)]
    fn shifted(self, bits: u32) -> Self {
        // A float frame's depth is the width of its own word, so only an integer
        // format ever names a depth below the samples it holds.
        debug_assert_eq!(bits, 0);
        self
    }
}

/// What a frame declares about the depth of the samples written into it.
struct FrameDepth {
    /// How far right one decoded sample moves to fit the frame's depth.
    shift: u32,
    /// Largest sample the frame holds, which is what an opaque alpha plane is
    /// filled with: a ten bit alpha frame is opaque at 1023, not at 65535.
    maximum: u16,
}

/// Reads the depth a frame declares and checks it against the samples the
/// decoder returned.
///
/// Every interleaved source hands its samples scaled to the whole word they are
/// stored in: `image` decodes a ten bit file into two byte samples holding the
/// ten bit value shifted up by the six bits the range it reports is wider, and
/// jpeg xl's decoder scales its own pipeline onto the whole range of the word it
/// was asked for. A frame whose format names a depth holds the sample itself,
/// right aligned at that depth, so the two are the same number exactly when the
/// frame declares the whole word - which is what an eight or a sixteen bit file,
/// and every file whose container states no depth, gets.
///
/// The shift recovers the sample exactly rather than approximately, whatever the
/// two depths are: scaling a sample of `bits` bits onto a word of `word` bits
/// multiplies it by less than `2^(word - bits) + 1`, and the largest scaled
/// sample stays below the smallest one of the value after it, so dividing the
/// scaled sample back down by that power of two returns the sample it started
/// as.
///
/// The two word sizes are checked to agree first, because every write moves
/// whole samples of the decoder's width into the frame's.
fn frame_depth(frame: &VideoFrame, source: PixelFormat) -> Result<FrameDepth> {
    let word_bytes = source.bytes_per_sample();
    let format = frame.get_video_format();
    let frame_bytes = usize::try_from(format.bytes_per_sample)
        .map_err(|_| ImgSeqError::new("VapourSynth returned a negative sample size"))?;
    if frame_bytes != word_bytes {
        return Err(ImgSeqError::new(format!(
            "the frame holds {frame_bytes} byte samples, but the decoder returned {word_bytes} byte samples"
        )));
    }
    let word_bits = u32::try_from(word_bytes * 8).expect("a sample is at most four bytes");
    let frame_bits = u32::try_from(format.bits_per_sample)
        .map_err(|_| ImgSeqError::new("VapourSynth returned a negative sample depth"))?;
    if frame_bits > word_bits {
        return Err(ImgSeqError::new(format!(
            "the frame declares {frame_bits} bit samples, but the decoder returned {word_bits} bit samples"
        )));
    }

    Ok(FrameDepth {
        shift: word_bits - frame_bits,
        maximum: if frame_bits >= 16 {
            u16::MAX
        } else {
            ((1u32 << frame_bits) - 1) as u16
        },
    })
}

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

/// Copy a packed row into a frame row of the same width, moving every sample
/// down by `shift`.
///
/// A frame that declares the whole word of its samples takes the row as it
/// stands, which one copy moves; a narrower one holds the sample itself rather
/// than the word it was scaled into, which is one move per sample.
fn copy_samples<T: Sample>(source: &[u8], destination: &mut [u8], shift: u32) {
    if shift == 0 {
        destination.copy_from_slice(source);
        return;
    }
    // Both rows are whole samples wide, because they were built from a width, so
    // no partial sample is ever moved.
    for (group, sample) in source.chunks(T::SIZE).zip(destination.chunks_mut(T::SIZE)) {
        T::store(T::load(group).shifted(shift), sample);
    }
}

/// Extract one interleaved channel of a single row into a packed row.
#[inline(always)]
fn extract_channel<T: Sample, const CHANNELS: usize, const CHANNEL: usize>(
    source: &[u8],
    destination: &mut [u8],
    shift: u32,
) {
    let source_stride = CHANNELS * T::SIZE;
    let sample_start = CHANNEL * T::SIZE;
    // The destination row holds exactly one sample per source group, so
    // `chunks_mut` never yields a partial sample.
    let groups = source.chunks_exact(source_stride);
    let samples = destination.chunks_mut(T::SIZE);
    // A frame that declares the whole word takes the sample as it stands, and
    // the two cases are told apart here so that the loop below is one move per
    // sample either way.
    if shift == 0 {
        for (group, sample) in groups.zip(samples) {
            let group = &group[sample_start..sample_start + T::SIZE];
            T::store(T::load(group), sample);
        }
        return;
    }
    for (group, sample) in groups.zip(samples) {
        let group = &group[sample_start..sample_start + T::SIZE];
        T::store(T::load(group).shifted(shift), sample);
    }
}

/// One plane of a frame, with the geometry every write walks it by.
///
/// The row size is the frame's own, which a transform may have swapped relative
/// to the decoder's, and `rows` is how many rows of it are written.
#[derive(Clone, Copy)]
struct PlaneTarget {
    /// First byte of the plane.
    destination: *mut u8,
    /// Distance between the starts of two rows, which is at least `row_bytes`.
    stride: isize,
    /// Bytes of one active row.
    row_bytes: usize,
    /// Active rows of the plane.
    rows: usize,
}

impl PlaneTarget {
    /// First byte of row `row` of this plane.
    ///
    /// Only the `rows` rows [`plane_target`] checked against the plane's size
    /// are inside it, which is what every caller must stay below.
    fn row_start(self, row: usize) -> *mut u8 {
        debug_assert!(row < self.rows);
        // Safety: the callers only walk rows that were checked against the
        // plane's size, and VapourSynth pads a plane to at least
        // `stride * height` bytes, so this offset is inside it.
        unsafe { self.destination.offset(self.stride * row as isize) }
    }
}

/// Plane `index` of `frame`, checked to hold `rows` rows of `row_bytes` bytes.
///
/// Every write path needs the same two facts checked, and the checks are the
/// only way either can fail once the decoder buffer is known to be the right
/// size.
fn plane_target(
    frame: &mut VideoFrame,
    index: usize,
    row_bytes: usize,
    rows: usize,
) -> Result<PlaneTarget> {
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

    Ok(PlaneTarget {
        destination,
        stride,
        row_bytes,
        rows,
    })
}

/// Samples per side of the square blocks a transformed write is walked in.
///
/// A transform that transposes reads across the decoder buffer while the frame
/// is written along it, so a walk that takes one destination sample at a time
/// fetches a cache line for each of them. Walking the destination in square
/// blocks instead keeps the reads of one block inside whole cache lines, and
/// those lines are reused by the block's remaining rows before the walk moves
/// on.
///
/// Eight samples a side is what the sweep recorded in
/// `docs/improvements/09-exif-orientation.md` found best on 12 megapixel pages,
/// which is about 24 bytes of a source row per block for the interleaved
/// formats. The exact size is not delicate: every value from 8 to 64 that the
/// sweep tried measured within 15% of the best one.
const TRANSFORM_BLOCK: usize = 8;

/// Call `visit` with each destination row of a transformed write, and the run
/// of columns of it to write, in the order that keeps one block of the decoder
/// buffer's reads in cache.
///
/// Every row is visited once with each of its runs of columns, in order, so a
/// caller writes complete rows and never revisits a sample.
fn for_each_block(output_width: usize, rows: usize, mut visit: impl FnMut(usize, Range<usize>)) {
    for block_row in (0..rows).step_by(TRANSFORM_BLOCK) {
        let block_rows = block_row..(block_row + TRANSFORM_BLOCK).min(rows);
        for block_column in (0..output_width).step_by(TRANSFORM_BLOCK) {
            let columns = block_column..(block_column + TRANSFORM_BLOCK).min(output_width);
            for row in block_rows.clone() {
                visit(row, columns.clone());
            }
        }
    }
}

/// Reverse the samples of a row in place, which is what a horizontal mirror does
/// on top of the copy that put them there.
///
/// A mirrored row cannot be a `memcpy` on its own, but copying the row and
/// turning it around afterwards can be: both passes are contiguous, and the pass
/// that reads the decoder buffer still reads along it. The samples are reversed
/// one at a time and not one byte at a time, which would swap the halves of
/// every 16 bit or float sample.
fn reverse_samples(row: &mut [u8], sample_bytes: usize) {
    if sample_bytes == 1 {
        row.reverse();
        return;
    }
    let half = row.len() / sample_bytes / 2 * sample_bytes;
    let (left, right) = row.split_at_mut(half);
    for (first, last) in left
        .chunks_exact_mut(sample_bytes)
        .zip(right.rchunks_exact_mut(sample_bytes))
    {
        first.swap_with_slice(last);
    }
}

/// Write one interleaved channel of every row directly into a VapourSynth
/// plane, leaving the row padding untouched.
///
/// `shift` is how far the samples move down out of the word the decoder scaled
/// them into, which is zero for every frame that declares that whole word.
fn write_channel<T: Sample, const CHANNELS: usize, const CHANNEL: usize>(
    target: PlaneTarget,
    pixels: &[u8],
    layout: &ImageLayout,
    transform: Transform,
    shift: u32,
) {
    // A row of the frame holds whole samples, so this is the width the
    // transform is defined against.
    let output_width = target.row_bytes / T::SIZE;

    // Every transform that does not transpose reads the buffer along the row it
    // is writing, so whole rows move at once: the mirrors only decide which row
    // that is and which way round it goes.
    if !transform.transposes() {
        for row in 0..target.rows {
            let source_row = if transform.flip_y {
                target.rows - 1 - row
            } else {
                row
            };
            let source_start = source_row * layout.row_bytes;
            let source = &pixels[source_start..source_start + layout.row_bytes];
            // Safety: `plane_target` checked the row size against the stride of
            // this plane, so every active row of it fits.
            let destination =
                unsafe { slice::from_raw_parts_mut(target.row_start(row), target.row_bytes) };
            if CHANNELS == 1 {
                copy_samples::<T>(source, destination, shift);
            } else {
                extract_channel::<T, CHANNELS, CHANNEL>(source, destination, shift);
            }
            if transform.flip_x {
                reverse_samples(destination, T::SIZE);
            }
        }

        return;
    }

    // A transposing transform reads the buffer from a different row for every
    // sample it writes, so the destination is walked in blocks: see
    // [`for_each_block`]. `row_bytes` is a whole number of samples because it
    // was built from a width, so no partial sample is ever copied.
    let sample_bytes = T::SIZE;
    let group_bytes = CHANNELS * T::SIZE;
    let channel_bytes = CHANNEL * T::SIZE;
    let source = pixels.as_ptr();
    for_each_block(output_width, target.rows, |row, columns| {
        // The row size was checked against the stride of this plane, so every
        // active row of it fits and `row_bytes` is a whole number of samples.
        let destination = target.row_start(row);
        for column in columns {
            let (x, y) = transform.source_of(column, row, output_width, target.rows);
            let from = y * layout.row_bytes + x * group_bytes + channel_bytes;
            let to = column * sample_bytes;
            // Safety: the source is in the row `y` and the column `x` of the
            // validated decoder buffer, and the destination is the `column`th
            // sample of a row that was checked to hold them all, so both
            // offsets are inside their buffers and the buffers are distinct.
            unsafe {
                let source = source.add(from);
                let destination = destination.add(to);
                if shift == 0 {
                    copy_nonoverlapping(source, destination, sample_bytes);
                } else {
                    T::store(
                        T::load(slice::from_raw_parts(source, sample_bytes)).shifted(shift),
                        slice::from_raw_parts_mut(destination, sample_bytes),
                    );
                }
            }
        }
    });
}

/// Write every channel of a transposing transform into its own plane in one walk.
///
/// One pass per plane reads the whole decoder buffer once per plane; when every
/// channel has a plane of its own, one walk reads each source sample group once
/// and writes every channel of it, which is the same destination traffic for a
/// third of the reads and a third of the loop.
fn write_transposed_planes<T: Sample, const CHANNELS: usize>(
    frame: &mut VideoFrame,
    layout: &ImageLayout,
    planes: usize,
    plane_row_bytes: usize,
    pixels: &[u8],
    transform: Transform,
    shift: u32,
) -> Result<()> {
    let output_width = plane_row_bytes / T::SIZE;
    let (_, output_height) = transform.output_size(layout.width, layout.height);
    let mut targets: [Option<PlaneTarget>; 3] = [None; 3];
    for (plane, target) in targets.iter_mut().enumerate().take(planes) {
        *target = Some(plane_target(frame, plane, plane_row_bytes, output_height)?);
    }

    let group_bytes = CHANNELS * T::SIZE;
    let sample_bytes = T::SIZE;
    let source = pixels.as_ptr();
    for_each_block(output_width, output_height, |row, columns| {
        for column in columns {
            let (x, y) = transform.source_of(column, row, output_width, output_height);
            // Safety: the source is the row `y` and the column `x` of the
            // validated decoder buffer, and every plane was checked to hold the
            // `column`th sample of its row, so both offsets are inside their
            // buffers and the buffers are distinct.
            unsafe {
                let group = source.add(y * layout.row_bytes + x * group_bytes);
                let to = column * sample_bytes;
                for (plane, target) in targets.iter().enumerate().take(planes) {
                    let target = target.expect("every plane was checked");
                    let from = group.add(plane * sample_bytes);
                    let destination = target.row_start(row).add(to);
                    if shift == 0 {
                        copy_nonoverlapping(from, destination, sample_bytes);
                    } else {
                        T::store(
                            T::load(slice::from_raw_parts(from, sample_bytes)).shifted(shift),
                            slice::from_raw_parts_mut(destination, sample_bytes),
                        );
                    }
                }
            }
        }
    });

    Ok(())
}

fn write_planes<T: Sample, const CHANNELS: usize>(
    frame: &mut VideoFrame,
    layout: &ImageLayout,
    planes: usize,
    pixels: &[u8],
    transform: Transform,
    shift: u32,
) -> Result<()> {
    let (output_width, output_height) = transform.output_size(layout.width, layout.height);
    let plane_row_bytes = output_width
        .checked_mul(T::SIZE)
        .ok_or_else(|| ImgSeqError::new("image plane row is too large"))?;
    // A transpose reads the buffer across the row it is writing, so a plane at a
    // time reads the whole buffer once per plane. When every channel has a plane
    // of its own, one walk fills all of them instead.
    if transform.transposes() && planes == CHANNELS && matches!(planes, 2 | 3) {
        return write_transposed_planes::<T, CHANNELS>(
            frame,
            layout,
            planes,
            plane_row_bytes,
            pixels,
            transform,
            shift,
        );
    }
    for plane in 0..planes {
        let target = plane_target(frame, plane, plane_row_bytes, output_height)?;
        match plane {
            0 => write_channel::<T, CHANNELS, 0>(target, pixels, layout, transform, shift),
            1 => write_channel::<T, CHANNELS, 1>(target, pixels, layout, transform, shift),
            _ => write_channel::<T, CHANNELS, 2>(target, pixels, layout, transform, shift),
        }
    }

    Ok(())
}

/// Planes of a frame a decoder buffer of `channels` channels can fill.
///
/// A frame can ask for fewer planes than the buffer holds channels, which is how
/// a monochrome avif is handed out: the `image` decoder converts it to r,g,b,
/// every channel holding the one sample the file stores, so the leading channel
/// fills the single gray plane. A frame that asks for more planes than the
/// buffer has channels cannot be filled at all.
fn planes_to_write(frame_planes: i32, channels: usize) -> Result<usize> {
    let planes = usize::try_from(frame_planes)
        .map_err(|_| ImgSeqError::new("VapourSynth returned a negative plane count"))?;
    if planes > channels {
        return Err(ImgSeqError::new(format!(
            "the frame has {planes} planes, but the decoder returned {channels} channels"
        )));
    }
    Ok(planes)
}

/// Convert one interleaved decoded image directly into the VapourSynth planes.
///
/// The frame holds what `transform` produces, so an orientation that transposes
/// the picture is written into a frame that is as tall as the image is wide.
/// The frame's own depth decides whether the samples are written as they were
/// decoded or moved down out of the word they were scaled into; see
/// [`frame_depth`].
pub fn write_planar(
    frame: &mut VideoFrame,
    color_type: ColorType,
    width: u32,
    height: u32,
    pixels: &[u8],
    transform: Transform,
) -> Result<WriteTimings> {
    let layout = image_layout(color_type, width, height, pixels)?;
    let planes = planes_to_write(frame.get_video_format().num_planes, layout.channels)?;
    let shift = frame_depth(frame, layout.format)?.shift;
    let started = Instant::now();
    match (layout.format.bytes_per_sample(), layout.channels) {
        (1, 1) => write_planes::<u8, 1>(frame, &layout, planes, pixels, transform, shift)?,
        (1, 2) => write_planes::<u8, 2>(frame, &layout, planes, pixels, transform, shift)?,
        (1, 3) => write_planes::<u8, 3>(frame, &layout, planes, pixels, transform, shift)?,
        (1, 4) => write_planes::<u8, 4>(frame, &layout, planes, pixels, transform, shift)?,
        (2, 1) => write_planes::<u16, 1>(frame, &layout, planes, pixels, transform, shift)?,
        (2, 2) => write_planes::<u16, 2>(frame, &layout, planes, pixels, transform, shift)?,
        (2, 3) => write_planes::<u16, 3>(frame, &layout, planes, pixels, transform, shift)?,
        (2, 4) => write_planes::<u16, 4>(frame, &layout, planes, pixels, transform, shift)?,
        (4, 3) => write_planes::<f32, 3>(frame, &layout, planes, pixels, transform, shift)?,
        (4, 4) => write_planes::<f32, 4>(frame, &layout, planes, pixels, transform, shift)?,
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
/// Unlike [`write_planar`] the buffers already have the plane layout, so a
/// transform-free write only moves whole rows around the frame padding, which is
/// what makes the planar formats cheaper to write: there is no per sample work
/// at all. A transform writes one sample at a time instead, in blocks that keep
/// its reads in cache, because a transposed plane is read down a column rather
/// than along a row.
pub fn write_decoded_planes(
    frame: &mut VideoFrame,
    format: PixelFormat,
    width: u32,
    height: u32,
    planes: &[Vec<u8>],
    transform: Transform,
) -> Result<WriteTimings> {
    // The sample size is a constant of the write rather than a value it carries,
    // so that a transformed plane moves whole samples instead of calling a copy
    // routine once per sample for it.
    match format.bytes_per_sample() {
        1 => write_sample_planes::<u8>(frame, format, width, height, planes, transform),
        2 => write_sample_planes::<u16>(frame, format, width, height, planes, transform),
        4 => write_sample_planes::<f32>(frame, format, width, height, planes, transform),
        bytes_per_sample => Err(ImgSeqError::new(format!(
            "unsupported sample size {bytes_per_sample} for {}",
            format.name()
        ))),
    }
}

fn write_sample_planes<T: Sample>(
    frame: &mut VideoFrame,
    format: PixelFormat,
    width: u32,
    height: u32,
    planes: &[Vec<u8>],
    transform: Transform,
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
    let sample_bytes = T::SIZE;
    let (output_width, output_height) = transform.output_size(width, height);
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
        let (frame_width, frame_height) =
            format.frame_plane_dimensions(index, output_width, output_height);
        let row_bytes = frame_width
            .checked_mul(sample_bytes)
            .ok_or_else(|| ImgSeqError::new("image plane row is too large"))?;
        // A transform reads across the plane as well as along it, so what has to
        // fit is the source turned the same way the frame was. On an odd size a
        // subsampled plane is rounded up by the decoder and down by the frame,
        // and this is what keeps the reads inside the buffer either way.
        let (fits, mapped_width, mapped_height) = if transform.transposes() {
            (
                frame_width <= decoded_height && frame_height <= decoded_width,
                decoded_height,
                decoded_width,
            )
        } else {
            (
                decoded_height >= frame_height && decoded_row >= row_bytes,
                decoded_width,
                decoded_height,
            )
        };
        if !fits {
            return Err(ImgSeqError::new(format!(
                "decoder returned a {decoded_width}x{decoded_height} plane {index}, which cannot fill the {frame_width}x{frame_height} the frame holds from a {mapped_width}x{mapped_height} source"
            )));
        }

        let target = plane_target(frame, index, row_bytes, frame_height)?;

        // Every transform that does not transpose reads the buffer along the row
        // it is writing, so whole rows move at once: a mirror only decides which
        // row that is and which way round it goes. `row_bytes` is a whole number
        // of samples because it came from a width, so no partial sample is ever
        // copied.
        if !transform.transposes() {
            for row in 0..frame_height {
                let source_row = if transform.flip_y {
                    frame_height - 1 - row
                } else {
                    row
                };
                // Every row of the decoder buffer is `decoded_row` bytes wide,
                // and the frame keeps the first `row_bytes` of it.
                let source =
                    &buffer[source_row * decoded_row..source_row * decoded_row + row_bytes];
                // Safety: `plane_target` checked the plane against these rows,
                // so every active row fits in it.
                let destination =
                    unsafe { slice::from_raw_parts_mut(target.row_start(row), row_bytes) };
                destination.copy_from_slice(source);
                if transform.flip_x {
                    reverse_samples(destination, sample_bytes);
                }
            }

            continue;
        }

        // A transpose reads the buffer from a different row for every sample it
        // writes, so the destination is walked in blocks: see [`for_each_block`].
        let source = buffer.as_ptr();
        for_each_block(frame_width, frame_height, |row, columns| {
            // The plane was checked against these rows, so every active row of
            // it fits and `row_bytes` is a whole number of samples.
            let destination = target.row_start(row);
            for column in columns {
                let (x, y) = transform.source_of(column, row, frame_width, frame_height);
                let from = y * decoded_row + x * sample_bytes;
                let to = column * sample_bytes;
                // Safety: the source is in the row `y` and the sample `x` of a
                // decoder plane that was checked to hold those, and the
                // destination is the `column`th sample of a row that was checked
                // to hold them all, so both offsets are inside their buffers and
                // the buffers are distinct.
                unsafe {
                    copy_nonoverlapping(source.add(from), destination.add(to), sample_bytes);
                }
            }
        });
    }

    Ok(WriteTimings {
        deinterleave: started.elapsed(),
    })
}

/// Fill the active rows of a plane with `value`.
fn fill_plane<T: Sample>(target: PlaneTarget, value: T) {
    for row in 0..target.rows {
        // Safety: `plane_target` checked the plane against these rows, and
        // `row_bytes` is a whole number of samples.
        let bytes = unsafe { slice::from_raw_parts_mut(target.row_start(row), target.row_bytes) };
        for sample in bytes.chunks_mut(T::SIZE) {
            value.store(sample);
        }
    }
}

/// Fill the single plane of a gray frame with the opaque value.
///
/// Sources that decode to planes have no alpha channel to write, so their
/// alpha clip is opaque everywhere. `width` and `height` are the size of the
/// frame, which a rotation may have swapped relative to the file.
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
    let target = plane_target(frame, 0, row_bytes, height)?;
    let started = Instant::now();
    match format.bytes_per_sample() {
        1 => fill_plane::<u8>(target, u8::MAX),
        2 => fill_plane::<u16>(target, format.integer_max()),
        4 => fill_plane::<f32>(target, 1.0),
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
/// always has a meaningful value for every frame. The plane is transformed the
/// same way the colour plane is, which is what keeps the two aligned, and it is
/// written at the frame's own depth: a ten bit alpha clip holds the sample
/// itself, and its opaque fill is 1023.
pub fn write_alpha(
    frame: &mut VideoFrame,
    color_type: ColorType,
    width: u32,
    height: u32,
    pixels: &[u8],
    transform: Transform,
) -> Result<WriteTimings> {
    let layout = image_layout(color_type, width, height, pixels)?;
    let depth = frame_depth(frame, layout.format)?;
    let started = Instant::now();
    match (layout.format.bytes_per_sample(), alpha_channel(color_type)) {
        (1, Some(1)) => {
            write_alpha_plane::<u8, 2, 1>(frame, &layout, pixels, transform, depth.shift)?;
        }
        (2, Some(1)) => {
            write_alpha_plane::<u16, 2, 1>(frame, &layout, pixels, transform, depth.shift)?;
        }
        (1, Some(3)) => {
            write_alpha_plane::<u8, 4, 3>(frame, &layout, pixels, transform, depth.shift)?;
        }
        (2, Some(3)) => {
            write_alpha_plane::<u16, 4, 3>(frame, &layout, pixels, transform, depth.shift)?;
        }
        (4, Some(3)) => {
            write_alpha_plane::<f32, 4, 3>(frame, &layout, pixels, transform, depth.shift)?;
        }
        (1, None) => fill_alpha_plane::<u8>(frame, &layout, transform, u8::MAX)?,
        (2, None) => fill_alpha_plane::<u16>(frame, &layout, transform, depth.maximum)?,
        (4, None) => fill_alpha_plane::<f32>(frame, &layout, transform, 1.0)?,
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

fn alpha_plane_row_bytes<T: Sample>(layout: &ImageLayout, transform: Transform) -> Result<usize> {
    transform
        .output_size(layout.width, layout.height)
        .0
        .checked_mul(T::SIZE)
        .ok_or_else(|| ImgSeqError::new("image plane row is too large"))
}

/// Write the alpha channel into the single plane of a gray frame.
fn write_alpha_plane<T: Sample, const CHANNELS: usize, const CHANNEL: usize>(
    frame: &mut VideoFrame,
    layout: &ImageLayout,
    pixels: &[u8],
    transform: Transform,
    shift: u32,
) -> Result<()> {
    let (_, output_height) = transform.output_size(layout.width, layout.height);
    let row_bytes = alpha_plane_row_bytes::<T>(layout, transform)?;
    let target = plane_target(frame, 0, row_bytes, output_height)?;
    write_channel::<T, CHANNELS, CHANNEL>(target, pixels, layout, transform, shift);
    Ok(())
}

/// Fill the single plane of a gray frame with one value.
fn fill_alpha_plane<T: Sample>(
    frame: &mut VideoFrame,
    layout: &ImageLayout,
    transform: Transform,
    value: T,
) -> Result<()> {
    let (_, output_height) = transform.output_size(layout.width, layout.height);
    let row_bytes = alpha_plane_row_bytes::<T>(layout, transform)?;
    let target = plane_target(frame, 0, row_bytes, output_height)?;
    // A constant plane has nothing to rearrange, so the transform is only in
    // the size it covers.
    fill_plane::<T>(target, value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        PixelFormat, TRANSFORM_BLOCK, Transform, alpha_channel, extract_channel, for_each_block,
        image_layout, inverse_orientation, planes_to_write, reverse_samples,
    };
    use image::{ColorType, metadata::Orientation};
    use vapoursynth4_rs::{ColorFamily, SampleType};

    /// The 3x2 source every orientation test rearranges, whose values are
    /// `row * 3 + column + 1` so a wrong sample names itself in the failure.
    const SOURCE: [[u8; 3]; 2] = [[1, 2, 3], [4, 5, 6]];

    /// Reads one sample of the source grid.
    fn source(x: usize, y: usize) -> u8 {
        SOURCE[y][x]
    }

    /// The frame a transform hands `SOURCE` out as, one row per entry.
    fn transform(orientation: Orientation) -> Vec<Vec<u8>> {
        let transform = Transform::from_orientation(orientation);
        let (width, height) = transform.output_size(3, 2);
        (0..height)
            .map(|row| {
                (0..width)
                    .map(|column| {
                        let (x, y) = transform.source_of(column, row, width, height);
                        source(x, y)
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn applies_every_exif_orientation() {
        // Each expectation is the picture a viewer shows, the way `image`'s own
        // `Orientation::apply` builds it: the rotation first and then the
        // mirror, which is what makes exif 5 the transpose and exif 7 the other
        // one.
        assert_eq!(transform(Orientation::NoTransforms), [[1, 2, 3], [4, 5, 6]]);
        assert_eq!(
            transform(Orientation::FlipHorizontal),
            [[3, 2, 1], [6, 5, 4]]
        );
        assert_eq!(transform(Orientation::Rotate180), [[6, 5, 4], [3, 2, 1]]);
        assert_eq!(transform(Orientation::FlipVertical), [[4, 5, 6], [1, 2, 3]]);
        assert_eq!(
            transform(Orientation::Rotate90FlipH),
            [[1, 4], [2, 5], [3, 6]]
        );
        assert_eq!(transform(Orientation::Rotate90), [[4, 1], [5, 2], [6, 3]]);
        assert_eq!(
            transform(Orientation::Rotate270FlipH),
            [[6, 3], [5, 2], [4, 1]]
        );
        assert_eq!(transform(Orientation::Rotate270), [[3, 6], [2, 5], [1, 4]]);
    }

    #[test]
    fn every_orientation_reads_each_sample_once() {
        // A transform that loses or doubles a sample still has the right size,
        // so the writes are only correct when the sources are a permutation.
        for orientation in [
            Orientation::NoTransforms,
            Orientation::FlipHorizontal,
            Orientation::Rotate180,
            Orientation::FlipVertical,
            Orientation::Rotate90FlipH,
            Orientation::Rotate90,
            Orientation::Rotate270FlipH,
            Orientation::Rotate270,
        ] {
            let mut seen = transform(orientation).concat();
            seen.sort_unstable();
            assert_eq!(seen, [1, 2, 3, 4, 5, 6], "{orientation:?}");
        }
    }

    #[test]
    fn reports_the_size_and_the_transpose() {
        assert_eq!(Transform::IDENTITY.output_size(3, 2), (3, 2));
        assert_eq!(
            Transform::from_orientation(Orientation::NoTransforms),
            Transform::IDENTITY
        );
        assert_eq!(
            Transform::from_orientation(Orientation::Rotate90).output_size(3, 2),
            (2, 3)
        );
        assert!(Transform::from_orientation(Orientation::Rotate90).transposes());
        assert!(!Transform::from_orientation(Orientation::FlipVertical).transposes());
        // Only the identity leaves every sample where it was, and a transpose
        // keeps the diagonal, so an off-diagonal sample is what tells them
        // apart.
        for orientation in [
            Orientation::FlipHorizontal,
            Orientation::Rotate180,
            Orientation::FlipVertical,
            Orientation::Rotate90FlipH,
            Orientation::Rotate90,
            Orientation::Rotate270FlipH,
            Orientation::Rotate270,
        ] {
            let transform = Transform::from_orientation(orientation);
            let (width, height) = transform.output_size(3, 2);
            assert!(
                transform.source_of(0, 0, width, height) != (0, 0)
                    || transform.source_of(1, 0, width, height) != (1, 0),
                "{orientation:?}"
            );
        }
    }

    #[test]
    fn every_exif_code_round_trips() {
        // `probe` reads the code and `color` writes it back, so a code that
        // does not survive the pair would report a different orientation than
        // the one that was applied.
        for code in 1..=8u8 {
            let orientation = Orientation::from_exif(code).expect("a known exif code");
            assert_eq!(orientation.to_exif(), code);
        }
    }

    #[test]
    fn undoing_an_orientation_hands_the_stored_picture_back() {
        // The jxl decoder applies the file's own code, so the stored picture is
        // the one its inverse produces; the two composed have to be the
        // identity for every code, not only for the quarter turns.
        for code in 1..=8u8 {
            let orientation = Orientation::from_exif(code).expect("a known exif code");
            let shown = rearrange(orientation, &source_grid());
            let restored = rearrange(inverse_orientation(orientation), &shown);
            assert_eq!(restored, source_grid(), "exif {code}");
        }
    }

    /// `SOURCE` as the rows a transform rearranges.
    fn source_grid() -> Vec<Vec<u8>> {
        SOURCE.iter().map(|row| row.to_vec()).collect()
    }

    /// Applies an orientation to a grid the way the writer applies it to a
    /// decoder buffer, which is sample by sample from `source_of`.
    fn rearrange(orientation: Orientation, grid: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let transform = Transform::from_orientation(orientation);
        let (width, height) = transform.output_size(grid[0].len(), grid.len());
        (0..height)
            .map(|row| {
                (0..width)
                    .map(|column| {
                        let (x, y) = transform.source_of(column, row, width, height);
                        grid[y][x]
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_gray_frame_can_be_filled_from_a_wider_buffer() {
        assert_eq!(planes_to_write(1, 3).unwrap(), 1);
        assert_eq!(planes_to_write(1, 4).unwrap(), 1);
        assert_eq!(planes_to_write(3, 3).unwrap(), 3);
        let error = planes_to_write(3, 1).expect_err("one channel cannot fill three planes");
        assert!(error.to_string().contains("3 planes"), "{error}");
    }

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
        extract_channel::<u8, 4, 0>(&pixels, &mut red, 0);
        extract_channel::<u8, 4, 1>(&pixels, &mut green, 0);
        extract_channel::<u8, 4, 2>(&pixels, &mut blue, 0);
        assert_eq!((red, green, blue), ([1, 4], [2, 5], [3, 6]));
    }

    #[test]
    fn extracts_gray16() {
        let pixels = [1, 2, 3, 4];
        let mut plane = [0; 4];
        extract_channel::<u16, 1, 0>(&pixels, &mut plane, 0);
        assert_eq!(plane, pixels);
    }

    #[test]
    fn extracts_a_channel_at_a_narrower_depth() {
        // Three ten bit samples, each left aligned in the sixteen bit word
        // `image` hands a deeper file over in, extracted into the ten bit plane
        // that holds the sample itself.
        let values: [u16; 3] = [1, 2, 1023];
        let mut pixels = Vec::new();
        for value in values {
            pixels.extend_from_slice(&(value << 6).to_ne_bytes());
        }
        let mut expected = Vec::new();
        for value in values {
            expected.extend_from_slice(&value.to_ne_bytes());
        }

        let mut plane = vec![0; expected.len()];
        extract_channel::<u16, 1, 0>(&pixels, &mut plane, 6);
        assert_eq!(plane, expected);
    }

    #[test]
    fn a_scaled_sample_moves_back_down_to_the_one_it_started_as() {
        // A decoder hands a deeper sample over scaled onto the whole word it is
        // stored in, either by shifting it up or by scaling it onto the word's
        // range with a round, and the writer moves it back down by the
        // difference between the two depths. That has to return the sample it
        // started as, whatever the two depths are.
        for bits in 1..=16u32 {
            let word = if bits <= 8 { 8 } else { 16 };
            let shift = word - bits;
            let maximum = (1u32 << bits) - 1;
            for value in 0..=maximum {
                assert_eq!(
                    (value << shift) >> shift,
                    value,
                    "{bits} bit {value} shifted"
                );
                let scaled = (value * ((1u32 << word) - 1) + maximum / 2) / maximum;
                assert_eq!(
                    scaled >> shift,
                    value,
                    "{bits} bit {value} scaled onto {word} bits"
                );
            }
        }
    }

    #[test]
    fn narrows_a_word_to_the_depth_a_container_states() {
        for (bits, gray, rgb) in [
            (9, PixelFormat::Gray9, PixelFormat::Rgb9),
            (10, PixelFormat::Gray10, PixelFormat::Rgb10),
            (11, PixelFormat::Gray11, PixelFormat::Rgb11),
            (12, PixelFormat::Gray12, PixelFormat::Rgb12),
            (13, PixelFormat::Gray13, PixelFormat::Rgb13),
            (14, PixelFormat::Gray14, PixelFormat::Rgb14),
            (15, PixelFormat::Gray15, PixelFormat::Rgb15),
            (16, PixelFormat::Gray16, PixelFormat::Rgb16),
        ] {
            assert_eq!(PixelFormat::Gray16.at_depth(bits), gray);
            assert_eq!(PixelFormat::Rgb16.at_depth(bits), rgb);
        }

        // A depth no format names leaves the word alone rather than rounding the
        // samples into one that would misstate them, and a decimal depth that is
        // not a depth at all is the same case.
        for bits in [0, 1, 7, 17, 32] {
            assert_eq!(PixelFormat::Gray16.at_depth(bits), PixelFormat::Gray16);
            assert_eq!(PixelFormat::Rgb16.at_depth(bits), PixelFormat::Rgb16);
        }

        // An eight bit word is the whole depth of the formats it names, so a
        // container that states another depth does not widen it, and a float or
        // planar format is already the depth it holds.
        assert_eq!(PixelFormat::Gray8.at_depth(9), PixelFormat::Gray8);
        assert_eq!(PixelFormat::Rgb8.at_depth(12), PixelFormat::Rgb8);
        assert_eq!(PixelFormat::Rgb32F.at_depth(10), PixelFormat::Rgb32F);
        assert_eq!(PixelFormat::Yuv420P10.at_depth(10), PixelFormat::Yuv420P10);
        assert_eq!(PixelFormat::Yuv444P12.at_depth(10), PixelFormat::Yuv444P12);
    }

    #[test]
    fn names_every_depth_a_container_can_state() {
        // The name is what a graph reads and what the validator checks: a gray
        // format is named after the depth of its one sample, an r,g,b one after
        // the bits of its three, and the depth every container states from eight
        // to sixteen is one of them.
        for bits in 8..=16i32 {
            let depth = u32::try_from(bits).unwrap();
            let gray = PixelFormat::Gray16.at_depth(depth);
            let rgb = PixelFormat::Rgb16.at_depth(depth);
            assert_eq!(gray.name(), format!("Gray{bits}"));
            assert_eq!(rgb.name(), format!("RGB{}", bits * 3));
            assert_eq!(gray.bits_per_sample(), bits);
            assert_eq!(rgb.bits_per_sample(), bits);
            assert_eq!(gray.color_family(), ColorFamily::Gray);
            assert_eq!(rgb.color_family(), ColorFamily::RGB);
            assert_eq!(gray.sample_type(), SampleType::Integer);
            let bytes = if bits <= 8 { 1 } else { 2 };
            assert_eq!(gray.bytes_per_sample(), bytes);
            assert_eq!(rgb.bytes_per_sample(), bytes);
            assert_eq!(
                gray.integer_max(),
                if bits >= 16 {
                    u16::MAX
                } else {
                    ((1u32 << bits) - 1) as u16
                }
            );
            assert_eq!(gray.plane_count(), 1);
            assert_eq!(rgb.plane_count(), 3);
            assert_eq!(rgb.sub_sampling(), (0, 0));
            // The alpha clip of a source is the same depth as its colour, so an
            // opaque plane is filled with the same maximum.
            assert_eq!(gray.alpha_format().bits_per_sample(), bits);
            assert_eq!(rgb.alpha_format().bits_per_sample(), bits);
            assert_eq!(rgb.alpha_format().name(), format!("Gray{bits}"));
            assert_eq!(gray.alpha_format().integer_max(), gray.integer_max());
            assert_eq!(rgb.alpha_format().integer_max(), rgb.integer_max());
            assert_eq!(gray.alpha_format().plane_count(), 1);
        }
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
    fn the_opaque_fill_is_the_largest_sample_of_the_format() {
        assert_eq!(PixelFormat::Gray8.integer_max(), 255);
        assert_eq!(PixelFormat::Rgb8.integer_max(), 255);
        assert_eq!(PixelFormat::Gray10.integer_max(), 1023);
        assert_eq!(PixelFormat::Yuv420P10.integer_max(), 1023);
        assert_eq!(PixelFormat::Gray12.integer_max(), 4095);
        assert_eq!(PixelFormat::Yuv444P12.integer_max(), 4095);
        assert_eq!(PixelFormat::Gray16.integer_max(), u16::MAX);
        // The fill of a format is the maximum of the alpha format it hands out.
        for format in [PixelFormat::Gray10, PixelFormat::Yuv444P12] {
            assert_eq!(format.alpha_format().integer_max(), format.integer_max());
        }
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
        extract_channel::<u8, 2, 1>(&gray_alpha, &mut alpha, 0);
        assert_eq!(alpha, [99, 98]);

        let rgba = [1, 2, 3, 4, 5, 6, 7, 8];
        extract_channel::<u8, 4, 3>(&rgba, &mut alpha, 0);
        assert_eq!(alpha, [4, 8]);

        let rgba16 = [
            0, 1, 0, 2, 0, 3, 0, 4, //
            0, 5, 0, 6, 0, 7, 0, 8,
        ];
        let mut alpha = [0; 4];
        extract_channel::<u16, 4, 3>(&rgba16, &mut alpha, 0);
        assert_eq!(alpha, [0, 4, 0, 8]);

        let mut rgba32f = Vec::new();
        for value in [1.0_f32, 2.0, 3.0, 0.25, 4.0, 5.0, 6.0, 0.75] {
            rgba32f.extend_from_slice(&value.to_ne_bytes());
        }
        let mut alpha = [0; 8];
        extract_channel::<f32, 4, 3>(&rgba32f, &mut alpha, 0);
        assert_eq!(
            f32::from_ne_bytes(alpha[0..4].try_into().unwrap()),
            0.25_f32
        );
        assert_eq!(
            f32::from_ne_bytes(alpha[4..8].try_into().unwrap()),
            0.75_f32
        );
    }

    #[test]
    fn reverses_rows_in_samples() {
        let mut bytes = [1, 2, 3, 4, 5, 6];
        reverse_samples(&mut bytes, 1);
        assert_eq!(bytes, [6, 5, 4, 3, 2, 1]);

        // A wider sample is turned around as a whole: reversing its bytes would
        // swap the halves of every value.
        let samples = [1u16, 2, 3, 4, 5];
        let mut row = Vec::new();
        for sample in samples {
            row.extend_from_slice(&sample.to_ne_bytes());
        }
        reverse_samples(&mut row, 2);
        let mut expected = Vec::new();
        for sample in samples.iter().rev() {
            expected.extend_from_slice(&sample.to_ne_bytes());
        }
        assert_eq!(row, expected);

        // An odd row leaves its middle sample alone, which is the sample the
        // empty half of the split never reaches.
        let mut odd = [1u8, 2, 3, 4, 5, 6, 7];
        reverse_samples(&mut odd, 1);
        assert_eq!(odd, [7, 6, 5, 4, 3, 2, 1]);

        // A row of one sample is already reversed, and a wider sample than the
        // row is never split.
        let mut single = [9u8, 8, 7, 6];
        reverse_samples(&mut single, 4);
        assert_eq!(single, [9, 8, 7, 6]);
        reverse_samples(&mut single, 2);
        assert_eq!(single, [7, 6, 9, 8]);
    }

    #[test]
    fn blocked_walk_covers_every_sample_once() {
        // The walk is the only thing a transform write shares with the write it
        // replaced, so a block that is off by one either skips a row or writes
        // one twice. The sizes go past a block on both sides, which is where a
        // partial last block is walked.
        for (width, height) in [(70, 33), (33, 70), (1, 1), (1, 70), (70, 1), (64, 64)] {
            let mut visits = vec![0u8; width * height];
            for_each_block(width, height, |row, columns| {
                for column in columns {
                    visits[row * width + column] += 1;
                }
            });
            assert!(
                visits.iter().all(|count| *count == 1),
                "{width}x{height} visits {visits:?}"
            );
        }
    }

    #[test]
    fn blocked_walk_keeps_rows_in_order() {
        // The reads of a block are cheap because the writes are not: each row
        // has to be written in ascending, gapless runs of columns from a block
        // boundary, so that the destination stays contiguous inside the block
        // and a row is whole once the walk has seen it.
        let mut written: Vec<Vec<usize>> = vec![Vec::new(); 33];
        for_each_block(70, 33, |row, columns| {
            assert!(columns.end <= 70);
            assert_eq!(columns.start % TRANSFORM_BLOCK, 0);
            written[row].extend(columns);
        });
        for (row, columns) in written.iter().enumerate() {
            assert_eq!(*columns, (0..70).collect::<Vec<usize>>(), "row {row}");
        }
    }

    #[test]
    fn blocked_walk_writes_a_column_block_in_row_order() {
        // A transposing write reads across the buffer while it writes along it,
        // so what has to stay in cache is the rows the column block touches: the
        // walk returns to a column block with the rows that follow the ones it
        // left, and never goes back over one it has written.
        let mut rows: Vec<Vec<usize>> = vec![Vec::new(); 100_usize.div_ceil(TRANSFORM_BLOCK)];
        for_each_block(100, 70, |row, columns| {
            rows[columns.start / TRANSFORM_BLOCK].push(row);
        });
        for (block, rows) in rows.iter().enumerate() {
            let mut sorted = rows.clone();
            sorted.sort_unstable();
            assert_eq!(*rows, sorted, "column block {block} goes back over rows");
            assert_eq!(rows.len(), 70, "column block {block}");
        }
    }
}
