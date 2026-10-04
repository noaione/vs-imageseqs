//! The RIFF walk that reads an animated webp's timeline.
//!
//! A webp is a RIFF container. An animated one carries a `VP8X` header that
//! states the canvas and an `ANIM` chunk with the loop count, then one `ANMF`
//! chunk per displayed frame. Each `ANMF` states a sub-rectangle of the canvas,
//! how long it is shown, and how it is drawn onto what is already there; inside
//! it is the `ALPH` and `VP8`/`VP8L` payload of that one frame.
//!
//! This module reads all of that and nothing else. It is a walk rather than a
//! decode: no pixel is touched, so a timeline costs two passes over a few
//! hundred bytes of headers. That is the whole reason the container is read here
//! rather than through a library -- libwebp's `WebPAnimDecoder` composes every
//! canvas into a buffer it owns before handing one over, which is both the
//! discovery decode this migration exists to remove and the wrong shape for a
//! reader that wants one sub-rectangle at a time. See
//! `docs/improvements/28-animation-container-decoders.md`.
//!
//! Two independent cross-checks read the same fields and both were run against
//! the fixture: `webpinfo.exe`, which is libwebp's own tool, and the research
//! walker under `target/cand-anim/webp-container.py`. Where my first walker
//! disagreed with them it was because the `ANMF` flags byte has the blend method
//! in bit 1 and the disposal in bit 0, which [`Frame::of`] now says explicitly.

use std::path::{Path, PathBuf};

use crate::{
    decoder::{DecodeTimings, DecodedImage, Pixels, image_error},
    error::{ImgSeqError, Result},
    layout::ColorType,
    pixel::PixelFormat,
};

use super::{AnimationDecoder, AnimationSource, Presentation, Rate, SegmentInfo};
/// File extensions that hold a webp.
const EXTENSIONS: [&str; 1] = ["webp"];

/// Bytes of a RIFF chunk header: the four-character code and the size.
const CHUNK_HEADER: usize = 8;

/// Bytes of an `ANMF` header before its sub-chunks.
const ANMF_HEADER: usize = 16;

/// Largest canvas this walk will describe, which bounds an allocation made from
/// a field of a file. libwebp's own limit is the same order.
const MAX_SIDE: u32 = 16_384;

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// Whether a webp bitstream holds an animation rather than one picture.
///
/// The still decoder needs this because libwebp's simple entry points read one
/// image and refuse a container of them, and a file whose name does not say
/// `webp` never reaches [`segment_info`]: it is a still to the probe, which
/// picks this adapter by name, so the still decoder has to recognise the
/// container it cannot read and hand it on.
#[must_use]
pub fn is_animated(data: &[u8], path: &Path) -> bool {
    parse(data, path).is_ok_and(|parsed| parsed.is_some_and(|shown| shown.is_animated()))
}

/// One displayed frame's rectangle, timing and drawing rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    /// Left edge of the rectangle on the canvas.
    pub x: u32,
    /// Top edge of the rectangle on the canvas.
    pub y: u32,
    /// Width of the rectangle.
    pub width: u32,
    /// Height of the rectangle.
    pub height: u32,
    /// How long it is shown, in milliseconds.
    pub duration_ms: u32,
    /// Whether the rectangle is blended onto the canvas rather than written
    /// over it. This is the `ANMF` flags bit 1 read as its *meaning*, so a frame
    /// that says "do not blend" answers `false`.
    pub blend: bool,
    /// Whether the rectangle is disposed of after being shown. See
    /// [`Animation::dispose_is_inert`].
    pub dispose: bool,
    /// Where this frame's `ALPH`/`VP8`/`VP8L` payload starts and ends in the
    /// file, so a decode can be handed just that frame.
    pub payload: std::ops::Range<usize>,
}

/// What the container states about an animated webp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Animation {
    /// Canvas width.
    pub width: u32,
    /// Canvas height.
    pub height: u32,
    /// The `ANIM` loop count, where zero means forever.
    pub loop_count: u16,
    /// Whether the `VP8X` header flags an alpha channel.
    pub has_alpha: bool,
    /// Whether the `VP8X` header flags an embedded ICC profile.
    pub has_icc_profile: bool,
    /// Whether the `VP8X` header flags an exif payload.
    pub has_exif: bool,
    /// The `ANIM` background colour hint, as it is written.
    pub background: [u8; 4],
    /// The displayed frames, in timeline order.
    pub frames: Vec<Frame>,
    /// Whether the disposal flag can have any effect on this reader.
    ///
    /// It cannot, and that is not an omission. A disposal clears its rectangle
    /// to the animation's *background*, and the reader this replaces never sets
    /// one: `image-webp` keeps the `ANIM` background as a `hint` and leaves the
    /// colour it would clear to as `None` unless a caller sets it, and `image`
    /// does not. The flag is therefore read, reported and otherwise inert, which
    /// is what keeps this reader's pixels identical to the one it replaces.
    pub dispose_is_inert: bool,
}

impl Animation {
    /// Whether the container holds more than one displayed picture.
    #[must_use]
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }
}

/// A canvas that a webp's frames are drawn onto.
///
/// One buffer persists between frames and it is the canvas itself: unlike a
/// gif, a webp frame is drawn *onto* what is already there rather than starting
/// from an undisposed copy, so the two containers need different state even
/// though both compose sub-rectangles.
///
/// It starts transparent, which is what the reader being replaced does: the
/// `ANIM` background is a hint that nothing sets, so the canvas is zeros.
struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        let (width, height) = (width as usize, height as usize);
        Self {
            width,
            height,
            pixels: vec![0; width.saturating_mul(height).saturating_mul(4)],
        }
    }

    /// Draws one decoded rectangle at `(x, y)` and answers the canvas.
    ///
    /// `blend` is the frame's own rule: a blending frame is combined with what
    /// is under it, and a non-blending one is written over it. A rectangle that
    /// runs past the canvas is clipped rather than refused, which is what the
    /// reader being replaced does and what keeps a slightly oversized frame
    /// readable.
    fn draw(
        &mut self,
        x: u32,
        y: u32,
        frame: &[u8],
        width: u32,
        height: u32,
        blend: bool,
    ) -> Vec<u8> {
        let frame_width = width as usize;
        let columns = frame_width.min(self.width.saturating_sub(x as usize));
        let rows = (height as usize).min(self.height.saturating_sub(y as usize));
        for row in 0..rows {
            for column in 0..columns {
                let source = (row * frame_width + column) * 4;
                let target = ((y as usize + row) * self.width + (x as usize + column)) * 4;
                let sample: [u8; 4] = frame[source..source + 4].try_into().expect("four samples");
                if blend {
                    let under: [u8; 4] = self.pixels[target..target + 4]
                        .try_into()
                        .expect("four samples");
                    self.pixels[target..target + 4].copy_from_slice(&over(sample, under));
                } else {
                    self.pixels[target..target + 4].copy_from_slice(&sample);
                }
            }
        }
        self.pixels.clone()
    }
}

/// One sample of `source` drawn over one sample of `under`, both straight
/// (not premultiplied) alpha.
///
/// This is a port of libwebp's own integer routine, which is what the reader
/// being replaced uses, and it is not the obvious `(dst * (255 - src_a)) / 255`:
/// the partial factor is divided by 255 *rounding to nearest*, and the colour
/// channels are renormalised by `(1 << 24) / blended_alpha` so that a partly
/// transparent result keeps its colour. Both details change the output by a
/// count or two on the fixtures, which is exactly the size of error that would
/// pass a loose comparison and fail a byte-for-byte one.
///
/// The source came from `image-webp`'s `alpha_blending.rs`, which itself cites
/// libwebp's `src/demux/anim_decode.c`.
fn over(source: [u8; 4], under: [u8; 4]) -> [u8; 4] {
    let source_alpha = source[3];
    if source_alpha == 0 {
        // Nothing of the source shows, so the sample under it stands, alpha
        // included.
        return under;
    }
    let under_alpha = under[3];
    // The part of the destination that survives is its alpha scaled by how
    // much of the source is missing, rounded to nearest.
    let keep = div_by_255(u32::from(under_alpha) * (255 - u32::from(source_alpha)));
    let blended_alpha = u32::from(source_alpha) + keep;
    if blended_alpha == 0 {
        return [0, 0, 0, 0];
    }
    // Renormalises each channel back onto the blended alpha. The shift is 24
    // because the scale is a 24-bit reciprocal.
    let scale = (1u32 << 24) / blended_alpha;
    let mut out = [0u8; 4];
    for channel in 0..3 {
        let blended =
            u32::from(source[channel]) * u32::from(source_alpha) + u32::from(under[channel]) * keep;
        out[channel] = ((blended * scale) >> 24) as u8;
    }
    out[3] = blended_alpha as u8;
    out
}

/// `value / 255`, rounding to nearest rather than down.
///
/// Integer division truncates, which would leave every blended sample one low
/// often enough to be visible in a byte-for-byte comparison.
const fn div_by_255(value: u32) -> u32 {
    (((value + 0x80) >> 8) + value + 0x80) >> 8
}

/// The smallest webp container libwebp will decode one frame out of.
///
/// An `ANMF` frame's payload is the frame's own `ALPH` and `VP8`/`VP8L`
/// chunks with no RIFF wrapper, because the wrapper belongs to the whole
/// animation. libwebp's still decoder wants a file, so a frame that carries
/// alpha gets one built around it: a `VP8X` header stating the alpha flag and
/// the rectangle's size, then the payload's chunks as they already are.
///
/// A frame with no `ALPH` needs no wrapper at all. A `VP8` or `VP8L` bitstream
/// is what the decoder's simple entry points accept directly, and a `VP8L`
/// carries its own alpha in-band, so wrapping it would only add a header that
/// says nothing.
fn container(payload: &[u8], width: u32, height: u32) -> Vec<u8> {
    if !payload.starts_with(b"ALPH") {
        return payload.to_vec();
    }
    // The canvas is stated one less than its size, exactly as the animation's
    // own header states it.
    let mut header = vec![0x10u8, 0, 0, 0];
    header.extend_from_slice(&width.saturating_sub(1).to_le_bytes()[..3]);
    header.extend_from_slice(&height.saturating_sub(1).to_le_bytes()[..3]);

    let mut body = Vec::with_capacity(payload.len() + 32);
    push_chunk(&mut body, b"VP8X", &header);
    // The payload's chunks already carry their own headers and padding.
    body.extend_from_slice(payload);

    let mut file = Vec::with_capacity(body.len() + 12);
    file.extend_from_slice(b"RIFF");
    // The RIFF size counts the four bytes of `WEBP` and the body.
    file.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    file.extend_from_slice(b"WEBP");
    file.extend_from_slice(&body);
    file
}

/// Appends one RIFF chunk, padded to an even length.
fn push_chunk(out: &mut Vec<u8>, code: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(code);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
}

/// Whether one frame's payload carries an alpha channel of its own.
///
/// A `VP8L` bitstream is treated as carrying one whatever it holds, because
/// that is what the reader being replaced does: it decides the frame's alpha
/// from the chunk's *kind* rather than from its contents, and that decision is
/// what selects between blending and overwriting.
fn payload_has_alpha(payload: &[u8]) -> bool {
    payload.starts_with(b"ALPH") || payload.starts_with(b"VP8L")
}

/// The rate a webp's delays are counted in: microseconds, from milliseconds.
const RATE: Rate = Rate::new(1_000_000, 1);

/// Describes an animated webp's timeline without holding its pictures.
///
/// Returns `None` for a webp that displays one picture, which leaves it on the
/// still path it was on before.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the container cannot be read.
pub fn segment_info(
    path: &Path,
    info: crate::decoder::ImageInfo,
    fps: Rate,
) -> Result<Option<SegmentInfo>> {
    let Some(animation) = walk(path)? else {
        return Ok(None);
    };
    if !animation.is_animated() {
        return Ok(None);
    }
    let mut presentations = Vec::with_capacity(animation.frames.len());
    let mut timestamp = 0i64;
    for frame in &animation.frames {
        // A millisecond is a thousand microseconds, and the rate counts
        // microseconds.
        let duration = i64::from(frame.duration_ms) * 1_000;
        presentations.push(Presentation {
            timestamp,
            duration: Some(duration),
        });
        timestamp = timestamp.checked_add(duration).ok_or_else(|| {
            ImgSeqError::new(format!("the timeline of '{}' overflows", path.display()))
        })?;
    }

    let source = std::sync::Arc::new(AnimationSource::new(
        path.to_path_buf(),
        Box::new(Source::new(path, &animation, info.transform, info.format)),
    ));
    Ok(Some(SegmentInfo {
        info,
        rate: RATE,
        presentations,
        decoder: source,
        fps,
    }))
}

/// One pass over the container, which is what a source keeps per request.
struct Source {
    path: PathBuf,
    /// What the container states, kept so a decode needs no second walk.
    animation: Animation,
    transform: crate::pixel::Transform,
    format: PixelFormat,
    canvas: Canvas,
    next: usize,
}

impl Source {
    fn new(
        path: &Path,
        animation: &Animation,
        transform: crate::pixel::Transform,
        format: PixelFormat,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            canvas: Canvas::new(animation.width, animation.height),
            animation: animation.clone(),
            transform,
            format,
            next: 0,
        }
    }

    /// Starts the pass again, which is what a backward request needs.
    fn restart(&mut self) {
        self.canvas = Canvas::new(self.animation.width, self.animation.height);
        self.next = 0;
    }

    /// Replays presentation `index`, advancing the pass to it.
    fn presentation(&mut self, index: usize) -> Result<DecodedImage> {
        if index < self.next {
            self.restart();
        }
        // The bytes are read once for the whole pass rather than once per
        // frame: a frame's payload is addressed by offset into this file.
        let data =
            std::fs::read(&self.path).map_err(|error| image_error("open", &self.path, error))?;
        while self.next <= index {
            let Some(frame) = self.animation.frames.get(self.next) else {
                return Err(ImgSeqError::new(format!(
                    "animated image '{}' holds no presentation {index}",
                    self.path.display()
                )));
            };
            let payload = data.get(frame.payload.clone()).ok_or_else(|| {
                image_error(
                    "decode",
                    &self.path,
                    "a frame runs past the end of the file",
                )
            })?;
            let container = container(payload, frame.width, frame.height);
            let (width, height, pixels) = crate::formats::webp::decode_rgba(&container)
                .map_err(|error| image_error("decode", &self.path, error))?;
            if (width, height) != (frame.width, frame.height) {
                return Err(ImgSeqError::new(format!(
                    "animated image '{}' states a {}x{} frame that decodes as {width}x{height}",
                    self.path.display(),
                    frame.width,
                    frame.height
                )));
            }
            // Blending is only meaningful for a frame that carries alpha of
            // its own; a frame without it is written over the canvas, which is
            // also what keeps the lossy renormalisation out of a frame that
            // has no transparency to preserve.
            let blend = frame.blend && payload_has_alpha(payload);
            let composed = self
                .canvas
                .draw(frame.x, frame.y, &pixels, width, height, blend);
            self.next += 1;
            if self.next - 1 == index {
                return Ok(DecodedImage {
                    width: self.canvas.width as u32,
                    height: self.canvas.height as u32,
                    format: self.format,
                    transform: self.transform,
                    pixels: Pixels::Interleaved {
                        color_type: ColorType::Rgba8,
                        buffer: composed,
                    },
                    timings: DecodeTimings::default(),
                });
            }
        }
        unreachable!("the loop returns on the presentation it was asked for")
    }
}

impl AnimationDecoder for Source {
    fn seek(&mut self, index: usize) -> Result<()> {
        if index < self.next {
            self.restart();
        }
        Ok(())
    }

    fn next_presentation(&mut self) -> Result<DecodedImage> {
        let index = self.next;
        self.presentation(index)
    }
}

/// Reads `path`'s animation, or `None` for a file this module does not read.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or its container is
/// malformed.
pub fn walk(path: &Path) -> Result<Option<Animation>> {
    if !owns(path) {
        return Ok(None);
    }
    let data = std::fs::read(path).map_err(|error| {
        ImgSeqError::new(format!(
            "failed to open image '{}': {error}",
            path.display()
        ))
    })?;
    parse(&data, path)
}

/// Reads an animated webp out of its bytes.
/// Reads an animated webp out of its bytes, or `None` for a webp that is not
/// animated.
///
/// A plain webp has no `VP8X` header at all -- the extended format exists for
/// the features a still bitstream cannot state -- and one with a `VP8X` whose
/// animation flag is clear is a still picture that happens to carry a profile
/// or an alpha chunk. Both are declined here rather than refused, because both
/// are valid files that belong on the still path.
fn parse(data: &[u8], path: &Path) -> Result<Option<Animation>> {
    let bad = |what: &str| {
        ImgSeqError::new(format!(
            "failed to decode image '{}': the webp container {what}",
            path.display()
        ))
    };

    if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return Err(bad("is not a RIFF/WEBP file"));
    }
    // The RIFF size counts everything after the two size fields, so a file that
    // states fewer bytes than it holds is read only as far as it claims. A file
    // that claims more is truncated rather than read past its own end.
    let riff_size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let end = (riff_size + 8).min(data.len());
    if end < 12 {
        return Err(bad("states a size too small to hold a header"));
    }

    let mut header: Option<(u32, u32, bool, bool, bool)> = None;
    let mut loop_count = 0;
    let mut background = [0u8; 4];
    let mut animation = false;
    let mut frames = Vec::new();

    for (fourcc, payload) in chunks(data, 12, end) {
        match fourcc {
            b"VP8X" => {
                if payload.len() < 10 {
                    return Err(bad("holds a VP8X chunk that is too short"));
                }
                let flags = payload[0];
                animation = flags & 0x02 != 0;
                // The canvas is stated one less than its size, because zero is
                // not a valid dimension; see the spec's `Canvas Width Minus One`.
                let width = read_three(&payload[4..7]) + 1;
                let height = read_three(&payload[7..10]) + 1;
                if width > MAX_SIDE || height > MAX_SIDE {
                    return Err(bad("states a canvas larger than this reader accepts"));
                }
                header = Some((
                    width,
                    height,
                    flags & 0x10 != 0,
                    flags & 0x20 != 0,
                    flags & 0x08 != 0,
                ));
            }
            b"ANIM" => {
                if payload.len() < 6 {
                    return Err(bad("holds an ANIM chunk that is too short"));
                }
                background.copy_from_slice(&payload[..4]);
                loop_count = u16::from_le_bytes([payload[4], payload[5]]);
            }
            b"ANMF" => {
                let frame = Frame::of(data, payload, &bad)?;
                frames.push(frame);
            }
            _ => {}
        }
    }

    // No `VP8X`: a plain still bitstream, which is not this reader's.
    let Some((width, height, has_alpha, has_icc_profile, has_exif)) = header else {
        return Ok(None);
    };
    // An extended webp that states no animation is a still picture.
    if !animation {
        return Ok(None);
    }
    if frames.is_empty() {
        return Err(bad("states an animation but holds no frame"));
    }

    Ok(Some(Animation {
        width,
        height,
        loop_count,
        has_alpha,
        has_icc_profile,
        has_exif,
        background,
        frames,
        dispose_is_inert: true,
    }))
}
impl Frame {
    /// Reads one `ANMF` chunk's header, keeping the range of its payload.
    ///
    /// `payload` is the `ANMF` chunk's body; its first [`ANMF_HEADER`] bytes are
    /// the header and the rest is the frame's own `ALPH`/`VP8`/`VP8L` chunks.
    /// The range is recorded against the *whole file* rather than against the
    /// chunk, so a decoder can be handed a slice of the bytes that were read.
    fn of(data: &[u8], payload: &[u8], bad: &impl Fn(&str) -> ImgSeqError) -> Result<Self> {
        if payload.len() < ANMF_HEADER {
            return Err(bad("holds an ANMF chunk that is too short"));
        }
        // The rectangle is stated in units of two pixels, so each field is
        // doubled; the size is stated one less, like the canvas.
        let x = read_three(&payload[0..3]) * 2;
        let y = read_three(&payload[3..6]) * 2;
        let width = read_three(&payload[6..9]) + 1;
        let height = read_three(&payload[9..12]) + 1;
        let duration_ms = read_three(&payload[12..15]);
        let flags = payload[15];
        // Bit 1 is the blend method and bit 0 the disposal. Blend is stated as
        // "do not blend", so the flag and the meaning are opposites.
        let blend = flags & 0b10 == 0;
        let dispose = flags & 0b01 != 0;

        // The payload is where this chunk's body begins in the file plus its
        // header; the chunk's own address is found by subtracting the header
        // that was already stripped.
        let start =
            offset_of(payload, data).ok_or_else(|| bad("is not in the file it was read from"))?;
        let body = start + ANMF_HEADER;
        let finish = start + payload.len();
        if finish > data.len() {
            return Err(bad("holds a frame that runs past the end of the file"));
        }

        Ok(Self {
            x,
            y,
            width,
            height,
            duration_ms,
            blend,
            dispose,
            payload: body..finish,
        })
    }
}

/// Where `slice` starts within `data`, for a slice that is part of it.
///
/// The walk hands out subslices of the bytes it read, so the offset is a pointer
/// difference rather than a second bookkeeping pass. It answers `None` for a
/// slice that is not inside `data`, which is the check that keeps a
/// pointer-derived index from being trusted blindly.
fn offset_of(slice: &[u8], data: &[u8]) -> Option<usize> {
    let base = data.as_ptr() as usize;
    let start = slice.as_ptr() as usize;
    let offset = start.checked_sub(base)?;
    let end = offset.checked_add(slice.len())?;
    (end <= data.len()).then_some(offset)
}

/// Reads a three byte little endian field, which is how a webp states a size or
/// a duration.
fn read_three(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16)
}

/// Walks the RIFF chunks in `[start, end)`, answering each one's code and body.
///
/// A chunk is padded to an even length, so the next one starts after the padding
/// rather than immediately after the body. A chunk that claims to run past `end`
/// ends the walk rather than being read: a truncated file describes the frames
/// it actually holds.
fn chunks(data: &[u8], start: usize, end: usize) -> impl Iterator<Item = (&[u8], &[u8])> {
    let mut offset = start;
    std::iter::from_fn(move || {
        if offset + CHUNK_HEADER > end {
            return None;
        }
        let fourcc = &data[offset..offset + 4];
        let size = u32::from_le_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]) as usize;
        let body = offset + CHUNK_HEADER;
        let finish = body.checked_add(size)?;
        if finish > end {
            return None;
        }
        offset = finish + (size & 1);
        Some((fourcc, &data[body..finish]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    /// A RIFF header around `chunks`, each written as a code, a size and a body
    /// padded to an even length.
    fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (code, payload) in chunks {
            body.extend_from_slice(*code);
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(payload);
            if payload.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut file = Vec::from(*b"RIFF");
        file.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        file.extend_from_slice(b"WEBP");
        file.extend_from_slice(&body);
        file
    }

    /// A `VP8X` header body stating `width` by `height` and `flags`.
    fn vp8x(width: u32, height: u32, flags: u8) -> Vec<u8> {
        let mut body = vec![flags, 0, 0, 0];
        body.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        body.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        body
    }

    /// An `ANMF` body for a rectangle, with a `VP8L` payload of `payload`.
    fn anmf(x: u32, y: u32, width: u32, height: u32, duration: u32, flags: u8) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&(x / 2).to_le_bytes()[..3]);
        body.extend_from_slice(&(y / 2).to_le_bytes()[..3]);
        body.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        body.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        body.extend_from_slice(&duration.to_le_bytes()[..3]);
        body.push(flags);
        // A minimal inner chunk; the walk does not look inside it.
        body.extend_from_slice(b"VP8L");
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(&[1, 2, 3, 4]);
        body
    }

    /// The fixture's fields, which `webpinfo.exe` reports independently: a 16x12
    /// canvas, alpha, no ICC, no exif, and a background of all zeros.
    #[test]
    fn the_fixture_container_is_read_as_libwebp_reads_it() {
        let animation = walk(&fixture("animation.webp"))
            .expect("the fixture is read")
            .expect("a webp is taken over");
        assert_eq!((animation.width, animation.height), (16, 12));
        assert!(animation.has_alpha, "the fixture states alpha");
        assert!(!animation.has_icc_profile);
        assert!(!animation.has_exif);
        assert_eq!(animation.loop_count, 0, "zero is forever");
        assert_eq!(animation.background, [0, 0, 0, 0]);
        assert!(animation.is_animated());
        assert_eq!(animation.frames.len(), 4);
    }

    /// Every frame's rectangle, duration and flags, which `webpinfo.exe` prints
    /// for this same file: 16x12 for 80 ms, 11x10 for 170 ms, and so on, all at
    /// the origin with blend off and no disposal.
    #[test]
    fn every_frame_is_read_as_libwebp_reads_it() {
        let animation = walk(&fixture("animation.webp"))
            .expect("the fixture is read")
            .expect("a webp is taken over");
        let rectangles: Vec<_> = animation
            .frames
            .iter()
            .map(|f| {
                (
                    f.x,
                    f.y,
                    f.width,
                    f.height,
                    f.duration_ms,
                    f.blend,
                    f.dispose,
                )
            })
            .collect();
        assert_eq!(
            rectangles,
            vec![
                (0, 0, 16, 12, 80, false, false),
                (0, 0, 11, 10, 170, false, false),
                (0, 0, 14, 10, 110, false, false),
                (0, 0, 16, 10, 240, false, false),
            ]
        );
    }

    /// Each frame's payload range lies inside the file and starts with the code
    /// of the chunk that holds its pixels.
    #[test]
    fn every_frame_points_at_its_own_payload() {
        let data = std::fs::read(fixture("animation.webp")).expect("the fixture is read");
        let animation = parse(&data, Path::new("animation.webp"))
            .expect("the container parses")
            .expect("it is an animation");
        for (index, frame) in animation.frames.iter().enumerate() {
            let payload = &data[frame.payload.clone()];
            assert!(
                payload.starts_with(b"VP8L") || payload.starts_with(b"ALPH"),
                "frame {index} starts at {}",
                frame.payload.start
            );
        }
        // The ranges are ordered and do not overlap, which is what makes them
        // addresses rather than guesses.
        for pair in animation.frames.windows(2) {
            assert!(pair[0].payload.end <= pair[1].payload.start);
        }
    }

    /// The blend and disposal flags are read as their *meaning*, not as their
    /// bits: bit 1 says "do not blend", so it answers `blend: false`.
    #[test]
    fn the_blend_flag_is_read_as_its_meaning() {
        // Bit 1 set: do not blend. Bit 0 set: dispose.
        let file = riff(&[
            (b"VP8X", vp8x(4, 4, 0x02)),
            (b"ANIM", vec![0, 0, 0, 0, 0, 0]),
            (b"ANMF", anmf(0, 0, 4, 4, 10, 0b11)),
        ]);
        let animation = parse(&file, Path::new("x.webp"))
            .expect("the container parses")
            .expect("it is an animation");
        assert!(!animation.frames[0].blend, "bit 1 means do not blend");
        assert!(animation.frames[0].dispose, "bit 0 means dispose");

        let file = riff(&[
            (b"VP8X", vp8x(4, 4, 0x02)),
            (b"ANIM", vec![0, 0, 0, 0, 0, 0]),
            (b"ANMF", anmf(0, 0, 4, 4, 10, 0b00)),
        ]);
        let animation = parse(&file, Path::new("x.webp"))
            .expect("the container parses")
            .expect("it is an animation");
        assert!(animation.frames[0].blend, "bit 1 clear means blend");
        assert!(!animation.frames[0].dispose);
    }

    /// The rectangle's offset is stated in units of two pixels, so a frame at
    /// `(1, 2)` is written as `(0, 1)` and has to read back as `(2, 4)`.
    #[test]
    fn a_rectangle_offset_is_doubled() {
        let file = riff(&[
            (b"VP8X", vp8x(8, 8, 0x02)),
            (b"ANIM", vec![0, 0, 0, 0, 0, 0]),
            (b"ANMF", anmf(2, 4, 3, 3, 10, 0)),
        ]);
        let animation = parse(&file, Path::new("x.webp"))
            .expect("the container parses")
            .expect("it is an animation");
        assert_eq!((animation.frames[0].x, animation.frames[0].y), (2, 4));
        // The canvas and the rectangle are both stated one less than their size.
        assert_eq!((animation.width, animation.height), (8, 8));
        assert_eq!(
            (animation.frames[0].width, animation.frames[0].height),
            (3, 3)
        );
    }

    /// Only a webp extension is taken over.
    #[test]
    fn only_webp_extensions_are_taken_over() {
        for name in ["a.webp", "a.WEBP"] {
            assert!(owns(Path::new(name)), "{name}");
        }
        for name in ["a.png", "a.gif", "a.webpx", "a"] {
            assert!(!owns(Path::new(name)), "{name}");
        }
    }

    /// A container this walk cannot describe is refused rather than guessed at.
    #[test]
    fn a_malformed_container_is_refused() {
        let path = Path::new("x.webp");
        assert!(parse(b"", path).is_err());
        assert!(parse(b"NOTARIFFATALL", path).is_err());
        // A RIFF that is not a webp.
        assert!(parse(b"RIFF\x08\x00\x00\x00WAVEfmt ", path).is_err());
        // A VP8X that says animation but holds no frame.
        let file = riff(&[(b"VP8X", vp8x(4, 4, 0x02))]);
        assert!(parse(&file, path).is_err());
        // A canvas larger than this reader accepts.
        let file = riff(&[(b"VP8X", vp8x(20_000, 4, 0x02))]);
        assert!(parse(&file, path).is_err());
    }

    /// A webp that is not animated is *declined*, not refused, so that a still
    /// webp stays on the libwebp path it was already on. This is the case that
    /// a reader claiming every `.webp` gets wrong: `lossy.webp` has no `VP8X`
    /// at all, and erroring on it would break every lossy still in the tree.
    #[test]
    fn a_still_webp_is_declined_rather_than_refused() {
        let path = Path::new("x.webp");
        // A plain bitstream, which is what a lossy or lossless still is.
        let file = riff(&[(b"VP8 ", vec![0; 8])]);
        assert!(matches!(parse(&file, path), Ok(None)));
        let file = riff(&[(b"VP8L", vec![0; 8])]);
        assert!(matches!(parse(&file, path), Ok(None)));
        // An extended webp with the animation flag clear: a still that
        // carries a profile or an alpha chunk.
        let file = riff(&[(b"VP8X", vp8x(4, 4, 0x10)), (b"VP8 ", vec![0; 8])]);
        assert!(matches!(parse(&file, path), Ok(None)));
        // And the same file with the flag set is claimed.
        let file = riff(&[
            (b"VP8X", vp8x(4, 4, 0x12)),
            (b"ANIM", vec![0, 0, 0, 0, 0, 0]),
            (b"ANMF", anmf(0, 0, 4, 4, 10, 0)),
        ]);
        assert!(matches!(parse(&file, path), Ok(Some(_))));
    }

    /// A chunk that claims to run past the end of the file ends the walk rather
    /// than being read, so a truncated container describes what it holds.
    #[test]
    fn a_truncated_chunk_ends_the_walk() {
        let mut file = riff(&[
            (b"VP8X", vp8x(4, 4, 0x02)),
            (b"ANIM", vec![0, 0, 0, 0, 0, 0]),
            (b"ANMF", anmf(0, 0, 4, 4, 10, 0)),
        ]);
        // Find the `ANMF` header and claim one more byte than the file holds,
        // so the chunk cannot be read.
        let anmf_at = file
            .windows(4)
            .position(|window| window == b"ANMF")
            .expect("the frame chunk was written");
        let size = u32::from_le_bytes([
            file[anmf_at + 4],
            file[anmf_at + 5],
            file[anmf_at + 6],
            file[anmf_at + 7],
        ]);
        file[anmf_at + 4..anmf_at + 8].copy_from_slice(&(size + 1).to_le_bytes());
        // The frame is skipped rather than read past its own end, and a file
        // that states an animation and then holds no readable frame is refused
        // instead of being described as an animation of nothing.
        assert!(parse(&file, Path::new("x.webp")).is_err());
    }

    /// The blending routine is libwebp's, and these four are the cases where
    /// the obvious implementation differs from it.
    #[test]
    fn a_sample_is_blended_the_way_libwebp_blends_it() {
        // A source with no alpha leaves the canvas exactly as it was, alpha
        // included. A plain multiply would darken it.
        assert_eq!(
            over([10, 20, 30, 0], [200, 100, 50, 128]),
            [200, 100, 50, 128]
        );
        // An opaque source over anything: the alpha is the source's, and the
        // colours come back through the same renormalisation -- which is
        // *lossy*. 10 becomes 9, because `2550 * ((1 << 24) / 255) >> 24`
        // truncates. That is the routine being ported, not a mistake in it, and
        // it is why a byte-for-byte comparison is the only honest check.
        assert_eq!(
            over([10, 20, 30, 255], [200, 100, 50, 128]),
            [9, 19, 29, 255]
        );
        // Nothing underneath is the same arithmetic, not a shortcut: the
        // renormalisation applies even when there is nothing to blend with, so
        // a first frame drawn onto the transparent canvas is one low on every
        // channel too. That is why the reader being replaced cannot simply
        // copy an opaque sample, and why the fixture's own pixels are the
        // only reference that settles it.
        assert_eq!(over([10, 20, 30, 255], [0, 0, 0, 0]), [9, 19, 29, 255]);
        // Half over half. The destination's surviving alpha is
        // `div_by_255(128 * 127)` = 64, not 127: the factor is the destination's
        // alpha scaled by the source's *shortfall*, so two half-transparent
        // samples do not add up to opaque.
        assert_eq!(over([255, 0, 0, 128], [0, 0, 255, 128]), [169, 0, 84, 192]);
    }

    /// `div_by_255` rounds to nearest. Truncating division would leave the
    /// result one low for every value that is not an exact multiple, which is
    /// most of them.
    #[test]
    fn division_by_255_rounds_to_nearest() {
        assert_eq!(div_by_255(0), 0);
        assert_eq!(div_by_255(255), 1);
        assert_eq!(div_by_255(510), 2);
        // 127.5 rounds up, where `127 / 255` truncating would answer 0.
        assert_eq!(div_by_255(128), 1, "128/255 rounds to 1, not 0");
        assert_eq!(div_by_255(127), 0, "127/255 rounds to 0");
    }

    /// A frame drawn onto a transparent canvas is the frame, and a rectangle
    /// that is smaller than the canvas leaves the rest of it alone.
    #[test]
    fn a_rectangle_is_drawn_where_it_says_on_the_canvas() {
        let mut canvas = Canvas::new(3, 3);
        // Every sample of a two by two frame is the same opaque colour.
        let patch: Vec<u8> = [40u8, 50, 60, 255].repeat(4);
        let composed = canvas.draw(1, 1, &patch, 2, 2, false);
        let at = |pixels: &[u8], x: usize, y: usize| {
            let index = (y * 3 + x) * 4;
            [
                pixels[index],
                pixels[index + 1],
                pixels[index + 2],
                pixels[index + 3],
            ]
        };
        assert_eq!(at(&composed, 1, 1), [40, 50, 60, 255]);
        assert_eq!(at(&composed, 2, 2), [40, 50, 60, 255]);
        assert_eq!(at(&composed, 0, 0), [0, 0, 0, 0], "outside the rectangle");
    }

    /// A rectangle that runs past the canvas is clipped rather than refused or
    /// read past its end.
    #[test]
    fn a_rectangle_past_the_canvas_is_clipped() {
        let mut canvas = Canvas::new(2, 2);
        let patch: Vec<u8> = [1u8, 2, 3, 255].repeat(16);
        let composed = canvas.draw(1, 1, &patch, 4, 4, false);
        assert_eq!(composed.len(), 2 * 2 * 4, "the canvas did not grow");
        assert_eq!(&composed[12..16], &[1, 2, 3, 255], "the corner was drawn");
    }
}
