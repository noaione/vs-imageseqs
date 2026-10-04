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
    animation::{AnimationSource, SegmentTable},
    color::set_frame_properties,
    decoder::{self, DecodeTimings, DecodedImage, Demand, ImageInfo, Pixels, PlaneRows, RowSink},
    error::{ImgSeqError, Result},
    pixel::{
        PixelFormat, PlaneSource, WriteTimings, write_alpha, write_decoded_planes,
        write_opaque_alpha, write_planar,
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

/// What a call that hands out `clips` has to decode.
///
/// The alpha plane is asked for exactly when one of the clips is the alpha clip,
/// and this reads the same list the frames are built from, so a decode and the
/// payload built from it cannot disagree about whether the alpha plane is there;
/// see [`crate::decoder::Demand`].
#[must_use]
pub fn demand_of(clips: &[Clip]) -> Demand {
    if clips.contains(&Clip::Alpha) {
        Demand::ALL
    } else {
        Demand::COLOR
    }
}

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

    /// Builds every frame of `clips` from one decoded presentation.
    ///
    /// The frames are allocated first and filled afterwards, which is what lets
    /// a format that hands each decoded row to the frame it belongs in fill all
    /// of them in one pass; see [`crate::decoder::RowStream`]. A buffered
    /// decode is written one clip at a time, as it always was.
    fn build(
        core: &Core,
        clips: &Arc<[Clip]>,
        image: &ImageInfo,
        index: usize,
        mut decoded: DecodedImage,
        export_icc_profile: bool,
    ) -> Result<Self> {
        let mut frames = Vec::with_capacity(clips.len());
        let mut timings = Vec::with_capacity(clips.len());
        let mut bytes: usize = 0;
        // A rotation may have swapped the stored size, and the frames are the
        // size the pixels are written as rather than the size the file holds.
        let (output_width, output_height) = (decoded.output_width(), decoded.output_height());
        for &clip in clips.iter() {
            let format = clip.pixel_format(decoded.format);
            let allocate_started = Instant::now();
            let frame = new_frame(core, format, output_width, output_height)?;
            let allocate = allocate_started.elapsed();
            frames.push(frame);
            timings.push(FrameTimings {
                allocate,
                write: WriteTimings::default(),
                properties: Duration::ZERO,
            });
        }

        let decode = match &mut decoded.pixels {
            Pixels::Stream(stream) => {
                let width = usize::try_from(output_width)
                    .map_err(|_| ImgSeqError::new("image width does not fit this platform"))?;
                let height = usize::try_from(output_height)
                    .map_err(|_| ImgSeqError::new("image height does not fit this platform"))?;
                // A file that states no alpha leaves the alpha plane to the
                // opaque value, and a frame arrives holding whatever the
                // allocator had. A file that does state one overwrites every
                // byte of the plane, so this only runs when it does not.
                if !stream.has_alpha()
                    && let Some(index) = clips.iter().position(|&clip| clip == Clip::Alpha)
                {
                    let format = Clip::Alpha.pixel_format(decoded.format);
                    timings[index].write = write_opaque_alpha(
                        &mut frames[index],
                        format,
                        output_width,
                        output_height,
                    )?;
                }
                let sink = row_sink(&mut frames, clips, decoded.format, width, height)?;
                stream.fill(sink)?
            }
            _ => {
                for (frame, &clip) in frames.iter_mut().zip(clips.iter()) {
                    let format = clip.pixel_format(decoded.format);
                    let write = write_frame(frame, clip, format, &decoded)?;
                    let slot = clips
                        .iter()
                        .position(|&candidate| candidate == clip)
                        .expect("the clips are the frames' own list");
                    timings[slot].write = write;
                }
                decoded.timings
            }
        };

        for ((frame, &clip), timing) in frames.iter_mut().zip(clips.iter()).zip(timings.iter_mut())
        {
            let format = clip.pixel_format(decoded.format);
            let properties_started = Instant::now();
            set_frame_properties(
                frame,
                image,
                index,
                format,
                clip.alpha_marker(),
                export_icc_profile,
            )?;
            timing.properties = properties_started.elapsed();
            bytes = bytes.saturating_add(frame_bytes(frame));
        }

        Ok(Self {
            clips: Arc::clone(clips),
            frames: frames.into_boxed_slice(),
            timings: timings.into_boxed_slice(),
            decode,
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

/// Builds the frames of every clip of one call, on the worker that produced the
/// image.
///
/// One output frame is not always one file, so the builder holds the segment
/// table that says which file a frame index names, and which presentation of
/// it. A still decodes through the same path it always did; an animated source
/// is asked for one composited presentation.
pub struct FrameBuilder {
    core: SharedCore,
    clips: Arc<[Clip]>,
    segments: Arc<SegmentTable>,
    export_icc_profile: bool,
}

impl FrameBuilder {
    /// Builder of the clips of one call, owned by the filter built with `core`.
    ///
    /// The builder must stay inside the filter that was created with this core:
    /// a filter is freed before its core, so a handle kept by a filter cannot
    /// outlive what it points at.
    #[must_use]
    pub fn new(
        core: CoreRef<'_>,
        clips: &[Clip],
        segments: Arc<SegmentTable>,
        export_icc_profile: bool,
    ) -> Self {
        // SAFETY: the filter built from this core owns the builder, so the
        // handle cannot outlive the core. `SharedCore` records why the workers
        // that use it may do so.
        let core =
            SharedCore(unsafe { std::mem::transmute::<CoreRef<'_>, CoreRef<'static>>(core) });
        Self {
            core,
            clips: Arc::from(clips),
            segments,
            export_icc_profile,
        }
    }
}

impl Prepare for FrameBuilder {
    type Payload = ClipFrames;

    fn frames(&self) -> usize {
        self.segments.len()
    }

    fn demand(&self) -> Demand {
        demand_of(&self.clips)
    }

    fn estimate(&self, index: usize) -> usize {
        self.segments
            .segment_of(index)
            .map_or(0, |segment| expected_bytes(&self.clips, &segment.info))
    }

    fn produce(&self, index: usize) -> Result<Self::Payload> {
        let frame = self.segments.resolve(index)?;
        let image = &frame.segment.info;
        let decoded = decode_frame(
            image,
            frame.segment.decoder(),
            frame.presentation,
            self.demand(),
        )?;
        ClipFrames::build(
            self.core.core(),
            &self.clips,
            image,
            index,
            decoded,
            self.export_icc_profile,
        )
    }
}

/// Decodes one presentation of one segment.
///
/// A still goes through the file's own decoder exactly as it did before
/// animations existed, and an animated file through the decoder that owns its
/// timeline, which hands back a fully composited logical canvas either way.
fn decode_frame(
    image: &ImageInfo,
    decoder: Option<&Arc<AnimationSource>>,
    presentation: usize,
    demand: Demand,
) -> Result<DecodedImage> {
    match decoder {
        Some(source) => source.presentation(presentation),
        None => decoder::decode(image, demand),
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

/// The planes a streaming decode writes into, taken from the frames it fills.
///
/// The rows land where the frame holds them rather than in a buffer the caller
/// would have to copy afterwards, which is the whole point of a stream: see
/// [`crate::decoder::RowStream`].
fn row_sink<'a>(
    frames: &'a mut [VideoFrame],
    clips: &[Clip],
    format: PixelFormat,
    width: usize,
    height: usize,
) -> Result<RowSink<'a>> {
    let mut colour = Vec::new();
    let mut alpha = None;
    for (frame, &clip) in frames.iter_mut().zip(clips.iter()) {
        let planes = plane_rows(frame, clip.pixel_format(format), width, height)?;
        match clip {
            Clip::Color => colour = planes,
            Clip::Alpha => alpha = Some(planes),
        }
    }
    Ok(RowSink { colour, alpha })
}

/// Every plane of one frame, as rows a decoder can write.
///
/// The plane a frame holds is at least `stride * height` bytes and its active
/// rows are `row_bytes` of each of them, which is the same pair
/// [`crate::pixel`] checks before it writes a plane itself.
fn plane_rows(
    frame: &mut VideoFrame,
    format: PixelFormat,
    width: usize,
    height: usize,
) -> Result<Vec<PlaneRows<'_>>> {
    let planes = format.plane_count();
    let mut out = Vec::with_capacity(planes);
    for plane in 0..planes {
        let index = i32::try_from(plane).expect("the plane count fits in i32");
        let pointer = frame.plane_mut(index);
        if pointer.is_null() {
            return Err(ImgSeqError::new(format!(
                "VapourSynth returned a null pointer for plane {plane}"
            )));
        }
        let rows = usize::try_from(frame.frame_height(index))
            .map_err(|_| ImgSeqError::new("a frame plane height does not fit this platform"))?;
        let stride = usize::try_from(frame.stride(index))
            .map_err(|_| ImgSeqError::new("a frame plane stride does not fit this platform"))?;
        let (plane_width, _) = format.frame_plane_dimensions(plane, width, height);
        let row_bytes = plane_width.saturating_mul(format.bytes_per_sample());
        if stride < row_bytes {
            return Err(ImgSeqError::new(format!(
                "VapourSynth plane {plane} stride {stride} is smaller than row size {row_bytes}"
            )));
        }
        // SAFETY: a VapourSynth frame owns at least `stride * height` bytes for
        // each of its planes, which is the length taken here, and the frame is
        // borrowed mutably for as long as the slice lives.
        let length = stride.saturating_mul(rows);
        let bytes = unsafe { std::slice::from_raw_parts_mut(pointer, length) };
        out.push(PlaneRows {
            bytes,
            stride,
            row_bytes,
        });
    }
    Ok(out)
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
        (Clip::Color, Pixels::Planar { planes, .. }) => write_decoded_planes(
            frame,
            decoded.format,
            decoded.width,
            decoded.height,
            PlaneSource::Packed(planes),
            transform,
        ),
        // A decode that kept the decoder's own plane-major buffer is written out
        // of that buffer rather than out of a copy of it, so this is the same
        // write with the decoder's own strides attached.
        (
            Clip::Color,
            Pixels::Strided {
                planes,
                buffer,
                row_stride,
                plane_stride,
            },
        ) => write_decoded_planes(
            frame,
            decoded.format,
            decoded.width,
            decoded.height,
            PlaneSource::Strided {
                buffer,
                planes: *planes,
                row_stride: *row_stride,
                plane_stride: *plane_stride,
            },
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
        // The alpha plane of a planar decode is a plane of its own, and the
        // frame it is written into is the gray format of the same depth, which
        // already has the geometry of the image rather than of its chroma.
        (
            Clip::Alpha,
            Pixels::Planar {
                alpha: Some(alpha), ..
            },
        ) => write_decoded_planes(
            frame,
            format,
            decoded.width,
            decoded.height,
            PlaneSource::Packed(std::slice::from_ref(alpha)),
            transform,
        ),
        (Clip::Alpha, Pixels::Planar { alpha: None, .. }) => write_opaque_alpha(
            frame,
            format,
            decoded.output_width(),
            decoded.output_height(),
        ),
        // A decode that kept the decoder's buffer states no alpha of its own,
        // because every channel of that picture is a plane of the buffer.
        (Clip::Alpha, Pixels::Strided { .. }) => write_opaque_alpha(
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
        // A stream fills the frames itself, so nothing reaches this arm: the
        // builder routes one before a clip is written from pixels.
        (_, Pixels::Stream(_)) => Err(ImgSeqError::new("a streaming decode fills its own frames")),
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

#[cfg(test)]
mod tests {
    use super::{Clip, Demand, READ_ALPHA_CLIPS, READ_CLIPS, demand_of};

    #[test]
    fn only_a_call_that_hands_out_alpha_asks_for_it() {
        assert_eq!(demand_of(READ_CLIPS), Demand::COLOR);
        assert_eq!(demand_of(READ_ALPHA_CLIPS), Demand::ALL);
        // A call that hands out both clips asks for everything, whatever order
        // the clips are in.
        assert_eq!(demand_of(&[Clip::Alpha, Clip::Color]), Demand::ALL);
        assert_eq!(demand_of(&[]), Demand::COLOR);
    }
}
