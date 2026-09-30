//! The animation adapters that ride the `image` crate's frame iterator.
//!
//! Animated GIF and animated WebP both arrive as a stream of already composited
//! logical canvases: `image` runs each format's blend and disposal before it
//! hands a frame over, so there is no composition to do here. What is left is
//! the timeline, which is what this module reads and replays.
//!
//! An iterator cannot be seeked, so a backward request restarts it. The first
//! pass through the file is also where the delays come from, which is why the
//! source discovers them rather than being handed them by its caller: the
//! container states them per frame and the only way `image` exposes them is by
//! iterating.

use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use image::{
    AnimationDecoder, ColorType, Frame, ImageReader,
    codecs::{gif::GifDecoder, webp::WebPDecoder},
};

use crate::{
    decoder::{DecodeTimings, DecodedImage, Pixels, image_error},
    error::{ImgSeqError, Result},
    pixel::PixelFormat,
};

use super::{AnimationDecoder as SourceDecoder, AnimationSource, Presentation, Rate, SegmentInfo};

/// A format `image` decodes as a stream of full-canvas animation frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Gif,
    Webp,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Gif => "gif",
            Self::Webp => "webp",
        }
    }
}

/// The two decoders whose frames are already composited logical canvases.
///
/// Both hand their frames over as owned buffers, so neither borrows the reader
/// and the frame iterator can be erased behind one type. The alternative, a box
/// over `impl ImageDecoder`, cannot reach `into_frames`: that method lives on
/// the concrete decoders rather than on the `ImageDecoder` trait, and it also
/// carries a lifetime that would not be `Send`.
enum Decoder {
    Gif(Box<GifDecoder<BufReader<File>>>),
    Webp(Box<WebPDecoder<BufReader<File>>>),
}

impl Decoder {
    fn into_frames(self) -> Frames {
        match self {
            Self::Gif(decoder) => Box::new(AnimationDecoder::into_frames(*decoder)),
            Self::Webp(decoder) => Box::new(AnimationDecoder::into_frames(*decoder)),
        }
    }
}

/// Whether `path` names a file of `kind`.
#[must_use]
pub fn owns(path: &Path, kind: Kind) -> bool {
    let extension = match kind {
        Kind::Gif => "gif",
        Kind::Webp => "webp",
    };
    path.extension()
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

/// Describes an animated gif or webp's timeline without holding its frames.
///
/// Returns `None` for a file of that format that is not animated, which leaves
/// it on the still-image path it was on before.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is animated but cannot be read.
pub fn segment_info(
    path: &Path,
    kind: Kind,
    info: crate::decoder::ImageInfo,
    fps: Rate,
) -> Result<Option<SegmentInfo>> {
    // A first pass discovers the delays and the canvas. A still file is
    // declined here, before anything is held, which is what keeps a plain gif
    // or webp on the path it always took.
    let canvas = match FrameSource::new(path, kind, info.transform, info.format).durations() {
        Ok(durations) if durations.len() > 1 => durations,
        Ok(_) => return Ok(None),
        Err(error) => return Err(error),
    };

    let mut presentations = Vec::with_capacity(canvas.len());
    let mut timestamp = 0i64;
    for (num, den) in &canvas {
        // `numer_denom_ms` states the delay as a fraction of a millisecond:
        // `num/den` ms. It is kept as microseconds so a delay finer than a
        // millisecond survives, and the rate below says so.
        let duration = (*num as i64) * 1_000 / (*den as i64);
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
        Box::new(StreamSource::new(path, kind, info.transform, info.format)?),
    ));
    Ok(Some(SegmentInfo {
        info,
        // A delay is stated in milliseconds, and this timeline counts
        // microseconds so that a fraction of one is not rounded away.
        rate: Rate::new(1_000_000, 1),
        presentations,
        decoder: source,
        fps,
    }))
}

/// One pass over a file's animation, as the `image` crate exposes it.
struct FrameSource {
    path: PathBuf,
    kind: Kind,
    transform: crate::pixel::Transform,
    format: PixelFormat,
    /// Whether the canvas has been measured, which the first frame reports.
    canvas: Option<(u32, u32)>,
}

impl FrameSource {
    fn new(
        path: &Path,
        kind: Kind,
        transform: crate::pixel::Transform,
        format: PixelFormat,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            kind,
            transform,
            format,
            canvas: None,
        }
    }

    /// Reads every frame's delay and returns them in timeline order.
    ///
    /// The canvas is learned from the first frame and checked against every
    /// later one: a format that changes size part way through its timeline
    /// cannot share a clip, and `mismatch` does not apply to one file's own
    /// frames.
    fn durations(&mut self) -> Result<Vec<(u32, u32)>> {
        let frames = self.frames()?;
        let mut durations = Vec::new();
        for frame in frames {
            let frame = frame.map_err(|error| image_error("decode", &self.path, error))?;
            self.measure(&frame)?;
            durations.push(frame.delay().numer_denom_ms());
        }
        // A still file of one of these formats yields no animation frames at
        // all, which is what tells the caller it is not an animation rather
        // than what makes it a failure.
        if durations.is_empty() {
            return Ok(durations);
        }
        if self.canvas.is_none() {
            return Err(ImgSeqError::new(format!(
                "animated image '{}' states frames but no canvas",
                self.path.display()
            )));
        }
        Ok(durations)
    }

    /// Decodes presentation `index` of one pass, or `None` at the end.
    fn presentation(&mut self, frames: &mut Frames, index: usize) -> Result<Option<DecodedImage>> {
        let Some(frame) = frames.next() else {
            return Ok(None);
        };
        let frame = frame.map_err(|error| image_error("decode", &self.path, error))?;
        self.measure(&frame)?;
        let _ = index;
        Ok(Some(self.decoded(frame)))
    }

    fn measure(&mut self, frame: &Frame) -> Result<()> {
        let buffer = frame.buffer();
        let canvas = (buffer.width(), buffer.height());
        match self.canvas {
            None => self.canvas = Some(canvas),
            Some(known) if known != canvas => {
                return Err(ImgSeqError::new(format!(
                    "animated image '{}' changes size at frame {}x{}, which cannot share a clip",
                    self.path.display(),
                    canvas.0,
                    canvas.1
                )));
            }
            Some(_) => {}
        }
        Ok(())
    }

    fn decoded(&self, frame: Frame) -> DecodedImage {
        let (width, height) = self.canvas.unwrap_or((0, 0));
        let buffer = frame.into_buffer().into_raw();
        DecodedImage {
            width,
            height,
            format: self.format,
            transform: self.transform,
            pixels: Pixels::Interleaved {
                color_type: ColorType::Rgba8,
                buffer,
            },
            timings: DecodeTimings {
                open: std::time::Duration::ZERO,
                metadata: std::time::Duration::ZERO,
                buffer: std::time::Duration::ZERO,
                read: std::time::Duration::ZERO,
            },
        }
    }

    /// Opens a new pass over the file.
    fn frames(&self) -> Result<Frames> {
        // The format is identified first, so a file whose extension lies about
        // it is refused rather than decoded as something else.
        let identified = ImageReader::open(&self.path)
            .map_err(|error| image_error("open", &self.path, error))?
            .with_guessed_format()
            .map_err(|error| image_error("identify", &self.path, error))?;
        let expected = match self.kind {
            Kind::Gif => image::ImageFormat::Gif,
            Kind::Webp => image::ImageFormat::WebP,
        };
        if identified.format() != Some(expected) {
            return Err(ImgSeqError::new(format!(
                "animated image '{}' is not the {} this reader opened it as",
                self.path.display(),
                self.kind.name()
            )));
        }
        drop(identified);

        let file =
            File::open(&self.path).map_err(|error| image_error("open", &self.path, error))?;
        let reader = BufReader::new(file);
        let decoder = match self.kind {
            Kind::Gif => {
                Decoder::Gif(Box::new(GifDecoder::new(reader).map_err(|error| {
                    image_error("create decoder for", &self.path, error)
                })?))
            }
            Kind::Webp => {
                Decoder::Webp(Box::new(WebPDecoder::new(reader).map_err(|error| {
                    image_error("create decoder for", &self.path, error)
                })?))
            }
        };
        Ok(decoder.into_frames())
    }
}

/// The frame iterator of one `image` decoder, boxed so its type does not leak.
type Frames = Box<dyn Iterator<Item = image::ImageResult<Frame>>>;

/// One pass over the file, which is what the shared source keeps per request.
///
/// # Safety
///
/// The frame iterator is not `Send`, because `image` erases it behind a box
/// that carries no auto trait. It never crosses a thread boundary while it is
/// alive: a source is owned by one [`AnimationSource`], which serialises every
/// call to it behind its own mutex, so only the thread that built the iterator
/// can ever touch it. The value that is moved between workers is the source,
/// and it holds no iterator until a worker asks for a presentation.
struct StreamSource {
    source: FrameSource,
    /// The pass in progress, which is built when it is first needed.
    ///
    /// An `image` frame iterator borrows its decoder and is not `Send`, so it
    /// is held only while a presentation is being read rather than across
    /// calls. Only one thread reaches a source at a time, because the shared
    /// [`AnimationSource`] serialises access to it.
    frames: Option<Frames>,
    /// How many presentations this pass has produced.
    next: usize,
}

impl StreamSource {
    fn new(
        path: &Path,
        kind: Kind,
        transform: crate::pixel::Transform,
        format: PixelFormat,
    ) -> Result<Self> {
        Ok(Self {
            source: FrameSource::new(path, kind, transform, format),
            frames: None,
            next: 0,
        })
    }
}

// SAFETY: see the note on the type. The iterator inside is only ever reached
// through the owning source's mutex, so it is used by one thread at a time.
unsafe impl Send for StreamSource {}

impl SourceDecoder for StreamSource {
    fn seek(&mut self, index: usize) -> Result<()> {
        if index < self.next || self.frames.is_none() {
            // An iterator has no index, so a backward step is a new pass.
            self.frames = None;
            self.next = 0;
        }
        Ok(())
    }

    fn next_presentation(&mut self) -> Result<DecodedImage> {
        let index = self.next;
        if self.frames.is_none() {
            self.frames = Some(self.source.frames()?);
        }
        let frames = self.frames.as_mut().expect("the pass was just built");
        let image = self.source.presentation(frames, index)?.ok_or_else(|| {
            ImgSeqError::new(format!(
                "animated image '{}' holds no presentation {index}",
                self.source.path.display()
            ))
        })?;
        self.next += 1;
        Ok(image)
    }
}
