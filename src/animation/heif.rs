//! Avif and heif/heic image sequences.
//!
//! A sequence stores its pictures in a visual track rather than as items, and
//! `libheif` decodes them one at a time through `decode_next_image`. Two things
//! about that path are not what a frame needs, and both are why this module is
//! more than a call:
//!
//! - **the timing is not asked of the decoder.** The embedded libheif 1.23.1
//!   build reports the duration of the sample being fed into the decoder rather
//!   than the one being returned, so every avif frame comes back as the first
//!   frame's duration. The container's own sample table is the normative
//!   statement and is what [`super::sequence`] reads; the same table is also
//!   how the sample count is known at all, because `libheif-rs 3.0.0` exposes
//!   no other way to count a track.
//! - **the presentation crop is not applied.** A track's coded pictures can be
//!   larger than the aperture it presents; the fixture's are 64x64 coded for a
//!   16x12 aperture. `decode_next_image` hands back the coded pixels, so the
//!   aperture is applied here.
//!
//! Alpha is not skipped for a colour-only read, because `libheif-rs 3.0.0`
//! exposes no control over it: decoding a visual frame of a sequence that has a
//! linked alpha track decodes that track too. That is recorded as a known
//! exception to the demand rule rather than worked around; see
//! `docs/improvements/21-animated-images.md`.
//!
//! The decoder is a forward cursor with no seek, so a backward request reopens
//! the file and replays the samples before it. Only the last presentation is
//! held, which is what bounds memory independently of a sequence's length.

use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use libheif_rs::{ColorSpace, HeifContext, Plane, Track};

use crate::{
    decoder::{DecodeTimings, DecodedImage, Pixels, image_error},
    error::{ImgSeqError, Result},
};

use super::sequence::{Crop, Sequence, TrackTiming};
use super::{AnimationDecoder, AnimationSource, Presentation, Rate, SegmentInfo};

/// Describes an avif or heif sequence's timeline without decoding it.
///
/// Returns `None` for a file that is not a sequence, which leaves it on the
/// still-image path it was on before.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is a sequence whose timeline or coded
/// size cannot be read.
pub fn segment_info(
    path: &Path,
    info: crate::decoder::ImageInfo,
    fps: Rate,
    file: &mut BufReader<File>,
) -> Result<Option<SegmentInfo>> {
    let Some(sequence) = super::sequence::read(file, path)? else {
        return Ok(None);
    };
    if sequence.timing.samples() < 2 {
        return Ok(None);
    }
    let presentations = presentations(&sequence.timing, path)?;
    let geometry = Geometry::of(&sequence, path)?;
    let source = std::sync::Arc::new(AnimationSource::new(
        path.to_path_buf(),
        Box::new(HeifSource::new(path, geometry, info.format)?),
    ));
    Ok(Some(SegmentInfo {
        info,
        // The sample table counts ticks of the track's own timescale, and that
        // is the rate the presentations stay in.
        rate: Rate::new(i64::from(sequence.timing.timescale), 1),
        presentations,
        decoder: source,
        fps,
    }))
}

/// Turns a track's sample durations and composition offsets into presentations.
///
/// A sample's presentation time is its decode time -- the sum of the durations
/// before it -- plus its composition offset, and a track that states none is
/// presented where it was decoded. A time that lands before zero is clamped
/// rather than wrapped: a version zero box states its offsets unsigned, and
/// reading one as the difference it stands for is exactly what a subtraction
/// gets wrong.
///
/// Each presentation is held until the next one starts, so the offsets decide
/// the holds as well as the instants; the last is held to the end of the track,
/// which is the decode time after it. A track whose offsets put a sample before
/// the one before it is refused: this reader replays the pictures in the order
/// they are decoded, and it has nowhere to put a presentation that has to be
/// shown before the one it follows.
fn presentations(timing: &TrackTiming, path: &Path) -> Result<Vec<Presentation>> {
    let mut instants = Vec::with_capacity(timing.durations.len());
    let mut decode = 0i64;
    let mut last = 0i64;
    for (index, &duration) in timing.durations.iter().enumerate() {
        let offset = timing.offsets.get(index).copied().unwrap_or(0);
        let instant = decode
            .checked_add(offset)
            .ok_or_else(|| {
                ImgSeqError::new(format!("the timeline of '{}' overflows", path.display()))
            })?
            .max(0);
        if instant < last {
            return Err(ImgSeqError::new(format!(
                "the composition offsets of '{}' put sample {index} before the one before it",
                path.display()
            )));
        }
        last = instant;
        instants.push(instant);
        decode = decode.checked_add(i64::from(duration)).ok_or_else(|| {
            ImgSeqError::new(format!("the timeline of '{}' overflows", path.display()))
        })?;
    }
    // The decode time after the last sample is where the track ends, and that is
    // what holds the last presentation for its own duration.
    let end = decode;
    // An edit list can end the track before its media does: a presentation that
    // starts at or after the end is not shown at all, and the one the end falls
    // inside is held only to it.
    let shown = timing.shown.map_or(end, |ticks| ticks.min(end));
    let mut presentations = Vec::with_capacity(instants.len());
    for (index, &instant) in instants.iter().enumerate() {
        if instant >= shown {
            break;
        }
        let next = instants.get(index + 1).copied().unwrap_or(end).min(shown);
        presentations.push(Presentation {
            timestamp: instant,
            duration: Some(next - instant),
        });
    }
    Ok(presentations)
}

/// How a sequence's coded pictures become the frames it presents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Geometry {
    /// The aperture every presentation is cropped to.
    crop: Crop,
    /// Size of the coded pictures the decoder hands over.
    coded: (u32, u32),
}

impl Geometry {
    fn of(sequence: &Sequence, path: &Path) -> Result<Self> {
        let coded = sequence.coded.ok_or_else(|| {
            ImgSeqError::new(format!(
                "the sequence in '{}' states no coded picture size",
                path.display()
            ))
        })?;
        // A track that states no aperture presents its coded picture whole.
        let crop = sequence.crop.unwrap_or(Crop {
            width: coded.0,
            height: coded.1,
            x: 0,
            y: 0,
        });
        if crop.x.saturating_add(crop.width) > coded.0
            || crop.y.saturating_add(crop.height) > coded.1
        {
            return Err(ImgSeqError::new(format!(
                "the sequence in '{}' presents a {}x{} aperture at {},{} of a {}x{} coded picture",
                path.display(),
                crop.width,
                crop.height,
                crop.x,
                crop.y,
                coded.0,
                coded.1
            )));
        }
        Ok(Self { crop, coded })
    }

    /// Size the frames of this sequence are handed out as.
    fn output(&self) -> (u32, u32) {
        (self.crop.width, self.crop.height)
    }
}

/// One pass over a sequence's visual track, which is a forward cursor.
///
/// A `libheif` track cannot be rewound, so the pass is dropped and rebuilt for
/// a backward request. The pass owns its context as well as its track because a
/// track borrows the context it came from.
struct Pass {
    /// The visual track, which is the cursor.
    ///
    /// It is taken from the context once and kept, because it is stateful:
    /// asking the context for the track again on every sample returns a track
    /// positioned for the next sample anyway, which is neither a seek nor a
    /// restart. Holding it is what makes one pass one forward walk.
    ///
    /// Declaration order matters here. A field is dropped after the fields that
    /// follow it, so the context below outlives this track.
    track: Track,
    /// The context the track was taken from, which has to outlive it.
    ///
    /// It is never read, and it is held only so that the track it lends out
    /// stays valid; the name records that rather than pretending it is unused.
    _context: HeifContext<'static>,
    /// Samples this pass has already produced.
    produced: usize,
}

// SAFETY: a pass is owned by one [`HeifSource`], which the shared
// [`AnimationSource`] reaches only through its own mutex, so a pass is used by
// one thread at a time. What is moved between workers is the source, and it
// holds no pass until a worker asks for a presentation. `libheif`'s own track
// handle is a pointer but is not a shared global: it belongs to the context
// this value also owns.
unsafe impl Send for Pass {}

impl Pass {
    fn open(path: &Path) -> Result<Self> {
        let name = path.to_str().ok_or_else(|| {
            ImgSeqError::new(format!(
                "image path '{}' is not valid utf-8",
                path.display()
            ))
        })?;
        let context =
            HeifContext::read_from_file(name).map_err(|error| image_error("open", path, error))?;
        if !context.has_sequence() {
            return Err(ImgSeqError::new(format!(
                "image '{}' holds no sequence to replay",
                path.display()
            )));
        }
        let id = visual_track(&context).ok_or_else(|| {
            ImgSeqError::new(format!(
                "the sequence in '{}' holds no picture track",
                path.display()
            ))
        })?;
        let track = context.track(id).ok_or_else(|| {
            ImgSeqError::new(format!(
                "the sequence in '{}' holds no track {id}",
                path.display()
            ))
        })?;
        Ok(Self {
            track,
            _context: context,
            produced: 0,
        })
    }

    /// Decodes the next sample of the track in the colour space asked for.
    fn next(&mut self, path: &Path, color_space: ColorSpace) -> Result<libheif_rs::Image> {
        let image = self
            .track
            .decode_next_image(color_space, None)
            .map_err(|error| image_error("decode", path, error))?;
        if image.color_space() != Some(color_space) {
            return Err(image_error(
                "decode",
                path,
                format!(
                    "libheif produced {:?}, not {color_space:?}",
                    image.color_space()
                ),
            ));
        }
        self.produced += 1;
        Ok(image)
    }
}

/// Id of the first track whose handler says it holds pictures.
fn visual_track(context: &HeifContext<'_>) -> Option<u32> {
    context.track_ids().into_iter().find(|id| {
        // The picture track is the one whose handler says so. `track_types`
        // is not reachable by path because the module that holds it is
        // private, but the code itself is stable and is the same one that
        // module names.
        context
            .track(*id)
            .is_some_and(|track| track.handler_type().0 == *b"pict")
    })
}

/// A sequence being replayed, one presentation at a time.
struct HeifSource {
    path: PathBuf,
    geometry: Geometry,
    format: crate::pixel::PixelFormat,
    /// The pass in progress, built on the first presentation and rebuilt after
    /// a backward request.
    pass: Option<Pass>,
    /// How many presentations have been handed out from the current pass.
    next: usize,
}

impl HeifSource {
    fn new(path: &Path, geometry: Geometry, format: crate::pixel::PixelFormat) -> Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            geometry,
            format,
            pass: None,
            next: 0,
        })
    }

    /// The colour space the frame's own format names.
    ///
    /// A colour picture coded as full resolution rgb stays rgb, and one coded
    /// as yuv stays the yuv its planes are; neither is converted into the
    /// other, so the frame a sequence hands out is the picture the track holds
    /// rather than a re-encoding of it.
    fn request(&self) -> Result<ColorSpace> {
        crate::formats::heif::color_space_of(self.format).ok_or_else(|| {
            ImgSeqError::new(format!(
                "image '{}' cannot be decoded as {}",
                self.path.display(),
                self.format.name()
            ))
        })
    }

    /// Decodes the next coded sample and turns it into a presentation.
    fn presentation(&mut self) -> Result<DecodedImage> {
        if self.pass.is_none() {
            self.pass = Some(Pass::open(&self.path)?);
        }
        // The request is read before the pass is borrowed, because the pass is
        // held mutably while it decodes.
        let color_space = self.request()?;
        let pass = self.pass.as_mut().expect("the pass was just opened");
        let image = pass.next(&self.path, color_space)?;
        let coded = (image.width(), image.height());
        if coded != self.geometry.coded {
            return Err(ImgSeqError::new(format!(
                "the sequence in '{}' changed its coded size (was {}x{}, now {}x{})",
                self.path.display(),
                self.geometry.coded.0,
                self.geometry.coded.1,
                coded.0,
                coded.1
            )));
        }
        self.next += 1;
        self.build(&image)
    }

    /// Crops a decoded sample to the aperture and lays its planes out.
    ///
    /// The colour space that is asked for is the one the frame's format names,
    /// so a file whose samples are rgb is handed over as three planes and a file
    /// whose samples are yuv or gray as its luma plane followed by its chroma
    /// ones. The aperture is applied per plane, which is what makes the crop a
    /// view of the presentation rather than of the coded picture.
    fn build(&self, image: &libheif_rs::Image) -> Result<DecodedImage> {
        let (width, height) = self.geometry.output();
        let format = self.format;
        let planes = image.planes();
        let alpha = planes.a.is_some();

        let mut out = Vec::with_capacity(3);
        match format.color_family() {
            vapoursynth4_rs::ColorFamily::RGB => {
                // A planar rgb sample arrives as three full resolution planes,
                // one per channel, in the order the frame holds them.
                for plane in [planes.r.as_ref(), planes.g.as_ref(), planes.b.as_ref()] {
                    let plane = plane.ok_or_else(|| {
                        image_error("decode", &self.path, "the sample holds no rgb plane")
                    })?;
                    out.push(crop_plane(plane, self.geometry, width, height, 0, 0)?);
                }
            }
            family => {
                let luma = planes.y.as_ref().ok_or_else(|| {
                    image_error("decode", &self.path, "the sample holds no colour plane")
                })?;
                out.push(crop_plane(luma, self.geometry, width, height, 0, 0)?);
                if !matches!(family, vapoursynth4_rs::ColorFamily::Gray) {
                    let (sub_w, sub_h) = subsampling(format);
                    let chroma_width = width.div_ceil(sub_w);
                    let chroma_height = height.div_ceil(sub_h);
                    // Each chroma plane is smaller than the luma one, so the
                    // same aperture in luma pixels lands at a fraction of it in
                    // chroma pixels. A 4:2:0 aperture that begins on an odd luma
                    // column would name half a chroma sample, which is refused
                    // rather than rounded.
                    for plane in [planes.cb.as_ref(), planes.cr.as_ref()] {
                        let plane = plane.ok_or_else(|| {
                            image_error("decode", &self.path, "the sample holds no chroma plane")
                        })?;
                        out.push(crop_chroma(
                            plane,
                            self.geometry,
                            chroma_width,
                            chroma_height,
                            sub_w,
                            sub_h,
                            &self.path,
                        )?);
                    }
                }
            }
        }

        let alpha = if alpha {
            let plane = planes.a.as_ref().expect("alpha was reported");
            Some(crop_plane(plane, self.geometry, width, height, 0, 0)?)
        } else {
            None
        };

        Ok(DecodedImage {
            width,
            height,
            format,
            transform: crate::pixel::Transform::IDENTITY,
            pixels: Pixels::Planar { planes: out, alpha },
            timings: DecodeTimings {
                open: std::time::Duration::ZERO,
                metadata: std::time::Duration::ZERO,
                buffer: std::time::Duration::ZERO,
                read: std::time::Duration::ZERO,
            },
        })
    }
}

/// Chroma subsampling of a format, as its horizontal and vertical factors.
///
/// A format that is not subsampled reports ones, and the conversion cannot fail
/// for a factor that is already non-negative.
fn subsampling(format: crate::pixel::PixelFormat) -> (u32, u32) {
    let (horizontal, vertical) = format.sub_sampling();
    (
        u32::try_from(horizontal).unwrap_or(1),
        u32::try_from(vertical).unwrap_or(1),
    )
}

/// Copies one plane's aperture out of a decoded sample.
///
/// The plane is a view over the decoder's own buffer, with a stride that is
/// usually wider than the picture, so rows are copied rather than sliced.
fn crop_plane(
    plane: &Plane<&[u8]>,
    geometry: Geometry,
    width: u32,
    height: u32,
    sub_w: u32,
    sub_h: u32,
) -> Result<Vec<u8>> {
    let sample = usize::from(plane.storage_bits_per_pixel.max(8) / 8);
    let columns = usize::try_from(width).unwrap_or(usize::MAX);
    let rows = usize::try_from(height).unwrap_or(usize::MAX);
    let mut out = vec![0u8; columns.saturating_mul(rows).saturating_mul(sample)];
    let x = usize::try_from(geometry.crop.x / sub_w.max(1)).unwrap_or(usize::MAX);
    let y = usize::try_from(geometry.crop.y / sub_h.max(1)).unwrap_or(usize::MAX);
    let row_bytes = columns.saturating_mul(sample);
    for row in 0..rows {
        let from = (y + row)
            .saturating_mul(plane.stride)
            .saturating_add(x.saturating_mul(sample));
        let to = row.saturating_mul(row_bytes);
        let Some(source) = plane.data.get(from..from.saturating_add(row_bytes)) else {
            return Err(ImgSeqError::new(
                "the decoded sample is smaller than the aperture it states",
            ));
        };
        out[to..to + row_bytes].copy_from_slice(source);
    }
    Ok(out)
}

/// Copies a chroma plane's aperture, refusing one that names half a sample.
#[allow(clippy::too_many_arguments)]
fn crop_chroma(
    plane: &Plane<&[u8]>,
    geometry: Geometry,
    width: u32,
    height: u32,
    sub_w: u32,
    sub_h: u32,
    path: &Path,
) -> Result<Vec<u8>> {
    if !geometry.crop.x.is_multiple_of(sub_w.max(1))
        || !geometry.crop.y.is_multiple_of(sub_h.max(1))
    {
        return Err(ImgSeqError::new(format!(
            "the sequence in '{}' presents a {}x{} aperture at {},{} of a {sub_w}:{sub_h} subsampled picture, which is not on a chroma sample",
            path.display(),
            geometry.crop.width,
            geometry.crop.height,
            geometry.crop.x,
            geometry.crop.y
        )));
    }
    crop_plane(plane, geometry, width, height, sub_w, sub_h)
}

impl AnimationDecoder for HeifSource {
    fn seek(&mut self, index: usize) -> Result<()> {
        if index < self.next {
            // The track is a forward cursor, so the pass is started again and
            // the samples before the target are decoded and dropped.
            self.pass = None;
            self.next = 0;
        }
        Ok(())
    }

    fn next_presentation(&mut self) -> Result<DecodedImage> {
        self.presentation()
    }
}
