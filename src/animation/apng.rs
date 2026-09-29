//! Animated PNG.
//!
//! `image` can read an APNG, but its compositor is 8-bit only: the 16-bit arm
//! of its mixer is `unreachable!`, and it refuses every 16-bit colour type
//! before reaching it. A 16-bit APNG is a real file and this plugin preserves
//! 16-bit stills, so the frames are read with the `png` crate that `image`
//! wraps and composed here at the depth the file states; see
//! `docs/improvements/21-animated-images.md`.
//!
//! The `png` reader is a stream with no index and no seek, so a backward
//! request replays the file from its beginning. That is what
//! [`super::AnimationSource`] asks for on this adapter's behalf.

use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use image::ColorType;
use png::{BitDepth, BlendOp, Decoder, DisposeOp, Reader};

use crate::{
    decoder::{DecodeTimings, DecodedImage, Pixels, image_error},
    error::{ImgSeqError, Result},
    formats::png as png_format,
};

use super::{AnimationDecoder, AnimationSource, Presentation, Rate, SegmentInfo};

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    png_format::owns(path)
}

/// Describes an animated png's timeline without rendering its frames.
///
/// Returns `None` for a png that is not an animation, which leaves the file on
/// the still-image path it was on before.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is an APNG whose structure or timeline
/// cannot be read.
pub fn segment_info(
    path: &Path,
    info: crate::decoder::ImageInfo,
    fps: Rate,
) -> Result<Option<SegmentInfo>> {
    let reader = open(path)?;
    let Some(animation) = reader.info().animation_control() else {
        return Ok(None);
    };
    // A file whose default image is not the first presentation hands that image
    // over first, with no frame control of its own. It is what a reader that
    // does not know APNG would show, and `image` calls it a thumbnail; it is
    // not part of the displayed timeline.
    // Whether the default image is a poster rather than the first presentation
    // decides how many frames the file yields, which is only needed by the
    // timing pass that walks them.
    if animation.num_frames == 0 {
        return Err(ImgSeqError::new(format!(
            "animated image '{}' states no frames",
            path.display()
        )));
    }

    let (rate, presentations) = timing(path)?;
    let canvas = reader.info().size();
    let source = std::sync::Arc::new(AnimationSource::new(
        path.to_path_buf(),
        Box::new(PngDecoder::open(path, canvas, info.transform, info.format)?),
    ));
    Ok(Some(SegmentInfo {
        info,
        rate,
        presentations,
        decoder: source,
        fps,
    }))
}

/// Reads every frame's control data and the delay it states, without rendering.
///
/// A delay is a fraction of a second; a denominator of zero means one hundredth,
/// as the specification says. Both are kept as they are, so a denominator that
/// divides 1000 stays exact and one that does not is placed on the timeline by
/// the cross multiplication in [`super::Segment::animated`].
fn timing(path: &Path) -> Result<(Rate, Vec<Presentation>)> {
    let mut reader = open(path)?;
    let mut buffer = vec![0u8; canvas_bytes(&reader, path)?];
    let mut presentations = Vec::new();
    let mut timestamp = 0i64;

    // The frame count is known up front, so the pass stops there rather than
    // relying on the reader's end-of-image error, which is not distinguishable
    // from the other parsing errors it shares a variant with.
    let expected = reader
        .info()
        .animation_control()
        .map_or(0, |animation| animation.num_frames) as usize;

    while presentations.len() < expected {
        if reader.next_frame(&mut buffer).is_err() {
            return Err(ImgSeqError::new(format!(
                "animated image '{}' ended after {} of its {expected} frames",
                path.display(),
                presentations.len()
            )));
        }
        let Some(control) = reader.info().frame_control().copied() else {
            continue;
        };
        let duration = delay_ms(control.delay_num, denominator_of(control.delay_den));
        presentations.push(Presentation {
            timestamp,
            duration: Some(duration),
        });
        timestamp = timestamp.checked_add(duration).ok_or_else(|| {
            ImgSeqError::new(format!("the timeline of '{}' overflows", path.display()))
        })?;
    }

    if presentations.is_empty() {
        return Err(ImgSeqError::new(format!(
            "animated image '{}' states no frames",
            path.display()
        )));
    }
    // The timeline counts milliseconds, which is a rate of 1000 ticks a second.
    Ok((Rate::new(1000, 1), presentations))
}

fn denominator_of(delay_den: u16) -> u16 {
    if delay_den == 0 { 100 } else { delay_den }
}

/// A delay of `num/den` seconds in milliseconds.
fn delay_ms(num: u16, denominator: u16) -> i64 {
    (i64::from(num) * 1000) / i64::from(denominator)
}

fn open(path: &Path) -> Result<Reader<BufReader<File>>> {
    let file = File::open(path).map_err(|error| image_error("open", path, error))?;
    // `Decoder::new` keeps the default `Transformations::IDENTITY`, which is
    // what hands back the file's own colour type and bit depth. That is the
    // whole reason this reader is here: every other transform changes one or
    // both, and any of them would narrow a 16-bit file.
    Decoder::new(BufReader::new(file))
        .read_info()
        .map_err(|error| image_error("decode", path, error))
}

fn canvas_bytes(reader: &Reader<BufReader<File>>, path: &Path) -> Result<usize> {
    reader.output_buffer_size().ok_or_else(|| {
        ImgSeqError::new(format!(
            "animated image '{}' is too large for this platform",
            path.display()
        ))
    })
}

/// One APNG decode pass, which is the whole state of replaying the file.
///
/// The canvas is kept across presentations, so a request that moves forward one
/// presentation only has to decode that one. A backward request restarts the
/// pass from the first frame, which the shared source does by dropping this.
struct PngSource {
    path: PathBuf,
    width: u32,
    height: u32,
    /// How the pixels are rearranged as they are written, from the probe: a
    /// file that states an orientation is handed out the way it describes.
    transform: crate::pixel::Transform,
    /// The format the probe recorded, which carries the depth the file states
    /// rather than only the word its samples are stored in.
    format: crate::pixel::PixelFormat,
    /// The colour type of the frame this source hands out, which is `image`'s
    /// and is what the frame writer is told.
    color_type: ColorType,
    /// The colour type the file states, which is the `png` crate's.
    source_color_type: png::ColorType,
    sixteen_bit: bool,
    /// The composited logical canvas, which is what every presentation is.
    canvas: Vec<u8>,
    /// The pixels a frame with disposal `Previous` puts back.
    previous: Vec<u8>,
    /// The rectangle each of those covers.
    previous_rect: Option<Rect>,
    /// The rectangle the frame just shown occupied, and how to dispose of it.
    pending: Option<(Rect, DisposeOp)>,
}

/// A frame rectangle on the logical canvas, in pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl PngSource {
    fn new(
        path: &Path,
        canvas: (u32, u32),
        transform: crate::pixel::Transform,
        format: crate::pixel::PixelFormat,
    ) -> Result<Self> {
        let reader = open(path)?;
        let source_color_type = reader.info().color_type;
        let color_type = color_of(source_color_type).ok_or_else(|| {
            ImgSeqError::new(format!(
                "animated image '{}' holds an indexed frame, which is not an animation this reader hands out",
                path.display()
            ))
        })?;
        let sixteen_bit = reader.info().bit_depth == BitDepth::Sixteen;
        let (width, height) = canvas;
        Ok(Self {
            path: path.to_path_buf(),
            width,
            height,
            transform,
            format,
            color_type,
            source_color_type,
            sixteen_bit,
            canvas: background(width, height, color_type, sixteen_bit),
            previous: Vec::new(),
            previous_rect: None,
            pending: None,
        })
    }

    /// Reads source frames until one presentation has been composited, then
    /// returns it.
    fn next_presentation(
        &mut self,
        reader: &mut Reader<BufReader<File>>,
        buffer: &mut [u8],
    ) -> Result<DecodedImage> {
        loop {
            reader
                .next_frame(buffer)
                .map_err(|error| crate::decoder::image_error("decode", &self.path, error))?;
            let Some(control) = reader.info().frame_control().copied() else {
                // The poster: shown only by a reader that does not know APNG,
                // so it is neither composited nor handed out.
                continue;
            };
            let rect = self.rect_of(&control)?;
            self.dispose_pending();
            self.remember_canvas_for(&control, rect);
            self.mix(buffer, rect, &control)?;
            self.pending = Some((rect, control.dispose_op));
            return Ok(self.decoded());
        }
    }

    /// Applies the disposal the previous presentation asked for.
    ///
    /// Disposal describes what happens to a frame once it has been shown, so it
    /// happens at the start of the frame that follows rather than at the end of
    /// the one that asked for it.
    fn dispose_pending(&mut self) {
        let Some((rect, dispose)) = self.pending.take() else {
            return;
        };
        match dispose {
            DisposeOp::None => {}
            DisposeOp::Background => {
                for row in rect.y..rect.y.saturating_add(rect.height) {
                    let start = self.offset_of(row, rect.x);
                    let end = start.saturating_add(rect.width as usize * self.pixel_bytes());
                    if let Some(slice) = self.canvas.get_mut(start..end) {
                        slice.fill(0);
                    }
                }
            }
            DisposeOp::Previous => {
                let Some(rect) = self.previous_rect else {
                    return;
                };
                if self.previous.len() != self.canvas.len() {
                    return;
                }
                let bytes = self.pixel_bytes();
                for row in rect.y..rect.y.saturating_add(rect.height) {
                    let start = self.offset_of(row, rect.x);
                    let end = start.saturating_add(rect.width as usize * bytes);
                    // The saved copy and the canvas are separate buffers, so
                    // they can be borrowed at the same time.
                    let source = self.previous.get(start..end);
                    let target = self.canvas.get_mut(start..end);
                    if let (Some(source), Some(target)) = (source, target) {
                        target.copy_from_slice(source);
                    }
                }
            }
        }
    }

    /// Saves the region a `Previous` disposal will need, before it is changed.
    ///
    /// The specification saves the whole canvas, but only the rectangle the
    /// frame touches can have changed, so that is what is kept.
    fn remember_canvas_for(&mut self, control: &png::FrameControl, rect: Rect) {
        if control.dispose_op != DisposeOp::Previous {
            self.previous_rect = None;
            return;
        }
        if self.previous.len() != self.canvas.len() {
            self.previous = self.canvas.clone();
        } else {
            self.previous.copy_from_slice(&self.canvas);
        }
        self.previous_rect = Some(rect);
    }

    /// Places one decoded subframe onto the canvas under its blend operation.
    fn mix(&mut self, buffer: &[u8], rect: Rect, control: &png::FrameControl) -> Result<()> {
        let pixel = self.pixel_bytes();
        let source_stride = rect.width as usize * pixel;
        let alpha_at = alpha_offset(self.source_color_type, self.pixels_of());
        // Read out of `self` before the canvas is borrowed for writing.
        let maximum = self.maximum();
        let sixteen_bit = self.sixteen_bit;

        for row in 0..rect.height {
            let source_start = row as usize * source_stride;
            let source_end = source_start.saturating_add(source_stride);
            let Some(source) = buffer.get(source_start..source_end) else {
                return Err(ImgSeqError::new(format!(
                    "animated image '{}' states a {}x{} frame outside its own data",
                    self.path.display(),
                    rect.width,
                    rect.height
                )));
            };
            let target_start = self.offset_of(rect.y + row, rect.x);
            for column in 0..rect.width as usize {
                let source_pixel = &source[column * pixel..(column + 1) * pixel];
                let target_at = target_start + column * pixel;
                let Some(target) = self.canvas.get_mut(target_at..target_at + pixel) else {
                    return Err(ImgSeqError::new(format!(
                        "animated image '{}' states a frame outside its canvas",
                        self.path.display()
                    )));
                };
                if control.blend_op == BlendOp::Over
                    && let Some(alpha_offset) = alpha_at
                {
                    let alpha = if sixteen_bit {
                        u32::from(u16::from_be_bytes([
                            source_pixel[alpha_offset],
                            source_pixel[alpha_offset + 1],
                        ]))
                    } else {
                        u32::from(source_pixel[alpha_offset])
                    };
                    if alpha == 0 {
                        continue;
                    }
                    if alpha < maximum {
                        blend(target, source_pixel, alpha, maximum, sixteen_bit);
                        continue;
                    }
                }
                target.copy_from_slice(source_pixel);
            }
        }
        Ok(())
    }

    /// Converts the canvas into the buffer the frame writer reads.
    ///
    /// A 16-bit png stores its samples big-endian and the frame is native, so
    /// the bytes are swapped on the way out. The canvas itself stays in the
    /// file's order because that is what the next subframe will be mixed into.
    fn decoded(&self) -> DecodedImage {
        let buffer = if self.sixteen_bit {
            self.canvas
                .chunks_exact(2)
                .flat_map(|sample| [sample[1], sample[0]])
                .collect()
        } else {
            self.canvas.clone()
        };
        let color_type = if self.sixteen_bit {
            match self.color_type {
                ColorType::L8 => ColorType::L16,
                ColorType::La8 => ColorType::La16,
                ColorType::Rgb8 => ColorType::Rgb16,
                _ => ColorType::Rgba16,
            }
        } else {
            self.color_type
        };
        DecodedImage {
            width: self.width,
            height: self.height,
            format: self.format,
            transform: self.transform,
            pixels: Pixels::Interleaved { color_type, buffer },
            timings: DecodeTimings {
                open: std::time::Duration::ZERO,
                metadata: std::time::Duration::ZERO,
                buffer: std::time::Duration::ZERO,
                read: std::time::Duration::ZERO,
            },
        }
    }

    fn maximum(&self) -> u32 {
        if self.sixteen_bit { 0xFFFF } else { 0xFF }
    }

    fn pixels_of(&self) -> usize {
        channels(self.source_color_type)
    }

    fn offset_of(&self, row: u32, column: u32) -> usize {
        (row as usize * self.width as usize + column as usize) * self.pixel_bytes()
    }

    /// Bytes one pixel of the file's colour type occupies.
    fn pixel_bytes(&self) -> usize {
        self.pixels_of() * if self.sixteen_bit { 2 } else { 1 }
    }

    fn rect_of(&self, control: &png::FrameControl) -> Result<Rect> {
        let right = control.x_offset.checked_add(control.width);
        let bottom = control.y_offset.checked_add(control.height);
        let inside = right.is_some_and(|right| right <= self.width)
            && bottom.is_some_and(|bottom| bottom <= self.height);
        if !inside {
            return Err(ImgSeqError::new(format!(
                "animated image '{}' places a {}x{} frame at {},{} outside its {}x{} canvas",
                self.path.display(),
                control.width,
                control.height,
                control.x_offset,
                control.y_offset,
                self.width,
                self.height
            )));
        }
        Ok(Rect {
            x: control.x_offset,
            y: control.y_offset,
            width: control.width,
            height: control.height,
        })
    }
}

/// The canvas a frame with no alpha starts from: opaque white, as the
/// specification says, or transparent black when the file has an alpha channel.
fn background(width: u32, height: u32, color_type: ColorType, sixteen_bit: bool) -> Vec<u8> {
    let pixel = channels_of(color_type) * if sixteen_bit { 2 } else { 1 };
    let mut canvas = vec![0u8; width as usize * height as usize * pixel];
    if !has_alpha(color_type) {
        canvas.fill(0xFF);
    }
    canvas
}

/// The `image` colour type a `png` colour type names.
///
/// Indexed does not appear in an animated png: an APNG frame is the same colour
/// type as the file, and the specification does not allow a palette animation,
/// so it is refused rather than expanded.
const fn color_of(color_type: png::ColorType) -> Option<ColorType> {
    match color_type {
        png::ColorType::Grayscale => Some(ColorType::L8),
        png::ColorType::GrayscaleAlpha => Some(ColorType::La8),
        png::ColorType::Rgb => Some(ColorType::Rgb8),
        png::ColorType::Rgba => Some(ColorType::Rgba8),
        png::ColorType::Indexed => None,
    }
}

/// Mixes one pixel of `source` over `target` with the coverage `alpha`.
///
/// The arithmetic is the specification's: the result is the source scaled by
/// its coverage plus the destination scaled by what is left, rounded down. A
/// channel order that differs between the two buffers is not possible here,
/// because both are the file's own colour type.
fn blend(target: &mut [u8], source: &[u8], alpha: u32, maximum: u32, sixteen_bit: bool) {
    let inverse = maximum - alpha;
    if sixteen_bit {
        for (target, source) in target.chunks_exact_mut(2).zip(source.chunks_exact(2)) {
            let destination = u32::from(u16::from_be_bytes([target[0], target[1]]));
            let value = u32::from(u16::from_be_bytes([source[0], source[1]]));
            let mixed = (value * alpha + destination * inverse) / maximum;
            let bytes = (mixed as u16).to_be_bytes();
            target.copy_from_slice(&bytes);
        }
    } else {
        for (target, &source) in target.iter_mut().zip(source) {
            let mixed = (u32::from(source) * alpha + u32::from(*target) * inverse) / maximum;
            *target = mixed as u8;
        }
    }
}

const fn has_alpha(color_type: ColorType) -> bool {
    matches!(color_type, ColorType::La8 | ColorType::Rgba8)
}

/// Byte offset of the alpha sample within one pixel, when the colour type has
/// one.
const fn alpha_offset(color_type: png::ColorType, pixels: usize) -> Option<usize> {
    match color_type {
        png::ColorType::GrayscaleAlpha => Some(1),
        png::ColorType::Rgba => {
            if pixels == 4 {
                Some(3)
            } else {
                None
            }
        }
        _ => None,
    }
}

const fn channels_of(color_type: ColorType) -> usize {
    match color_type {
        ColorType::L8 | ColorType::La8 => {
            if matches!(color_type, ColorType::La8) {
                2
            } else {
                1
            }
        }
        ColorType::Rgb8 => 3,
        ColorType::Rgba8 => 4,
        _ => 1,
    }
}

/// Channels in one pixel of the file's own colour type.
const fn channels(color_type: png::ColorType) -> usize {
    match color_type {
        png::ColorType::Grayscale | png::ColorType::Indexed => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
    }
}

/// One pass over the file, which is what the shared source keeps per request.
pub struct PngDecoder {
    source: PngSource,
    reader: Reader<BufReader<File>>,
    buffer: Vec<u8>,
    /// The next presentation this pass will produce.
    next: usize,
}

impl PngDecoder {
    fn open(
        path: &Path,
        canvas: (u32, u32),
        transform: crate::pixel::Transform,
        format: crate::pixel::PixelFormat,
    ) -> Result<Self> {
        let reader = open(path)?;
        let buffer = vec![0u8; canvas_bytes(&reader, path)?];
        Ok(Self {
            source: PngSource::new(path, canvas, transform, format)?,
            reader,
            buffer,
            next: 0,
        })
    }

    /// Restarts the pass, which is what a backward request needs on a stream
    /// with no index.
    fn restart(&mut self) -> Result<()> {
        self.reader = open(&self.source.path)?;
        self.buffer = vec![0u8; canvas_bytes(&self.reader, &self.source.path)?];
        self.source = PngSource::new(
            &self.source.path,
            (self.source.width, self.source.height),
            self.source.transform,
            self.source.format,
        )?;
        self.next = 0;
        Ok(())
    }
}

impl AnimationDecoder for PngDecoder {
    fn seek(&mut self, index: usize) -> Result<()> {
        if index < self.next {
            self.restart()?;
        }
        Ok(())
    }

    fn next_presentation(&mut self) -> Result<DecodedImage> {
        let image = self
            .source
            .next_presentation(&mut self.reader, &mut self.buffer)?;
        self.next += 1;
        Ok(image)
    }
}
