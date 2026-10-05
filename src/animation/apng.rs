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
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use crate::layout::ColorType;
use png::{BitDepth, BlendOp, Decoder, DisposeOp, Reader};

use crate::{
    decoder::{DecodeTimings, DecodedImage, Pixels, image_error},
    error::{ImgSeqError, Result},
};

use super::{AnimationDecoder, AnimationSource, Presentation, Rate, SegmentInfo};

/// Bytes of the png signature, which the first chunk follows.
const SIGNATURE: u64 = 8;

/// Bytes of a chunk header: a length, which counts the payload alone, and a
/// kind.
const CHUNK_HEADER: u64 = 8;

/// Bytes of the checksum that follows every chunk payload.
const CHUNK_CRC: u64 = 4;

/// Bytes of an `fcTL` payload: a sequence number, the rectangle, the delay and
/// the two operation bytes.
const FRAME_CONTROL: u64 = 26;

/// Largest timebase a segment is placed on, in ticks a second. A file asking
/// for more than this is placed on milliseconds instead; see [`common_timebase`].
const TIMEBASE_LIMIT: u32 = 1 << 31;

#[cfg(test)]
thread_local! {
    /// Frames this thread has rendered, which is how a description is checked
    /// not to render the timeline it describes.
    static FRAMES_DECODED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Frames rendered on this thread.
#[cfg(test)]
pub(crate) fn frames_decoded() -> usize {
    FRAMES_DECODED.with(std::cell::Cell::get)
}

/// Starts [`frames_decoded`] from zero.
#[cfg(test)]
pub(crate) fn reset_frames_decoded() {
    FRAMES_DECODED.with(|count| count.set(0));
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
    file: &mut BufReader<File>,
) -> Result<Option<SegmentInfo>> {
    // The `png` crate's reader borrows the probe's open, so what it states is
    // copied out of it before the chunk walk below wants the same handle.
    let (frames, canvas) = {
        let reader = open_over(file, path)?;
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
        (animation.num_frames as usize, reader.info().size())
    };

    let (rate, presentations) = timing(file, path, frames)?;
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

/// The delays a file's frames state, in the order they are written.
///
/// A frame's delay is its `fcTL` chunk, and the `png` crate hands that chunk
/// out only from a reader that has just decoded the frame. The chunks are a
/// list of headers, so this walks them and reads the `fcTL` payloads alone: a
/// description costs twenty-six bytes a frame rather than a decoded canvas
/// each.
///
/// `expected` is the frame count the file's own animation control chunk states.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or states fewer frames
/// than that chunk counted.
fn timing(
    file: &mut BufReader<File>,
    path: &Path,
    expected: usize,
) -> Result<(Rate, Vec<Presentation>)> {
    let length = file
        .seek(SeekFrom::End(0))
        .map_err(|error| image_error("read", path, error))?;
    let mut delays: Vec<(u16, u16)> = Vec::with_capacity(expected);
    // The chunks follow the signature, each a length, a kind, its payload and a
    // checksum this does not check: the walk needs the two fields that place it
    // at the next one, and the delay of the frames it passes.
    let mut at = SIGNATURE;
    while delays.len() < expected {
        if at + CHUNK_HEADER > length {
            break;
        }
        file.seek(SeekFrom::Start(at))
            .map_err(|error| image_error("read", path, error))?;
        let mut header = [0u8; CHUNK_HEADER as usize];
        file.read_exact(&mut header)
            .map_err(|error| image_error("read", path, error))?;
        let size = u64::from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]));
        let kind = [header[4], header[5], header[6], header[7]];
        let payload = at + CHUNK_HEADER;
        let Some(next) = payload
            .checked_add(size)
            .and_then(|end| end.checked_add(CHUNK_CRC))
        else {
            break;
        };
        if next > length {
            break;
        }
        if &kind == b"fcTL" {
            // A payload too short to hold a delay is a frame the file does not
            // finish stating, which ends the timeline the way a missing frame
            // does.
            if size < FRAME_CONTROL {
                break;
            }
            let mut control = [0u8; FRAME_CONTROL as usize];
            file.read_exact(&mut control)
                .map_err(|error| image_error("read", path, error))?;
            let numerator = u16::from_be_bytes([control[20], control[21]]);
            let denominator = u16::from_be_bytes([control[22], control[23]]);
            delays.push((numerator, denominator));
        }
        if &kind == b"IEND" {
            break;
        }
        at = next;
    }
    if delays.len() < expected {
        return Err(ImgSeqError::new(format!(
            "animated image '{}' ended after {} of its {expected} frames",
            path.display(),
            delays.len()
        )));
    }
    timeline(&delays, path)
}

/// Places frames whose delays are the fractions `delays` states on a timeline.
///
/// A delay is a fraction of a second and a segment has one rate, so the
/// fractions go on the lowest common denominator of the ones a file states:
/// every delay is then a whole number of ticks and none of them is rounded
/// before it is accumulated, which is what keeps a long timeline from drifting.
/// An APNG may state any denominator, not only the hundredth the specification
/// defaults to.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the timeline's ticks overflow.
fn timeline(delays: &[(u16, u16)], path: &Path) -> Result<(Rate, Vec<Presentation>)> {
    let timebase = common_timebase(delays);
    let mut presentations = Vec::with_capacity(delays.len());
    let mut timestamp = 0i64;
    for (numerator, denominator) in delays {
        let denominator = denominator_of(*denominator);
        let duration = match timebase {
            // Exact: the timebase is a multiple of every denominator here.
            Some(timebase) => i64::from(*numerator) * i64::from(timebase / u32::from(denominator)),
            // A millisecond a tick, which is where every delay was placed
            // before there was a timebase to work out.
            None => delay_ms(*numerator, denominator),
        };
        presentations.push(Presentation {
            timestamp,
            duration: Some(duration),
        });
        timestamp = timestamp.checked_add(duration).ok_or_else(|| {
            ImgSeqError::new(format!("the timeline of '{}' overflows", path.display()))
        })?;
    }
    let ticks = timebase.unwrap_or(1000);
    Ok((Rate::new(i64::from(ticks), 1), presentations))
}

/// The lowest common denominator of `delays`, when one that holds them all is
/// worth using.
///
/// A file stating several coprime denominators asks for a timebase as large as
/// their product, which is billions of ticks a second for a handful of frames.
/// `None` means the caller places the delays on milliseconds instead, which is
/// what it did for every file before this.
fn common_timebase(delays: &[(u16, u16)]) -> Option<u32> {
    let mut timebase = 1u32;
    for (_, denominator) in delays {
        let denominator = u32::from(denominator_of(*denominator));
        let divisor = gcd(timebase, denominator);
        timebase = timebase.checked_div(divisor)?.checked_mul(denominator)?;
        if timebase > TIMEBASE_LIMIT {
            return None;
        }
    }
    Some(timebase)
}

/// Greatest common divisor, by Euclid.
fn gcd(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn denominator_of(delay_den: u16) -> u16 {
    if delay_den == 0 { 100 } else { delay_den }
}

/// A delay of `num/den` seconds in milliseconds.
fn delay_ms(num: u16, denominator: u16) -> i64 {
    (i64::from(num) * 1000) / i64::from(denominator)
}

/// Opens the file for the decoder, which replays it from its beginning.
///
/// A stream with no index keeps this reader for the life of the source rather than
/// for the length of a description, so it opens for itself.
fn open(path: &Path) -> Result<Reader<BufReader<File>>> {
    let file = File::open(path).map_err(|error| image_error("open", path, error))?;
    read_info(BufReader::new(file), path)
}

/// The same reader over an open the caller already holds, which is what the probe
/// hands this adapter while it looks for a timeline.
fn open_over<'a>(
    file: &'a mut BufReader<File>,
    path: &Path,
) -> Result<Reader<&'a mut BufReader<File>>> {
    read_info(file, path)
}

/// Reads what the file states, over any reader.
///
/// `Decoder::new` keeps the default `Transformations::IDENTITY`, which is what
/// hands back the file's own colour type and bit depth. That is the whole reason
/// this reader is here: every other transform changes one or both, and any of them
/// would narrow a 16-bit file.
fn read_info<R: BufRead + Seek>(file: R, path: &Path) -> Result<Reader<R>> {
    Decoder::new(file)
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
            #[cfg(test)]
            FRAMES_DECODED.with(|count| count.set(count.get() + 1));
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
                .as_chunks::<2>()
                .0
                .iter()
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
        let (targets, _) = target.as_chunks_mut::<2>();
        let (sources, _) = source.as_chunks::<2>();
        for (target, source) in targets.iter_mut().zip(sources) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A delay a millisecond timebase cannot hold is placed exactly: the rate is
    /// the lowest common denominator of the delays the file states.
    #[test]
    fn a_delay_is_placed_on_a_timebase_that_holds_it() {
        let (rate, presentations) =
            timeline(&[(1, 3), (1, 3), (1, 3)], Path::new("a.png")).expect("a timeline");
        assert_eq!(rate, Rate::new(3, 1), "a third of a second a tick");
        assert_eq!(presentations[0].duration, Some(1));
        assert_eq!(
            presentations[2].timestamp, 2,
            "nothing was rounded before it was added"
        );

        // A thousandth and a third share three thousand ticks a second, where
        // the third was placed as 333 ms before.
        let (rate, presentations) =
            timeline(&[(1, 1000), (1, 3)], Path::new("a.png")).expect("a timeline");
        assert_eq!(rate, Rate::new(3000, 1));
        assert_eq!(presentations[0].duration, Some(3));
        assert_eq!(presentations[1].timestamp, 3);
    }

    /// Denominators no timebase worth using can hold are placed on milliseconds,
    /// which is where every delay was placed before.
    #[test]
    fn denominators_no_timebase_holds_are_milliseconds() {
        // Two large coprime denominators ask for four billion ticks a second
        // between them.
        let (rate, presentations) =
            timeline(&[(1, 65521), (1, 65519)], Path::new("a.png")).expect("a timeline");
        assert_eq!(rate, Rate::new(1000, 1));
        assert_eq!(presentations[0].duration, Some(0));
        assert_eq!(presentations[0].timestamp, 0);
    }
}
