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
    io::BufReader,
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
fn timings(path: &Path) -> Result<Vec<Timing>> {
    crate::animation::count_timeline_read();
    let file = File::open(path).map_err(|error| image_error("open", path, error))?;
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    // The whole point of this pass: the frame graph is walked, the LZW streams
    // are not run.
    options.skip_frame_decoding(true);
    let mut decoder = options
        .read_info(BufReader::new(file))
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
) -> Result<Option<SegmentInfo>> {
    let frames = timings(path)?;
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
pub fn screen(path: &Path) -> Result<(u32, u32, Option<Vec<u8>>)> {
    let decoder = open(path)?;
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
    let animated = source.next_subframe()?.is_some() && source.next_subframe()?.is_some();
    source.restart()?;
    if animated {
        return source.presentation(0);
    }
    let Some(frame) = source.next_subframe()? else {
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
            buffer,
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
        for y in 0..self.height {
            for x in 0..self.width {
                let frame_x = x.wrapping_sub(frame.left as usize);
                let frame_y = y.wrapping_sub(frame.top as usize);
                if frame_x >= frame.width as usize || frame_y >= frame.height as usize {
                    // Outside the rectangle: the undisposed sample stands,
                    // which `composed` already holds.
                    continue;
                }
                let canvas = (y * self.width + x) * 4;
                let source = (frame_y * frame.width as usize + frame_x) * 4;
                composed[canvas..canvas + 4].copy_from_slice(&frame.pixels[source..source + 4]);
                blend(
                    frame.dispose,
                    &mut self.undisposed[canvas..canvas + 4],
                    &mut composed[canvas..canvas + 4],
                );
            }
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
        for y in 0..self.height {
            for x in 0..self.width {
                let frame_x = x.wrapping_sub(frame.left as usize);
                let frame_y = y.wrapping_sub(frame.top as usize);
                if frame_x >= frame.width as usize || frame_y >= frame.height as usize {
                    continue;
                }
                let to = (y * self.width + x) * 4;
                let from = (frame_y * frame.width as usize + frame_x) * 4;
                placed[to..to + 4].copy_from_slice(&frame.pixels[from..from + 4]);
            }
        }
        placed
    }
}

/// Blends one sample and records what disposal must restore.
///
/// This is the port of `image`'s `blend_and_dispose_pixel`, and its two
/// oddities are documented on the module. `current` is the sample about to be
/// displayed and `undisposed` is what a `Previous` disposal restores.
fn blend(dispose: DisposalMethod, undisposed: &mut [u8], current: &mut [u8]) {
    // A transparent sample shows whatever was under it.
    if current[3] == 0 {
        current.copy_from_slice(undisposed);
    }
    match dispose {
        DisposalMethod::Any | DisposalMethod::Keep => undisposed.copy_from_slice(current),
        // Not the background colour; see the module note.
        DisposalMethod::Background => undisposed.copy_from_slice(&[0, 0, 0, 0]),
        // `Previous` leaves the undisposed canvas alone, which is its point.
        DisposalMethod::Previous => {}
    }
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
                        buffer: composed,
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
    let mut options = DecodeOptions::new();
    // The crate does the palette, transparency and interlacing work; this module
    // does the drawing, which is the part the crate does not offer.
    options.set_color_output(ColorOutput::RGBA);
    options
        .read_info(BufReader::new(file))
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
}
