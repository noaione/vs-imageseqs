//! Animated gif: the timeline from the container, the canvas composed here.
//!
//! A gif is a sequence of *sub-rectangles* with their own palettes, their own
//! transparency index and a disposal rule, drawn onto a logical screen. The
//! `gif` crate decodes those sub-rectangles; nothing in it draws them, so the
//! compositor is here.
//!
//! The compositor is a port of the one inside `image`'s `codecs/gif.rs` that
//! this module replaces, and the port is deliberate rather than approximate,
//! because two of its choices are *not* what the specification says and the
//! pixels of every existing fixture depend on them:
//!
//! - The background colour is never used. `image` says so in as many words --
//!   `// intentionally ignore the background color for web compatibility` --
//!   and keeps its canvas at `Rgba8`, so a `Background`-disposed rectangle is
//!   restored to **transparent** rather than to the background index. Viewers
//!   do the same, and Pillow agrees: `target/cand-anim/gif-vs-pillow.py`
//!   compares 14 of 14 frames of the fixture against it.
//! - `DisposalMethod::Any` is treated as `Keep`. The specification leaves `Any`
//!   underspecified and most viewers treat it as `Keep`.
//!
//! The timeline comes from a second pass with `skip_frame_decoding(true)`, which
//! walks every frame's metadata without running LZW. That pass reads the delays,
//! the rectangles and the disposal methods, which is all the shared timeline
//! code needs; see `docs/improvements/28-animation-container-decoders.md` for
//! the measurement.
//!
//! Disposal is applied *after* the composited canvas is published, and the
//! canvas is never mutated while a published presentation still refers to it:
//! [`AnimationSource`] keeps a window of decoded pictures and a later frame must
//! not rewrite one of them.

use std::{
    fs::File,
    io::{BufReader, Seek},
    path::{Path, PathBuf},
};

// Fully qualified: this module is itself named `gif`.
use ::gif::{ColorOutput, DecodeOptions, DisposalMethod};

use crate::{
    decoder::{DecodeTimings, DecodedImage, Pixels, image_error},
    error::{ImgSeqError, Result},
    layout::ColorType,
    pixel::PixelFormat,
};

use super::{AnimationDecoder, AnimationSource, Presentation, Rate, SegmentInfo};

/// The rate a gif's delays are counted in.
///
/// A gif states a delay in hundredths of a second, so the timeline counts
/// microseconds and a delay of one is 10 ms exactly. `image` states the delay as
/// a `num/100` millisecond ratio; both reach the same ticks.
const RATE: Rate = Rate::new(1_000_000, 1);

/// One displayed picture's timing and rectangle, as the metadata pass reads it.
struct Timing {
    /// Delay in hundredths of a second, which is the unit a gif states.
    delay: u16,
}

/// Reads every frame's delay without decoding a pixel of any of them.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or its stream is
/// malformed.
fn timings(file: &mut BufReader<File>, path: &Path) -> Result<Vec<Timing>> {
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    // The whole point of this pass: the frame graph is walked, the LZW streams
    // are not run.
    options.skip_frame_decoding(true);
    let mut decoder = options
        .read_info(file)
        .map_err(|error| image_error("create decoder for", path, error))?;

    let mut timings = Vec::new();
    loop {
        let frame = decoder
            .read_next_frame()
            .map_err(|error| image_error("decode", path, error))?;
        let Some(frame) = frame else {
            break;
        };
        timings.push(Timing { delay: frame.delay });
    }
    Ok(timings)
}

/// Describes an animated gif's timeline without holding its pictures.
///
/// Returns `None` for a gif that displays one picture, which leaves it on the
/// still-image path it was on before.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is animated but cannot be read.
pub fn segment_info(
    path: &Path,
    info: crate::decoder::ImageInfo,
    fps: Rate,
    file: &mut BufReader<File>,
) -> Result<Option<SegmentInfo>> {
    let frames = timings(file, path)?;
    // A still gif is declined here, before anything is held, which is what keeps
    // a plain gif on the path it always took. One frame is one picture however
    // long it says it is displayed.
    if frames.len() < 2 {
        return Ok(None);
    }

    let mut presentations = Vec::with_capacity(frames.len());
    let mut timestamp = 0i64;
    for frame in &frames {
        // A hundredth of a second is 10 000 microseconds, and the rate above
        // counts microseconds.
        let duration = i64::from(frame.delay) * 10_000;
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
        Box::new(Source::new(path, info.transform, info.format)?),
    ));
    Ok(Some(SegmentInfo {
        info,
        rate: RATE,
        presentations,
        decoder: source,
        fps,
    }))
}

/// The logical screen and the embedded profile, from the header alone.
///
/// The still path asks for these here rather than opening the file a second
/// way: the options and the colour output have to be the ones the compositor
/// reads with, and both facts come from the same header read. `image`'s gif
/// reader took its size and its profile from the same crate call.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or is not a gif.
pub fn screen(file: &mut BufReader<File>, path: &Path) -> Result<(u32, u32, Option<Vec<u8>>)> {
    // The logical screen is at the front of the file, so the walk starts there
    // whatever the caller left the reader at.
    file.rewind()
        .map_err(|error| image_error("open", path, error))?;
    let decoder = open_with(&mut *file, path)?;
    Ok((
        u32::from(decoder.width()),
        u32::from(decoder.height()),
        decoder.icc_profile().map(<[u8]>::to_vec),
    ))
}

/// Decodes the one picture a gif that displays one picture shows.
///
/// A gif that displays one picture is that picture placed on a transparent canvas
/// and nothing else: no blend, and no disposal, because a disposal rule is about
/// what a *later* frame does to an earlier one and there is no later frame. That is
/// [`Canvas::place`], deliberately not the [`Canvas::compose`] the animation path
/// uses, and the difference shows in the red plane of a file that states a
/// transparent index.
///
/// A gif that displays several is its first *presentation* instead, which is what
/// the animation path shows for frame zero and therefore what a renamed copy of an
/// animation has always shown.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read, is malformed, or holds no
/// frame at all.
pub fn still(
    path: &Path,
    transform: crate::pixel::Transform,
    format: PixelFormat,
) -> Result<DecodedImage> {
    let mut source = Source::new(path, transform, format)?;
    // Which read is right is the file's to decide and not the caller's: the
    // header that answers it is the one already open, so this peeks rather than
    // paying a second pass over the metadata.
    // One read decides it and, for a single picture, is the whole decode: the
    // first sub-frame the peek decoded is that picture, so a file with no second
    // one is placed from it rather than decoded again. That used to take three
    // passes over the LZW stream -- two to answer this question and one to read
    // the picture -- and a one-picture gif is most of the gifs there are.
    let first = source.next_subframe()?;
    let animated = if first.is_some() {
        source.next_subframe()?.is_some()
    } else {
        false
    };
    if animated {
        // The animation path decodes its own presentations from the front.
        source.restart()?;
        return source.presentation(0);
    }
    let Some(frame) = first else {
        return Err(ImgSeqError::new(format!(
            "image '{}' holds no picture",
            path.display()
        )));
    };
    let buffer = source.canvas.place(&frame);
    Ok(DecodedImage {
        width: source.canvas.width as u32,
        height: source.canvas.height as u32,
        format: source.format,
        transform: source.transform,
        pixels: Pixels::Interleaved {
            color_type: ColorType::Rgba8,
            buffer: buffer.into(),
        },
        timings: DecodeTimings::default(),
    })
}

/// One decoded sub-rectangle, in the layout the `gif` crate hands over.
struct Subframe {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    dispose: DisposalMethod,
    pixels: Vec<u8>,
}

/// The compositor's canvas state, which is what makes a gif a sequence.
///
/// Only one buffer persists between frames, and it is not the displayed
/// picture: each displayed picture is built from the sub-rectangle that is
/// being drawn and the *undisposed* canvas underneath it. Keeping the drawn
/// canvas instead would leave the next frame's out-of-rectangle samples
/// reading content the last frame had already replaced.
struct Canvas {
    width: usize,
    height: usize,
    /// What a transparent sample shows and what a `Previous` disposal
    /// restores: the picture as it stood after the last frame that did not
    /// dispose of itself.
    undisposed: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        let (width, height) = (width as usize, height as usize);
        let bytes = width.saturating_mul(height).saturating_mul(4);
        Self {
            width,
            height,
            // Every sample starts transparent, which is what makes the part
            // of the first frame its rectangle does not cover show through as
            // nothing rather than as the background colour.
            undisposed: vec![0; bytes],
        }
    }

    /// Composes `frame` onto the undisposed canvas and answers the picture.
    ///
    /// The answer is a buffer of its own rather than the canvas, because a
    /// published presentation outlives the frame that produced it: the canvas
    /// goes on being disposed and drawn into afterwards.
    fn compose(&mut self, frame: &Subframe) -> Vec<u8> {
        let full = (frame.left, frame.top) == (0, 0)
            && (self.width, self.height) == (frame.width as usize, frame.height as usize);
        // A sub-rectangle that is the whole canvas *is* the picture: no sample
        // of it is out of bounds, so nothing has to be carried over. Anything
        // smaller starts from the canvas underneath it.
        let mut composed = if full {
            frame.pixels.clone()
        } else {
            self.undisposed.clone()
        };
        let area = clipped(frame, self.width, self.height);
        if area.across == 0 {
            return composed;
        }
        let frame_width = frame.width as usize;
        for y in area.top..area.bottom {
            let target = (y * self.width + area.left) * 4;
            let length = area.across * 4;
            if !full {
                // The rectangle's own row, copied whole. Only the rows and
                // columns the rectangle covers are walked, which is what makes
                // a small patch on a big canvas one memcpy a row rather than a
                // branch a pixel.
                let source = (y - frame.top as usize) * frame_width * 4;
                composed[target..target + length]
                    .copy_from_slice(&frame.pixels[source..source + length]);
            }
            blend_row(
                frame.dispose,
                &mut composed[target..target + length],
                &mut self.undisposed[target..target + length],
            );
        }
        composed
    }

    /// Places `frame` on a transparent canvas without compositing it.
    ///
    /// This is what a *still* gif is, and it is not [`compose`](Self::compose).
    /// The reader being replaced builds a still by copying the one frame's
    /// rectangle to its offset and leaving everything else transparent; it never
    /// blends, because a disposal rule is about what a *later* frame does to an
    /// earlier one and there is no later frame. The difference shows: a sample the
    /// frame's own transparency index leaves transparent keeps its palette colour
    /// here, where `compose` would replace it with whatever is underneath -- which
    /// for a first frame is nothing at all. Getting this wrong moves the red
    /// plane of every still gif that states a transparent index, and only that
    /// plane; see the `gif-still-alpha` fixture.
    fn place(&self, frame: &Subframe) -> Vec<u8> {
        let mut placed = vec![0u8; self.width.saturating_mul(self.height).saturating_mul(4)];
        let area = clipped(frame, self.width, self.height);
        if area.across == 0 {
            return placed;
        }
        let frame_width = frame.width as usize;
        for y in area.top..area.bottom {
            let target = (y * self.width + area.left) * 4;
            let length = area.across * 4;
            let source = (y - frame.top as usize) * frame_width * 4;
            placed[target..target + length].copy_from_slice(&frame.pixels[source..source + length]);
        }
        placed
    }
}

/// The part of a frame's rectangle that lands on the canvas.
///
/// A gif states its rectangle in absolute screen coordinates and a file can
/// state one that pokes past the screen, so a walk is over the intersection:
/// a rectangle that starts past the canvas covers nothing.
struct Clipped {
    top: usize,
    bottom: usize,
    left: usize,
    across: usize,
}

fn clipped(frame: &Subframe, width: usize, height: usize) -> Clipped {
    let left = (frame.left as usize).min(width);
    let top = (frame.top as usize).min(height);
    let right = (frame.left as usize)
        .saturating_add(frame.width as usize)
        .min(width);
    let bottom = (frame.top as usize)
        .saturating_add(frame.height as usize)
        .min(height);
    Clipped {
        top,
        bottom: bottom.max(top),
        left,
        across: right.saturating_sub(left),
    }
}

/// Blends one row of the composed picture and records what disposal must
/// restore.
///
/// This is the port of `image`'s `blend_and_dispose_pixel`, and its two
/// oddities are documented on the module; the per-sample walk it replaces ran
/// the disposal branch a pixel, and a frame's disposal is one value for its
/// whole rectangle. Hoisting it makes the row a select between two four-byte
/// values, which on aarch64 is four pixels a block: `vbslq_u32` with the alpha
/// byte of every pixel as its mask.
fn blend_row(dispose: DisposalMethod, composed: &mut [u8], undisposed: &mut [u8]) {
    #[cfg(target_arch = "aarch64")]
    let done = unsafe { neon::blend_row(dispose, composed, undisposed) };
    #[cfg(not(target_arch = "aarch64"))]
    let done = 0usize;
    blend_row_scalar(
        dispose,
        &mut composed[done * 4..],
        &mut undisposed[done * 4..],
    );
}

/// The scalar walk the kernel's tail and a machine without one run.
fn blend_row_scalar(dispose: DisposalMethod, composed: &mut [u8], undisposed: &mut [u8]) {
    let (current, _) = composed.as_chunks_mut::<4>();
    let (under, _) = undisposed.as_chunks_mut::<4>();
    for (current, under) in current.iter_mut().zip(under.iter_mut()) {
        // A transparent sample shows whatever was under it.
        if current[3] == 0 {
            current.copy_from_slice(under);
        }
        match dispose {
            DisposalMethod::Any | DisposalMethod::Keep => under.copy_from_slice(current),
            // Not the background colour; see the module note.
            DisposalMethod::Background => under.copy_from_slice(&[0, 0, 0, 0]),
            // `Previous` leaves the undisposed canvas alone, which is its point.
            DisposalMethod::Previous => {}
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    // Fully qualified: this module is itself named `gif`.
    use ::gif::DisposalMethod;
    use std::arch::aarch64::*;

    /// Four pixels a block, a select between the frame and the canvas.
    ///
    /// A pixel is one little-endian `u32`, so the alpha byte is the word's
    /// high byte and one masked compare a lane is the transparency test. The
    /// disposal branch is outside the loop because it is one value a frame.
    ///
    /// # Safety
    ///
    /// `composed` and `undisposed` must be the same length and a whole number
    /// of four-byte pixels, which every caller builds from a row.
    pub(super) unsafe fn blend_row(
        dispose: DisposalMethod,
        composed: &mut [u8],
        undisposed: &mut [u8],
    ) -> usize {
        let pixels = composed.len() / 4;
        let mut index = 0usize;
        let alpha = unsafe { vdupq_n_u32(0xFF00_0000) };
        let zero = unsafe { vdupq_n_u32(0) };
        while index + 4 <= pixels {
            // SAFETY: the block reads and writes sixteen bytes at `index` of
            // both rows, which the caller sized to `pixels` four-byte samples.
            unsafe {
                let current = vreinterpretq_u32_u8(vld1q_u8(composed.as_ptr().add(index * 4)));
                let under = vreinterpretq_u32_u8(vld1q_u8(undisposed.as_ptr().add(index * 4)));
                let transparent = vceqq_u32(vandq_u32(current, alpha), zero);
                let blended = vbslq_u32(transparent, under, current);
                vst1q_u8(
                    composed.as_mut_ptr().add(index * 4),
                    vreinterpretq_u8_u32(blended),
                );
                match dispose {
                    DisposalMethod::Any | DisposalMethod::Keep => vst1q_u8(
                        undisposed.as_mut_ptr().add(index * 4),
                        vreinterpretq_u8_u32(blended),
                    ),
                    DisposalMethod::Background => vst1q_u8(
                        undisposed.as_mut_ptr().add(index * 4),
                        vreinterpretq_u8_u32(zero),
                    ),
                    DisposalMethod::Previous => {}
                }
            }
            index += 4;
        }
        index
    }
}

/// Runs the kernel and the scalar tail directly, for the test that holds the
/// two to each other.
#[cfg(all(test, target_arch = "aarch64"))]
fn blend_row_forced(dispose: DisposalMethod, composed: &mut [u8], undisposed: &mut [u8]) {
    let done = unsafe { neon::blend_row(dispose, composed, undisposed) };
    blend_row_scalar(
        dispose,
        &mut composed[done * 4..],
        &mut undisposed[done * 4..],
    );
}

/// One pass over the file, which is what a source keeps per request.
struct Source {
    path: PathBuf,
    transform: crate::pixel::Transform,
    format: PixelFormat,
    width: u32,
    height: u32,
    canvas: Canvas,
    decoder: ::gif::Decoder<BufReader<File>>,
    /// How many presentations this pass has produced.
    next: usize,
}

impl Source {
    fn new(path: &Path, transform: crate::pixel::Transform, format: PixelFormat) -> Result<Self> {
        let decoder = open(path)?;
        // The canvas is the file's logical screen, which is what every frame is
        // drawn onto. A frame is clipped to it rather than allowed to grow it: a
        // file that draws outside its own screen is malformed, and a release
        // build must not read past a buffer.
        let (width, height) = (u32::from(decoder.width()), u32::from(decoder.height()));
        Ok(Self {
            path: path.to_path_buf(),
            transform,
            format,
            width,
            height,
            canvas: Canvas::new(width, height),
            decoder,
            next: 0,
        })
    }

    /// Opens a pass at the beginning of the file.
    fn restart(&mut self) -> Result<()> {
        self.decoder = open(&self.path)?;
        let (width, height) = (self.width, self.height);
        self.canvas = Canvas::new(width, height);
        self.next = 0;
        Ok(())
    }

    /// Reads the next sub-rectangle.
    fn next_subframe(&mut self) -> Result<Option<Subframe>> {
        let frame = self
            .decoder
            .read_next_frame()
            .map_err(|error| image_error("decode", &self.path, error))?;
        let Some(frame) = frame else {
            return Ok(None);
        };
        Ok(Some(Subframe {
            left: u32::from(frame.left),
            top: u32::from(frame.top),
            width: u32::from(frame.width),
            height: u32::from(frame.height),
            dispose: frame.dispose,
            pixels: frame.buffer.to_vec(),
        }))
    }

    /// Reads presentation `index`, advancing the pass to it.
    fn presentation(&mut self, index: usize) -> Result<DecodedImage> {
        // A pass is a forward cursor. An index behind it is answered by starting
        // again, which is also what resets the canvas: composing a later frame
        // needs every frame before it.
        if index < self.next {
            self.restart()?;
        }
        while self.next <= index {
            let Some(frame) = self.next_subframe()? else {
                return Err(ImgSeqError::new(format!(
                    "animated image '{}' holds no presentation {index}",
                    self.path.display()
                )));
            };
            self.next += 1;
            let composed = self.canvas.compose(&frame);
            if self.next - 1 == index {
                return Ok(DecodedImage {
                    width: self.canvas.width as u32,
                    height: self.canvas.height as u32,
                    format: self.format,
                    transform: self.transform,
                    pixels: Pixels::Interleaved {
                        color_type: ColorType::Rgba8,
                        buffer: composed.into(),
                    },
                    timings: DecodeTimings::default(),
                });
            }
        }
        unreachable!("the loop returns on the presentation it was asked for")
    }
}

/// Opens the file and reads its logical screen.
fn open(path: &Path) -> Result<::gif::Decoder<BufReader<File>>> {
    let file = File::open(path).map_err(|error| image_error("open", path, error))?;
    open_with(BufReader::new(file), path)
}

/// The same decoder over an open the caller already holds, which is what the
/// probe hands this while it describes the logical screen.
fn open_with<R: std::io::BufRead>(reader: R, path: &Path) -> Result<::gif::Decoder<R>> {
    let mut options = DecodeOptions::new();
    // The crate does the palette, transparency and interlacing work; this module
    // does the drawing, which is the part the crate does not offer.
    options.set_color_output(ColorOutput::RGBA);
    options
        .read_info(reader)
        .map_err(|error| image_error("create decoder for", path, error))
}

impl AnimationDecoder for Source {
    fn seek(&mut self, index: usize) -> Result<()> {
        if index < self.next {
            self.restart()?;
        }
        Ok(())
    }

    fn next_presentation(&mut self) -> Result<DecodedImage> {
        let index = self.next;
        self.presentation(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sub-rectangle of `width` by `height` at `(left, top)`, every sample of
    /// it `sample`, disposed of by `dispose`.
    fn subframe(
        left: u32,
        top: u32,
        width: u32,
        height: u32,
        sample: [u8; 4],
        dispose: DisposalMethod,
    ) -> Subframe {
        Subframe {
            left,
            top,
            width,
            height,
            dispose,
            pixels: sample.repeat((width * height) as usize),
        }
    }

    /// A sample of the composed picture.
    fn at(canvas: &Canvas, pixels: &[u8], x: usize, y: usize) -> [u8; 4] {
        let index = (y * canvas.width + x) * 4;
        pixels[index..index + 4].try_into().expect("four samples")
    }

    /// A frame that covers the whole canvas is the picture, and it is not
    /// blended against itself. This is the case a compositor gets wrong by
    /// starting from an empty canvas and never copying the frame in.
    #[test]
    fn a_full_canvas_frame_is_the_picture() {
        let mut canvas = Canvas::new(2, 2);
        let red = [255, 0, 0, 255];
        let composed = canvas.compose(&subframe(0, 0, 2, 2, red, DisposalMethod::Keep));
        for y in 0..2 {
            for x in 0..2 {
                assert_eq!(at(&canvas, &composed, x, y), red, "{x},{y}");
            }
        }
    }

    /// A sub-rectangle is drawn where it says it is, and every sample outside
    /// it keeps what the canvas already held.
    #[test]
    fn a_sub_rectangle_lands_where_it_says() {
        let mut canvas = Canvas::new(3, 3);
        let base = [10, 20, 30, 255];
        let _ = canvas.compose(&subframe(0, 0, 3, 3, base, DisposalMethod::Keep));
        let patch = [40, 50, 60, 255];
        let composed = canvas.compose(&subframe(1, 1, 1, 1, patch, DisposalMethod::Keep));
        assert_eq!(at(&canvas, &composed, 0, 0), base);
        assert_eq!(at(&canvas, &composed, 1, 1), patch, "the rectangle itself");
        assert_eq!(
            at(&canvas, &composed, 2, 2),
            base,
            "the far corner is untouched"
        );
    }

    /// A `Background` disposal restores the rectangle to *transparent*, not to
    /// the file's background colour. This is one of the two places the ported
    /// compositor deliberately departs from the specification, and Pillow
    /// agrees with it; see the module note.
    #[test]
    fn a_background_disposal_restores_transparency() {
        let mut canvas = Canvas::new(2, 2);
        let painted = [10, 20, 30, 255];
        let _ = canvas.compose(&subframe(0, 0, 2, 2, painted, DisposalMethod::Background));
        // The next frame paints nothing anywhere, so every sample shows what
        // the disposal left behind: transparency, not `painted`.
        let composed = canvas.compose(&subframe(0, 0, 1, 1, painted, DisposalMethod::Keep));
        assert_eq!(at(&canvas, &composed, 1, 1), [0, 0, 0, 0]);
    }

    /// A `Previous` disposal means the frame's own drawing does **not** become
    /// the base for the next frame: the canvas the next frame starts from is
    /// the last one that did not dispose of itself.
    ///
    /// That is what the variant is for -- it is how a gif draws something for one
    /// frame only -- and it is why `blend` is handed the sample and the
    /// undisposed canvas separately.
    #[test]
    fn a_previous_disposal_does_not_become_the_next_base() {
        let mut canvas = Canvas::new(2, 2);
        let painted = [10, 20, 30, 255];
        let _ = canvas.compose(&subframe(0, 0, 2, 2, painted, DisposalMethod::Keep));
        let patch = [40, 50, 60, 255];
        // Drawn for this frame only.
        let shown = canvas.compose(&subframe(0, 0, 1, 1, patch, DisposalMethod::Previous));
        assert_eq!(at(&canvas, &shown, 0, 0), patch, "it is displayed");
        // The next frame paints the opposite corner, so the first corner shows
        // whatever the base holds: `painted`, not the frame that disposed of
        // itself.
        let composed = canvas.compose(&subframe(1, 1, 1, 1, patch, DisposalMethod::Keep));
        assert_eq!(
            at(&canvas, &composed, 0, 0),
            painted,
            "the base is unchanged"
        );
        assert_eq!(at(&canvas, &composed, 1, 1), patch);
    }

    /// A transparent sample shows what is under it rather than the palette
    /// colour that shares its index.
    #[test]
    fn a_transparent_sample_shows_what_is_under_it() {
        let mut canvas = Canvas::new(2, 2);
        let painted = [10, 20, 30, 255];
        let _ = canvas.compose(&subframe(0, 0, 2, 2, painted, DisposalMethod::Keep));
        let composed = canvas.compose(&subframe(0, 0, 1, 1, [99, 99, 99, 0], DisposalMethod::Keep));
        assert_eq!(at(&canvas, &composed, 0, 0), painted);
    }

    /// The blend row and its NEON kernel against the per-pixel walk the row
    /// replaces, for every disposal mode and every tail.
    #[test]
    fn a_blend_row_is_the_per_pixel_blend_sample_for_sample() {
        for dispose in [
            DisposalMethod::Any,
            DisposalMethod::Keep,
            DisposalMethod::Background,
            DisposalMethod::Previous,
        ] {
            for pixels in 0..=33usize {
                let under: Vec<u8> = (0..pixels * 4).map(|i| (i * 3 % 251) as u8).collect();
                let composed: Vec<u8> = (0..pixels * 4)
                    .map(|i| match i % 4 {
                        3 if (i / 4) % 3 == 0 => 0u8,
                        3 => 255u8,
                        _ => (i * 7 % 253) as u8,
                    })
                    .collect();
                let mut expected_under = under.clone();
                let mut expected_composed = composed.clone();
                blend_per_sample(dispose, &mut expected_under, &mut expected_composed);

                let mut walked_under = under.clone();
                let mut walked_composed = composed.clone();
                blend_row(dispose, &mut walked_composed, &mut walked_under);
                assert_eq!(
                    (walked_under, walked_composed),
                    (expected_under.clone(), expected_composed.clone()),
                    "{dispose:?}, {pixels} pixels"
                );

                #[cfg(target_arch = "aarch64")]
                {
                    let mut kernel_under = under;
                    let mut kernel_composed = composed;
                    blend_row_forced(dispose, &mut kernel_composed, &mut kernel_under);
                    assert_eq!(
                        (kernel_under, kernel_composed),
                        (expected_under, expected_composed),
                        "kernel: {dispose:?}, {pixels} pixels"
                    );
                }
            }
        }
    }

    /// What the compose row costs, which is a measurement rather than a check.
    ///
    /// It is ignored because a wall clock in a test suite is a flaky test, and
    /// it lives here rather than under `target/bench/` because it calls the
    /// row directly.
    ///
    /// Three canvas sizes, which answer three questions. 256x256 is a quarter
    /// of a megabyte a buffer, so the row is its own arithmetic; 1024x1024 is
    /// the shape `target/bench/plan48/anim-corpus` animates, which is what a
    /// frame really walks; 4096x4096 is large enough that the row is bound by
    /// memory instead. A block's worth and a frame's worth are different
    /// numbers, and the second is the one that decides whether to land one.
    /// This is the measurement that declined the x86-64 halves of the block
    /// on a host whose scalar walk already runs at the memory system's
    /// ceiling; `docs/improvements/48-decoder-vectorisation.md` has the
    /// numbers.
    ///
    /// ```text
    /// cargo test --release --lib -- --ignored --nocapture the_compose_measured
    /// ```
    #[test]
    #[ignore = "a measurement"]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn the_compose_measured() {
        use std::time::Instant;

        const CANVASES: [(usize, usize); 3] = [(256, 256), (1024, 1024), (4096, 4096)];
        const REPEATS: u32 = 20;

        let dispose = DisposalMethod::Keep;

        /// One loop this test times, by the name it is reported under.
        type Row = (&'static str, Box<dyn Fn(&mut [u8], &mut [u8])>);

        /// Times every path over one canvas, alternating them round by round so
        /// that a machine that drifts cannot flatter one, and reports each
        /// path's best round. This is `target/bench/plan48/ab.py`'s method
        /// with the two plugins replaced by the two loops.
        fn measure(canvas: (usize, usize), paths: &[Row], composed: &[u8], under: &[u8]) {
            let (width, height) = canvas;
            let row = width * 4;
            let mut best = vec![f64::MAX; paths.len()];
            for _ in 0..REPEATS {
                for (index, (_, run)) in paths.iter().enumerate() {
                    let mut composed = composed.to_vec();
                    let mut under = under.to_vec();
                    let start = Instant::now();
                    for y in 0..height {
                        let at = y * row;
                        run(&mut composed[at..at + row], &mut under[at..at + row]);
                    }
                    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                    // A row's writes are never read afterwards, so without this
                    // the optimiser is free to delete the loop and time an empty
                    // one.
                    std::hint::black_box((&composed, &under));
                    best[index] = best[index].min(elapsed);
                }
            }
            for ((name, _), best) in paths.iter().zip(best) {
                let pixel = best * 1e6 / (width * height) as f64;
                println!("{width:<5} {name:<12} {best:8.3} ms a canvas {pixel:6.3} ns a pixel");
            }
        }

        for (width, height) in CANVASES {
            // Every third pixel transparent, like a rectangle that covers
            // part of the canvas it is drawn over.
            let under: Vec<u8> = (0..width * height * 4)
                .map(|i| (i * 3 % 251) as u8)
                .collect();
            let composed: Vec<u8> = (0..width * height * 4)
                .map(|i| match i % 4 {
                    3 if (i / 4) % 3 == 0 => 0u8,
                    3 => 255u8,
                    _ => (i * 7 % 253) as u8,
                })
                .collect();
            let canvas = (width, height);

            let mut paths: Vec<Row> = vec![(
                "scalar",
                Box::new(move |composed, under| blend_row_scalar(dispose, composed, under)),
            )];
            #[cfg(target_arch = "aarch64")]
            paths.push((
                "neon",
                Box::new(move |composed, under| blend_row_forced(dispose, composed, under)),
            ));
            paths.push((
                "dispatched",
                Box::new(move |composed, under| blend_row(dispose, composed, under)),
            ));
            measure(canvas, &paths, &composed, &under);
        }
    }

    /// The per-pixel walk the row replaced, kept here as the oracle.
    fn blend_per_sample(dispose: DisposalMethod, under: &mut [u8], composed: &mut [u8]) {
        for index in 0..under.len() / 4 {
            let at = index * 4;
            if composed[at + 3] == 0 {
                let below: [u8; 4] = under[at..at + 4].try_into().expect("four samples");
                composed[at..at + 4].copy_from_slice(&below);
            }
            match dispose {
                DisposalMethod::Any | DisposalMethod::Keep => {
                    let now: [u8; 4] = composed[at..at + 4].try_into().expect("four samples");
                    under[at..at + 4].copy_from_slice(&now);
                }
                DisposalMethod::Background => under[at..at + 4].copy_from_slice(&[0, 0, 0, 0]),
                DisposalMethod::Previous => {}
            }
        }
    }
}
