//! The clips an image sequence hands out, and the frames they deliver.
//!
//! The lookahead worker that reads a file also builds the frames of every clip
//! of the call, so the thread that answers a frame request only has to hand a
//! finished frame to VapourSynth. Decoding on that thread instead put the frame
//! allocation, the page faults of first touching it, and the channel
//! deinterleave on the critical path of every request; see
//! `docs/improvements/02-frame-write-path.md`.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use vapoursynth4_rs::{
    core::{Core, CoreRef},
    frame::VideoFrame,
};

use crate::{
    color::set_frame_properties,
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels},
    error::{ImgSeqError, Result},
    pixel::{
        PixelFormat, WriteTimings, write_alpha, write_decoded_planes, write_opaque_alpha,
        write_planar,
    },
    prefetch::{Payload, Prepare},
};

/// One of the clips an image sequence hands out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Clip {
    /// Color planes of every frame.
    Color,
    /// Alpha plane of every frame, filled with the opaque value when the file
    /// has no alpha channel.
    Alpha,
}

impl Clip {
    /// Pixel format this clip uses for an image decoded as `format`.
    #[must_use]
    pub const fn pixel_format(self, format: PixelFormat) -> PixelFormat {
        match self {
            Self::Color => format,
            Self::Alpha => format.alpha_format(),
        }
    }

    /// `ImgSeqAlpha` property of this clip's frames; color clips have none.
    #[must_use]
    pub const fn alpha_marker(self) -> Option<bool> {
        match self {
            Self::Color => None,
            Self::Alpha => Some(true),
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Alpha => "alpha",
        }
    }
}

/// Clips `Read` hands out.
pub const READ_CLIPS: &[Clip] = &[Clip::Color];
/// Clips `ReadAlpha` hands out.
pub const READ_ALPHA_CLIPS: &[Clip] = &[Clip::Color, Clip::Alpha];

/// Output format of one clip of a sequence of `format` images.
#[must_use]
pub fn query_format(core: &Core, format: PixelFormat) -> vapoursynth4_rs::frame::VideoFormat {
    let (sub_sampling_w, sub_sampling_h) = format.sub_sampling();
    core.query_video_format(
        format.color_family(),
        format.sample_type(),
        format.bits_per_sample(),
        sub_sampling_w,
        sub_sampling_h,
    )
}

/// Frame bytes one image of `clips` is expected to need.
///
/// This is the planar size of every frame the image is written into, which is
/// what the lookahead budget is sized from before anything is decoded. The
/// frames are sized from the same answer the probe gave, so an orientation that
/// swaps width and height is accounted for here as well.
/// VapourSynth pads the stride of a frame, so what the frames really hold is a
/// little more than this.
#[must_use]
pub fn expected_bytes(clips: &[Clip], image: &ImageInfo) -> usize {
    let width = usize::try_from(image.output_width()).unwrap_or(usize::MAX);
    let height = usize::try_from(image.output_height()).unwrap_or(usize::MAX);
    clips
        .iter()
        .map(|&clip| clip.pixel_format(image.format).planes_bytes(width, height))
        .sum()
}

/// What one clip's frame needed, measured where the work happened.
#[derive(Clone, Copy, Debug)]
pub struct FrameTimings {
    /// Creating the frame in the core.
    pub allocate: Duration,
    /// Writing the decoded pixels into its planes.
    pub write: WriteTimings,
    /// Attaching the source metadata.
    pub properties: Duration,
}

/// The frames of every clip of one call, built from one decoded image.
///
/// Cloning one hands out the same frames again: a clone only takes a reference
/// to each frame, so a second clip of a call asks the pool for this payload and
/// clones the frame it needs.
#[derive(Clone)]
pub struct ClipFrames {
    clips: Arc<[Clip]>,
    frames: Box<[VideoFrame]>,
    timings: Box<[FrameTimings]>,
    decode: DecodeTimings,
    bytes: usize,
}

impl ClipFrames {
    /// Frame of `clip`.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] if this call does not hand out `clip`.
    pub fn frame(&self, clip: Clip) -> Result<VideoFrame> {
        let index = self.clip_index(clip)?;
        Ok(self.frames[index].clone())
    }

    /// Decode timings of the image these frames were written from.
    #[must_use]
    pub fn decode_timings(&self) -> &DecodeTimings {
        &self.decode
    }

    /// What writing one clip's frame cost, in the worker that did it.
    #[must_use]
    pub fn frame_timings(&self, clip: Clip) -> Option<FrameTimings> {
        self.clip_index(clip)
            .ok()
            .and_then(|index| self.timings.get(index))
            .copied()
    }

    fn clip_index(&self, clip: Clip) -> Result<usize> {
        self.clips
            .iter()
            .position(|&candidate| candidate == clip)
            .ok_or_else(|| {
                ImgSeqError::new(format!(
                    "this call does not hand out the {} clip",
                    clip.name()
                ))
            })
    }

    /// Builds every frame of `clips` from one decoded image.
    fn build(
        core: &Core,
        clips: &Arc<[Clip]>,
        image: &ImageInfo,
        index: i32,
        decoded: DecodedImage,
    ) -> Result<Self> {
        let index = usize::try_from(index)
            .map_err(|_| ImgSeqError::new(format!("requested invalid frame {index}")))?;
        let mut frames = Vec::with_capacity(clips.len());
        let mut timings = Vec::with_capacity(clips.len());
        let mut bytes: usize = 0;
        // A rotation may have swapped the stored size, and the frames are the
        // size the pixels are written as rather than the size the file holds.
        let (output_width, output_height) = (decoded.output_width(), decoded.output_height());
        for &clip in clips.iter() {
            let format = clip.pixel_format(decoded.format);
            let allocate_started = Instant::now();
            let mut frame = new_frame(core, format, output_width, output_height)?;
            let allocate = allocate_started.elapsed();
            let write = write_frame(&mut frame, clip, format, &decoded)?;
            let properties_started = Instant::now();
            set_frame_properties(&mut frame, image, index, format, clip.alpha_marker())?;
            let properties = properties_started.elapsed();
            bytes = bytes.saturating_add(frame_bytes(&frame));
            frames.push(frame);
            timings.push(FrameTimings {
                allocate,
                write,
                properties,
            });
        }
        Ok(Self {
            clips: Arc::clone(clips),
            frames: frames.into_boxed_slice(),
            timings: timings.into_boxed_slice(),
            decode: decoded.timings,
            bytes,
        })
    }
}

impl Payload for ClipFrames {
    fn bytes(&self) -> usize {
        self.bytes
    }
}

/// The core of the graph, kept so that lookahead workers can create frames.
///
/// [`Filter::create`](vapoursynth4_rs::node::Filter) is handed a core borrowed
/// for that call, which is extended to `'static` in [`FrameBuilder::new`]. A
/// filter is freed before the core it was built with, and a pool lives inside
/// the filter that owns it, so a handle kept by a pool cannot dangle.
struct SharedCore(CoreRef<'static>);

// SAFETY: the handle is only used from the pool's workers to create a frame and
// to look up a video format, both of which VapourSynth allows from any thread:
// it schedules a filter's own `get_frame`, where frames are created, on
// whichever thread it likes.
unsafe impl Send for SharedCore {}
unsafe impl Sync for SharedCore {}

impl SharedCore {
    /// The core handle, borrowed from the handle the pool holds.
    fn core(&self) -> &Core {
        self.0.as_ref()
    }
}

/// Builds the frames of every clip of one call, on the worker that decoded the
/// image.
pub struct FrameBuilder {
    core: SharedCore,
    clips: Arc<[Clip]>,
}

impl FrameBuilder {
    /// Builder of the clips of one call, owned by the filter built with `core`.
    ///
    /// The builder must stay inside the filter that was created with this core:
    /// a filter is freed before its core, so a handle kept by a filter cannot
    /// outlive what it points at.
    #[must_use]
    pub fn new(core: CoreRef<'_>, clips: &[Clip]) -> Self {
        // SAFETY: the filter built from this core owns the builder, so the
        // handle cannot outlive the core. `SharedCore` records why the workers
        // that use it may do so.
        let core =
            SharedCore(unsafe { std::mem::transmute::<CoreRef<'_>, CoreRef<'static>>(core) });
        Self {
            core,
            clips: Arc::from(clips),
        }
    }
}

impl Prepare for FrameBuilder {
    type Payload = ClipFrames;

    fn estimate(&self, image: &ImageInfo) -> usize {
        expected_bytes(&self.clips, image)
    }

    fn build(&self, image: &ImageInfo, index: i32, decoded: DecodedImage) -> Result<Self::Payload> {
        ClipFrames::build(self.core.core(), &self.clips, image, index, decoded)
    }
}

/// Creates one frame of `format`.
fn new_frame(core: &Core, format: PixelFormat, width: u32, height: u32) -> Result<VideoFrame> {
    let width = i32::try_from(width)
        .map_err(|_| ImgSeqError::new("image width does not fit VapourSynth"))?;
    let height = i32::try_from(height)
        .map_err(|_| ImgSeqError::new("image height does not fit VapourSynth"))?;
    Ok(core.new_video_frame(&query_format(core, format), width, height, None))
}

/// Writes the decoded pixels of one image into one clip's frame.
///
/// The frames are the size `decoded.transform` produces, and both clips of an
/// image are written with the same transform, so the alpha plane lands where the
/// colour planes do rather than the two agreeing by construction.
fn write_frame(
    frame: &mut VideoFrame,
    clip: Clip,
    format: PixelFormat,
    decoded: &DecodedImage,
) -> Result<WriteTimings> {
    let transform = decoded.transform;
    match (clip, &decoded.pixels) {
        (Clip::Color, Pixels::Planar(planes)) => write_decoded_planes(
            frame,
            decoded.format,
            decoded.width,
            decoded.height,
            planes,
            transform,
        ),
        (Clip::Color, Pixels::Interleaved { color_type, buffer }) => write_planar(
            frame,
            *color_type,
            decoded.width,
            decoded.height,
            buffer,
            transform,
        ),
        // Planar decodes have no alpha channel to read: the format that decodes
        // to planes only takes files without one.
        (Clip::Alpha, Pixels::Planar(_)) => write_opaque_alpha(
            frame,
            format,
            decoded.output_width(),
            decoded.output_height(),
        ),
        (Clip::Alpha, Pixels::Interleaved { color_type, buffer }) => write_alpha(
            frame,
            *color_type,
            decoded.width,
            decoded.height,
            buffer,
            transform,
        ),
    }
}

/// Bytes one frame holds, row padding included.
fn frame_bytes(frame: &VideoFrame) -> usize {
    let planes = frame.get_video_format().num_planes;
    (0..planes)
        .map(|plane| {
            let stride = usize::try_from(frame.stride(plane)).unwrap_or(0);
            let height = usize::try_from(frame.frame_height(plane)).unwrap_or(0);
            stride.saturating_mul(height)
        })
        .sum()
}
