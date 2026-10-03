//! Animated image sources: one input path that contributes many frames.
//!
//! A still file contributes exactly one output frame whatever the clip's frame
//! rate is. An animated file contributes its displayed timeline instead: every
//! presentation the container describes is sampled onto the clip's constant
//! frame rate, so the output keeps the timing the file states rather than
//! showing every encoded subframe at the clip rate. See
//! `docs/improvements/21-animated-images.md`.
//!
//! The two halves of that are separated here on purpose:
//!
//! - [`Segment`] and [`SegmentTable`] are the timeline. They describe what one
//!   input path contributes, resolve an output frame number to a path and a
//!   presentation, and do all of it with checked rational integer arithmetic.
//!   Nothing here decodes or allocates.
//! - [`AnimationSource`] is the decoder half. A format adapter implements
//!   [`AnimationDecoder`] and hands over fully composited logical canvases, one
//!   presentation at a time, in timeline order.
//!
//! Keeping the timeline free of decoders is what lets the sampling rules be unit
//! tested against exact boundaries without a fixture per case.

pub mod apng;
pub mod frames;
pub mod heif;
pub mod jxl;
pub mod sequence;

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use crate::{
    decoder::{DecodedImage, ImageInfo},
    error::{ImgSeqError, Result},
};

/// A rate as an exact rational: `num` units per `den` seconds.
///
/// This is the same shape as `fpsnum`/`fpsden`, and it is used for two things
/// that must stay distinguishable:
///
/// - the clip's frame rate, where a unit is one output frame, so `24/1` means
///   twenty-four frames a second;
/// - a container's timeline rate, where a unit is one of its own ticks, so a
///   GIF or WebP that states milliseconds is `1000/1` and one tick is a
///   millisecond.
///
/// One unit is therefore `den / num` seconds, and a presentation a container
/// states as `n` ticks lasts `n * den / num` seconds. Keeping both sides exact
/// is what lets a delay that does not divide the output rate be placed by cross
/// multiplication instead of a rounded millisecond value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rate {
    pub num: i64,
    pub den: i64,
}

impl Rate {
    #[must_use]
    pub const fn new(num: i64, den: i64) -> Self {
        Self { num, den }
    }

    /// The rate of a clip whose frames are `num/den` seconds apart.
    #[must_use]
    pub const fn from_fps(fps_num: i64, fps_den: i64) -> Self {
        Self::new(fps_num, fps_den)
    }
}

/// What a format adapter found in an animated file.
///
/// The adapter knows the file's own timeline and how to replay it; the shared
/// code is what maps that timeline onto the output rate, because the mapping is
/// the same for every format.
pub struct SegmentInfo {
    /// The file-level facts every frame of it carries.
    pub info: crate::decoder::ImageInfo,
    /// The rate the presentation timestamps are counted in.
    pub rate: Rate,
    /// The displayed pictures, in timeline order.
    pub presentations: Vec<Presentation>,
    /// How to replay them.
    pub decoder: Arc<AnimationSource>,
    /// The output clip's rate, which the mapping needs.
    pub fps: Rate,
}

impl SegmentInfo {
    /// Samples this file's timeline onto the output rate.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when the timeline cannot be represented.
    pub fn into_segment(self) -> Result<Segment> {
        Segment::animated(
            self.info,
            self.rate,
            self.presentations,
            self.decoder,
            self.fps,
        )
    }
}

/// One presentation of one segment: when it starts, in the segment's own rate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Presentation {
    /// Ticks from the start of the segment at [`Segment::rate`].
    pub timestamp: i64,
    /// How long this presentation is held, in the same ticks.
    ///
    /// `None` means the file states no delay for it, which the decoders
    /// normalize to a single output tick; see [`Segment::from_presentations`].
    pub duration: Option<i64>,
}

/// One input path's contribution to the output clip.
///
/// A still image is a segment of one presentation; an animation is a segment of
/// as many presentations as it displays. Every output frame of a segment carries
/// the same file-level metadata, which is why the record lives here instead of
/// once per held tick.
#[derive(Clone, Debug)]
pub struct Segment {
    /// The file-level facts every frame of this path carries, including the
    /// orientation it is written with and the ICC profile it states.
    pub info: ImageInfo,
    /// The rate the presentation timestamps are counted in.
    pub rate: Rate,
    /// The displayed pictures, in timeline order, as the container states them.
    pub presentations: Vec<Presentation>,
    /// Whether this path is an animation rather than a single picture.
    pub animated: bool,
    /// The decoder that produces this segment's presentations, which is `None`
    /// for a still, whose one picture is decoded by the file's own path.
    decoder: Option<Arc<AnimationSource>>,
    /// Output frame numbers, one per presentation, relative to the segment.
    ///
    /// A picture held across several output ticks appears once, with the first
    /// tick it is displayed at; a picture that falls entirely between ticks has
    /// no entry.
    samples: Vec<usize>,
    /// Number of output frames this segment contributes.
    count: usize,
}

impl Segment {
    /// A still image: one picture, displayed for one output tick.
    #[must_use]
    pub fn still(info: ImageInfo) -> Self {
        Self {
            info,
            rate: Rate::new(1, 1),
            presentations: vec![Presentation {
                timestamp: 0,
                duration: None,
            }],
            animated: false,
            decoder: None,
            samples: vec![0],
            count: 1,
        }
    }

    /// An animated file and the decoder that replays it.
    ///
    /// `fps` is the output clip's frame rate: the mapping from this file's own
    /// timeline onto the output ticks is decided here, once, so a frame request
    /// only has to index the result.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when a timestamp overflows, when the file states
    /// no presentations at all, or when the expansion does not fit
    /// VapourSynth's frame count.
    pub fn animated(
        info: ImageInfo,
        rate: Rate,
        presentations: Vec<Presentation>,
        decoder: Arc<AnimationSource>,
        fps: Rate,
    ) -> Result<Self> {
        if presentations.is_empty() {
            return Err(ImgSeqError::new(format!(
                "animated image '{}' states no frames",
                info.path.display()
            )));
        }
        if rate.num <= 0 || rate.den <= 0 || fps.num <= 0 || fps.den <= 0 {
            return Err(ImgSeqError::new(format!(
                "animated image '{}' states an invalid rate {}/{}",
                info.path.display(),
                rate.num,
                rate.den
            )));
        }

        // The durations of the presentations make up the segment's timeline.
        // A file that states no delay for a picture, or a delay of zero, would
        // otherwise contribute no time at all and drop every one of its frames
        // but the first; instead it is held for the shortest time the format
        // can express, which is one tick of the output rate. This is the rule
        // every player already applies to a zero-delay GIF or WebP frame.
        //
        // One output tick is `fps.den / fps.num` seconds, and one unit of the
        // segment's rate is `rate.den / rate.num` seconds, so a tick is
        // `fps.den * rate.num / (fps.num * rate.den)` units.
        //
        // The ceiling is what keeps a run of zero-delay pictures from piling
        // onto one output frame. A tick is rarely a whole number of units, and
        // a subsituted duration rounded down is shorter than the tick it stands
        // for, so two pictures could still land on the same tick and one would
        // be dropped. Rounding the substitute up instead makes every
        // consecutive picture start at or after the next tick.
        let one_tick = ceil_div(
            i128::from(rate.num) * i128::from(fps.den),
            i128::from(rate.den) * i128::from(fps.num),
        )
        .max(1);
        let zero_delay = i64::try_from(one_tick).map_err(|_| {
            ImgSeqError::new(format!(
                "the delay of a zero-delay frame of '{}' does not fit this build",
                info.path.display()
            ))
        })?;

        // One unit of the segment's rate is `rate.den / rate.num` seconds, so
        // a duration counted in that rate is `duration * rate.den / rate.num`
        // seconds; multiplying by the output rate then gives
        // `duration * rate.den * fps.num / (rate.num * fps.den)` output ticks.
        // This is the one conversion both the count and the sample instants
        // below are built from.
        let to_frames_num = i128::from(rate.den)
            .checked_mul(i128::from(fps.num))
            .ok_or_else(|| timeline_overflow(&info))?;
        let to_frames_den = i128::from(rate.num)
            .checked_mul(i128::from(fps.den))
            .ok_or_else(|| timeline_overflow(&info))?;

        let mut total = 0i128;
        for presentation in &presentations {
            let duration = match presentation.duration {
                Some(duration) if duration > 0 => i128::from(duration),
                _ => i128::from(zero_delay),
            };
            total = total
                .checked_add(duration)
                .ok_or_else(|| timeline_overflow(&info))?;
        }

        // A segment is active over `[0, total)`, so it covers the output ticks
        // that start before its end and not the one that starts exactly at it.
        // That last tick belongs to whatever comes next, which is what keeps
        // two segments of the same length from both claiming the tick between
        // them: the count is `ceil(total * fps) - 1`.
        //
        // The ticks that start before the segment ends, which is
        // `floor(total * fps)`. The floor at one is what shows a segment
        // shorter than a single output tick: a whole 10 ms animation at 24 fps
        // is `floor(0.24) = 0` ticks strictly before its end and still has to
        // appear once.
        let frames = total
            .checked_mul(to_frames_num)
            .ok_or_else(|| timeline_overflow(&info))?
            .checked_div(to_frames_den)
            .ok_or_else(|| timeline_overflow(&info))?
            .max(1);
        let count = usize::try_from(frames).map_err(|_| {
            ImgSeqError::new(format!(
                "animated image '{}' expands to more frames than this build can count",
                info.path.display()
            ))
        })?;

        // Where each presentation first lands on the output timeline: the
        // first tick at or after the presentation starts, which is a ceiling
        // because a picture that becomes visible part way through a tick is
        // what that tick shows. A presentation that lands on the same tick as
        // the one before it has been held for less than one output frame and is
        // shown by neither tick, which is the documented sampling rule rather
        // than a rounding accident.
        let mut samples = Vec::with_capacity(presentations.len());
        let mut timestamp = 0i128;
        for presentation in &presentations {
            let sample = ceil_div(
                timestamp
                    .checked_mul(to_frames_num)
                    .ok_or_else(|| timeline_overflow(&info))?,
                to_frames_den,
            );
            samples.push(usize::try_from(sample).unwrap_or(usize::MAX).min(count));
            let duration = match presentation.duration {
                Some(duration) if duration > 0 => i128::from(duration),
                _ => i128::from(zero_delay),
            };
            timestamp += duration;
        }

        Ok(Self {
            info,
            rate,
            presentations,
            animated: true,
            decoder: Some(decoder),
            samples,
            count,
        })
    }

    /// Number of output frames this segment contributes.
    #[must_use]
    pub const fn frame_count(&self) -> usize {
        self.count
    }
    /// Width and height every output frame of this segment is written as.
    ///
    /// This is the size the file is handed out as, which a transposing
    /// orientation swaps relative to the size it stores, and is the same for
    /// every output frame of one segment.
    #[must_use]
    pub fn output_size(&self) -> (u32, u32) {
        (self.info.output_width(), self.info.output_height())
    }

    /// Whether two segments are written as the same size and format.
    ///
    /// `mismatch` is what decides whether a clip may hold a mix of them, and
    /// this is the comparison it makes: the size the frames are handed out as,
    /// not the size the files store.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.output_size() == other.output_size() && self.info.format == other.info.format
    }

    /// The decoder for this segment, which only an animation has.
    #[must_use]
    pub fn decoder(&self) -> Option<&Arc<AnimationSource>> {
        self.decoder.as_ref()
    }

    /// Presentation shown at output frame `frame`, relative to this segment.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when `frame` is past the segment.
    pub fn presentation(&self, frame: usize) -> Result<usize> {
        let index = self
            .samples
            .iter()
            .rposition(|&sample| sample <= frame)
            .ok_or_else(|| {
                ImgSeqError::new(format!(
                    "output frame {frame} is before the first presentation of '{}'",
                    self.info.path.display()
                ))
            })?;
        // The answer has to be one of the file's own pictures, and the timeline
        // it was computed from is what says how many there are.
        debug_assert!(index < self.presentations.len());
        Ok(index)
    }
}

/// The output timeline: the segments every listed path contributes, in order.
#[derive(Clone, Debug)]
pub struct SegmentTable {
    segments: Vec<Segment>,
    /// Output frame number each segment starts at, plus the total at the end.
    starts: Vec<usize>,
}

impl SegmentTable {
    /// Builds the table and checks that its expansion fits VapourSynth.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when the total frame count does not fit the
    /// `i32` VapourSynth stores a clip's length in.
    pub fn new(segments: Vec<Segment>) -> Result<Self> {
        let mut starts = Vec::with_capacity(segments.len() + 1);
        let mut total = 0usize;
        starts.push(0);
        for segment in &segments {
            total = total
                .checked_add(segment.frame_count())
                .ok_or_else(|| ImgSeqError::new("the image sequence has too many frames"))?;
            starts.push(total);
        }
        i32::try_from(total).map_err(|_| {
            ImgSeqError::new(format!(
                "the sequence expands to {total} frames, which is more than VapourSynth can hold"
            ))
        })?;
        Ok(Self { segments, starts })
    }

    /// Total number of output frames.
    #[must_use]
    pub fn len(&self) -> usize {
        *self.starts.last().unwrap_or(&0)
    }

    /// How many of the input paths are animations, which is what the debug log
    /// reports about the mapping it just built.
    #[must_use]
    pub fn animated_segments(&self) -> usize {
        self.segments
            .iter()
            .filter(|segment| segment.animated)
            .count()
    }

    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Output frame `frame` and the presentation within it that it samples.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when `frame` is not one of this table's.
    pub fn resolve(&self, frame: usize) -> Result<FrameRef<'_>> {
        let after = self.starts.partition_point(|&start| start <= frame);
        let index = after
            .checked_sub(1)
            .filter(|&index| index < self.segments.len())
            .ok_or_else(|| {
                ImgSeqError::new(format!(
                    "requested frame {frame}, but the clip has {} frames",
                    self.len()
                ))
            })?;
        let segment = &self.segments[index];
        let presentation = segment.presentation(frame - self.starts[index])?;
        Ok(FrameRef {
            segment,
            presentation,
            local: frame - self.starts[index],
        })
    }

    /// The segment record of one output frame, without resolving the
    /// presentation. Used to size lookahead and to report properties.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when `frame` is not one of this table's.
    pub fn segment_of(&self, frame: usize) -> Result<&Segment> {
        Ok(self.resolve(frame)?.segment)
    }
}

/// One resolved output frame: the segment it comes from and its presentation.
#[derive(Clone, Copy, Debug)]
pub struct FrameRef<'a> {
    pub segment: &'a Segment,
    /// Index into [`Segment::presentations`].
    pub presentation: usize,
    /// Output frame number relative to the segment.
    pub local: usize,
}

/// Where a segment's presentations come from.
///
/// An adapter replays a file's own timeline. Every call returns a fully
/// composited logical canvas: the format's blend and disposal operations are
/// applied before the pixels are handed over, so this is the only place that
/// has to know about them.
pub trait AnimationDecoder: Send {
    /// Prepares the decoder to return presentation `index` on the next
    /// [`Self::next_presentation`] call.
    ///
    /// A backward step may replay the file from its beginning; the adapter is
    /// free to keep whatever checkpoint state its format offers.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when the file cannot be seeked.
    fn seek(&mut self, index: usize) -> Result<()>;

    /// Decodes the next presentation in timeline order.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when the picture cannot be decoded.
    fn next_presentation(&mut self) -> Result<DecodedImage>;
}

/// A segment's decoder, shared by every worker that reads that segment.
///
/// The mutex is what keeps one animation's presentations in timeline order:
/// two workers that ask for frames of the same file take turns rather than
/// replaying it twice. Workers on different files hold different mutexes and
/// still decode concurrently.
pub struct AnimationSource {
    path: PathBuf,
    cursor: Mutex<Cursor>,
}

/// Number of decoded presentations a source keeps.
///
/// One animation's output frames are decoded by whichever lookahead worker
/// reaches them, so the worker that is ahead of the consumer is not the worker
/// that asked for a frame. Keeping a window of them means a request that is
/// behind the cursor is answered instead of replayed, and it bounds the memory
/// this holds to that window times one presentation, which is what
/// `docs/improvements/21-animated-images.md` asks for. The lookahead window
/// itself is capped at sixteen output frames, so this covers it with room for
/// two clips of one call asking for the same frames.
const PRESENTATION_CACHE: usize = 48;

struct Cursor {
    decoder: Box<dyn AnimationDecoder>,
    /// Presentation the next [`Self::presentation`] call will decode.
    next: usize,
    /// Decoded presentations, keyed by their index.
    ///
    /// The start and the end of this are what is dropped when it grows past
    /// [`PRESENTATION_CACHE`], because a request is always near the cursor.
    cached: BTreeMap<usize, DecodedImage>,
}

impl AnimationSource {
    #[must_use]
    pub fn new(path: PathBuf, decoder: Box<dyn AnimationDecoder>) -> Self {
        Self {
            path,
            cursor: Mutex::new(Cursor {
                decoder,
                next: 0,
                cached: BTreeMap::new(),
            }),
        }
    }

    /// Returns presentation `index`, composited onto the logical canvas.
    ///
    /// A request for the presentation that was decoded last, which is what
    /// every held output frame asks for, is answered from the cache. A forward
    /// request advances the cursor through the presentations in between; a
    /// backward one asks the adapter to seek.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when the file cannot be decoded or seeked.
    pub fn presentation(&self, index: usize) -> Result<DecodedImage> {
        let mut cursor = self
            .cursor
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(image) = cursor.cached.get(&index) {
            return Ok(image.clone());
        }

        // The cursor decodes strictly forward from the presentation it produced
        // last, so a request for anything after that walks to it and a request
        // for anything before it starts the pass again. What was decoded stays
        // cached, so a lookahead worker that has moved the cursor past the
        // consumer's request does not force a replay of it.
        if index < cursor.next {
            cursor.decoder.seek(index)?;
            cursor.next = index;
        }
        while cursor.next <= index {
            let produced = cursor.next;
            let image = cursor.decoder.next_presentation()?;
            cursor.next = produced.saturating_add(1);
            cursor.cached.insert(produced, image);
            trim(&mut cursor);
        }
        cursor.cached.get(&index).cloned().ok_or_else(|| {
            ImgSeqError::new(format!(
                "animated image '{}' holds no presentation {index}",
                self.path.display()
            ))
        })
    }
}

/// Drops the presentations furthest from the cursor until the cache fits.
///
/// The ones nearest the request are what a running graph asks for again, so the
/// two ends go first: a seek moves the cursor back and the entries above it are
/// the ones that would have to be decoded again anyway.
fn trim(cursor: &mut Cursor) {
    while cursor.cached.len() > PRESENTATION_CACHE {
        let Some(first) = cursor.cached.keys().next().copied() else {
            break;
        };
        let Some(last) = cursor.cached.keys().next_back().copied() else {
            break;
        };
        // Keep the end the cursor is walking towards.
        if last < cursor.next || first >= cursor.next {
            cursor.cached.remove(&first);
        } else {
            cursor.cached.remove(&last);
        }
    }
}

impl std::fmt::Debug for AnimationSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnimationSource")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Error for a timeline whose arithmetic does not fit the intermediate type.
///
/// A file states its own durations, so a malformed one can name a delay no
/// checked operation accepts; that is refused rather than wrapping into a
/// plausible-looking shorter timeline.
fn timeline_overflow(info: &ImageInfo) -> ImgSeqError {
    ImgSeqError::new(format!(
        "the timeline of '{}' overflows",
        info.path.display()
    ))
}

/// Ceiling division for a non-negative numerator and a positive denominator.
///
/// This is the "first tick at or after an instant" mapping: a picture that
/// becomes visible part way through a tick is what that tick shows, so its
/// instant rounds forward to it.
fn ceil_div(numerator: i128, denominator: i128) -> i128 {
    debug_assert!(numerator >= 0, "an instant on a timeline is not negative");
    debug_assert!(denominator > 0, "the divisor is a rate");
    let quotient = numerator / denominator;
    if numerator % denominator == 0 {
        quotient
    } else {
        quotient + 1
    }
}

#[cfg(test)]
mod tests {
    use super::{AnimationSource, Presentation, Rate, Segment, SegmentTable};
    use crate::decoder::ImageInfo;
    use crate::layout::{ColorType, Orientation, SourceColorType};
    use crate::pixel::{PixelFormat, Transform};
    use std::path::PathBuf;

    /// A segment record without a decoder, for the timeline tests.
    fn info(name: &str) -> ImageInfo {
        ImageInfo {
            path: PathBuf::from(name),
            width: 4,
            height: 4,
            color_type: ColorType::Rgba8,
            original_color_type: SourceColorType::Rgba8,
            has_icc_profile: false,
            icc_profile: None,
            cicp: None,
            chroma_location: None,
            orientation: Orientation::NoTransforms,
            transform: Transform::IDENTITY,
            format: PixelFormat::Rgb8,
        }
    }

    /// An adapter that never decodes, so a timeline can be built in a test.
    struct NoDecoder;

    impl super::AnimationDecoder for NoDecoder {
        fn seek(&mut self, _index: usize) -> crate::error::Result<()> {
            Ok(())
        }

        fn next_presentation(&mut self) -> crate::error::Result<crate::decoder::DecodedImage> {
            unreachable!("the timeline tests never decode")
        }
    }

    /// Builds an animated segment whose rate is `rate` and whose presentations
    /// hold for `durations` of that rate.
    fn animated_with(durations: &[i64], rate: (i64, i64), fps: (i64, i64)) -> Segment {
        let presentations = durations
            .iter()
            .map(|&duration| Presentation {
                timestamp: 0,
                duration: Some(duration),
            })
            .collect();
        let source = AnimationSource::new(PathBuf::from("a.gif"), Box::new(NoDecoder));
        Segment::animated(
            info("a.gif"),
            Rate::new(rate.0, rate.1),
            presentations,
            std::sync::Arc::new(source),
            Rate::from_fps(fps.0, fps.1),
        )
        .expect("the timeline is valid")
    }

    /// A segment whose rate counts milliseconds, which is what every GIF,
    /// APNG, WebP and HEIC fixture states: a rate of `1000/1`.
    fn animated(durations_ms: &[i64], fps: (i64, i64)) -> Segment {
        animated_with(durations_ms, (1000, 1), fps)
    }

    /// An independent model of the documented sampling rule.
    ///
    /// A segment is active over the half-open interval `[0, total)` in seconds,
    /// output frame `k` is the instant `k * fps_den / fps_num` seconds, and a
    /// presentation shown over `[start, end)` is what every tick strictly
    /// inside that interval displays. Deriving the expected answer from those
    /// definitions rather than from the implementation is what lets these tests
    /// catch the implementation being wrong; a model that repeated the
    /// implementation's arithmetic would agree with any bug in it.
    struct Model {
        ticks: Vec<usize>,
        frames: usize,
    }

    fn model(durations: &[i64], rate: (i64, i64), fps: (i64, i64)) -> Model {
        let (rate_num, rate_den) = (i128::from(rate.0), i128::from(rate.1));
        let (fps_num, fps_den) = (i128::from(fps.0), i128::from(fps.1));
        // One unit of the segment's rate is `rate_den / rate_num` seconds, so
        // a duration counted in that rate is `duration * rate_den / rate_num`
        // seconds; scaling it by the output rate gives
        // `duration * rate_den * fps_num / (rate_num * fps_den)` ticks, and the
        // two constants below are that fraction.
        let units_per_tick = rate_num * fps_den;
        let tick_scale = rate_den * fps_num;

        let mut ticks = Vec::new();
        let mut total = 0i128;
        for &duration in durations {
            // The first tick at or after the presentation's start instant.
            let numerator = total * tick_scale;
            let quotient = numerator / units_per_tick;
            let tick = if numerator % units_per_tick == 0 {
                quotient
            } else {
                quotient + 1
            };
            eprintln!(
                "  iter total={total} duration={duration} numerator={numerator} quotient={quotient} rem={} tick={tick}",
                numerator % units_per_tick
            );
            ticks.push(usize::try_from(tick).expect("the test rates are small"));
            total += i128::from(duration);
        }
        // The ticks that start before the segment ends, floored at one.
        let frames = usize::try_from((total * tick_scale) / units_per_tick)
            .expect("the test timelines are small")
            .max(1);
        eprintln!(
            "MODEL in durations={durations:?} rate={rate:?} fps={fps:?} | rate_num={rate_num} rate_den={rate_den} fps_num={fps_num} fps_den={fps_den} units_per_tick={units_per_tick} tick_scale={tick_scale} ticks={ticks:?} frames={frames}"
        );
        eprintln!(
            "MODEL rate={rate:?} fps={fps:?} rn={rate_num} rd={rate_den} fn={fps_num} fd={fps_den} upt={units_per_tick} scale={tick_scale} ticks={ticks:?} frames={frames}"
        );
        Model { ticks, frames }
    }

    /// The duration a zero-delay picture is held for, in the units of `rate`:
    /// one output tick, rounded up so consecutive pictures cannot share a tick.
    fn tick_substitute(rate: (i64, i64), fps: (i64, i64)) -> i64 {
        let (rate_num, rate_den) = (i128::from(rate.0), i128::from(rate.1));
        let (fps_num, fps_den) = (i128::from(fps.0), i128::from(fps.1));
        // seconds per tick = fps_den / fps_num; units per second = rate_num / rate_den
        let numerator = fps_den * rate_num;
        let denominator = fps_num * rate_den;
        i64::try_from(super::ceil_div(numerator, denominator)).expect("the test rates are small")
    }

    /// Asserts that `segment` displays exactly what the model says.
    fn assert_timeline(segment: &Segment, durations: &[i64], rate: (i64, i64), fps: (i64, i64)) {
        let model = model(durations, rate, fps);
        assert_eq!(
            segment.frame_count(),
            model.frames,
            "frame count for {durations:?} at {rate:?} onto {fps:?}, ticks {:?}",
            model.ticks
        );
        for frame in 0..model.frames {
            let expected = model
                .ticks
                .iter()
                .rposition(|&tick| tick <= frame)
                .unwrap_or_else(|| panic!("frame {frame} is before every presentation"));
            assert_eq!(
                segment.presentation(frame).unwrap(),
                expected,
                "frame {frame} of {} with ticks {:?}",
                model.frames,
                model.ticks
            );
        }
    }

    #[test]
    fn a_still_is_one_frame_at_any_rate() {
        for fps in [(24, 1), (24000, 1001), (1, 1), (1000, 1)] {
            let segment = Segment::still(info("a.png"));
            assert_eq!(segment.frame_count(), 1, "{fps:?}");
            assert_eq!(segment.presentation(0).unwrap(), 0);
        }
    }

    #[test]
    fn a_timeline_is_sampled_onto_the_output_rate() {
        let durations = [80, 170, 110, 240];
        let segment = animated(&durations, (24, 1));
        // 600 ms at 24 fps is 14.4, so the segment covers the 14 ticks that
        // start before it ends, and the pictures start at ticks 0, 2, 6 and 9.
        // 600 ms at 24 fps ends 14.4 ticks in, so 14 ticks start before it.
        assert_eq!(model(&durations, (1000, 1), (24, 1)).frames, 14);
        assert_eq!(model(&durations, (1000, 1), (24, 1)).ticks, [0, 2, 6, 9]);
        assert_eq!(segment.frame_count(), 14);
        assert_timeline(&segment, &durations, (1000, 1), (24, 1));
    }

    #[test]
    fn the_boundary_tick_belongs_to_the_segment_after_it() {
        // Two 100 ms animations at 10 fps are one output frame each, and the
        // tick that starts where the first one ends is the second one's frame
        // rather than a second frame of the first.
        let table = SegmentTable::new(vec![animated(&[100], (10, 1)), animated(&[100], (10, 1))])
            .expect("the table is valid");
        assert_eq!(table.len(), 2);
        assert_eq!(table.resolve(0).unwrap().local, 0);
        assert_eq!(table.resolve(1).unwrap().local, 0);
        assert_eq!(table.segment_of(1).unwrap().presentations.len(), 1);
    }

    #[test]
    fn an_exact_rate_match_shows_every_picture() {
        // Four 100 ms pictures at 10 fps are exactly four output frames, one
        // per picture.
        let durations = [100, 100, 100, 100];
        let segment = animated(&durations, (10, 1));
        assert_timeline(&segment, &durations, (1000, 1), (10, 1));
        for frame in 0..4 {
            assert_eq!(segment.presentation(frame).unwrap(), frame, "frame {frame}");
        }
    }

    #[test]
    fn a_delay_shorter_than_a_tick_is_omitted() {
        // Four 10 ms pictures at 24 fps: the whole 40 ms is less than one
        // output tick, so only the picture active at tick 0 is shown.
        let durations = [10, 10, 10, 10];
        let segment = animated(&durations, (24, 1));
        assert_eq!(model(&durations, (1000, 1), (24, 1)).frames, 1);
        assert_eq!(segment.frame_count(), 1);
        assert_timeline(&segment, &durations, (1000, 1), (24, 1));
        assert_eq!(segment.presentation(0).unwrap(), 0);
    }

    #[test]
    fn a_delay_longer_than_a_tick_holds_one_picture() {
        // One 1000 ms picture at 24 fps is exactly 24 ticks long, so the 24
        // ticks that start before its end are 0..=23, and the tick at 1 s
        // belongs to whatever comes next.
        let durations = [1000];
        let segment = animated(&durations, (24, 1));
        assert_eq!(model(&durations, (1000, 1), (24, 1)).frames, 24);
        assert_eq!(segment.frame_count(), 24);
        assert_timeline(&segment, &durations, (1000, 1), (24, 1));
    }

    #[test]
    fn a_zero_delay_is_held_for_one_output_tick() {
        // A file whose decoder states no delay would otherwise contribute one
        // frame; every zero-delay picture is instead held one tick, so all four
        // are shown at 24 fps. One tick is 41 ms at 24 fps, so the four
        // pictures start at 0, 41, 82 and 123 ms, which the model then maps
        // onto ticks 0..3.
        let segment = animated(&[0, 0, 0, 0], (24, 1));
        // One output tick at 24 fps is 1000/24 ms, which is not a whole number
        // of milliseconds, so the substitute has to round up to 42 ms;
        // rounding it down to 41 would put two of the four pictures on the same
        // output tick.
        assert_eq!(tick_substitute((1000, 1), (24, 1)), 42);
        let substitute = [42, 42, 42, 42];
        assert_timeline(&segment, &substitute, (1000, 1), (24, 1));
        assert_eq!(segment.frame_count(), 4);
    }

    #[test]
    fn a_fractional_rate_maps_exactly() {
        // Four 100 ms pictures at 30000/1001 fps. The pictures start at 0,
        // 100, 250 and 360 ms, which are 0, 2.997, 5.994 and 8.991 ticks, so
        // they are shown at ticks 0, 3, 6 and 9. The 400 ms timeline ends 11.99
        // ticks in, so 11 ticks start before it.
        let durations = [100, 100, 100, 100];
        let segment = animated(&durations, (30000, 1001));
        assert_eq!(model(&durations, (1000, 1), (30000, 1001)).frames, 11);
        assert_eq!(
            model(&durations, (1000, 1), (30000, 1001)).ticks,
            [0, 3, 6, 9]
        );
        assert_eq!(segment.frame_count(), 11);
        assert_timeline(&segment, &durations, (1000, 1), (30000, 1001));
    }

    #[test]
    fn an_arbitrary_delay_denominator_stays_exact() {
        // A container whose rate is `3/1` means three ticks a second, so one
        // tick is a third of a second: a spelling no millisecond timeline can
        // represent. Three such pictures are one second, which covers the six
        // ticks that start before it ends at 6 fps, and each picture is shown
        // at ticks 0, 2 and 4.
        let third = Presentation {
            timestamp: 0,
            duration: Some(1),
        };
        let source = AnimationSource::new(PathBuf::from("a.apng"), Box::new(NoDecoder));
        let segment = Segment::animated(
            info("a.apng"),
            Rate::new(3, 1),
            vec![third, third, third],
            std::sync::Arc::new(source),
            Rate::from_fps(6, 1),
        )
        .expect("the timeline is valid");
        assert_eq!(model(&[1, 1, 1], (3, 1), (6, 1)).frames, 6);
        assert_eq!(model(&[1, 1, 1], (3, 1), (6, 1)).ticks, [0, 2, 4]);
        assert_timeline(&segment, &[1, 1, 1], (3, 1), (6, 1));
        assert_eq!(segment.frame_count(), 6);
    }

    #[test]
    fn the_table_orders_stills_and_animations() {
        let table = SegmentTable::new(vec![
            Segment::still(info("first.png")),
            animated(&[100, 100, 100, 100], (10, 1)),
            Segment::still(info("last.png")),
        ])
        .expect("the table is valid");
        assert_eq!(table.len(), 6);
        assert_eq!(
            table.segment_of(0).unwrap().info.path,
            PathBuf::from("first.png")
        );
        assert_eq!(
            table.segment_of(1).unwrap().info.path,
            PathBuf::from("a.gif")
        );
        assert_eq!(
            table.segment_of(4).unwrap().info.path,
            PathBuf::from("a.gif")
        );
        assert_eq!(
            table.segment_of(5).unwrap().info.path,
            PathBuf::from("last.png")
        );
        let resolved = table.resolve(3).unwrap();
        assert_eq!(resolved.presentation, 2);
        assert_eq!(resolved.local, 2);
        assert_eq!(table.animated_segments(), 1);
        assert!(table.resolve(6).is_err());
    }

    #[test]
    fn a_still_only_table_stays_unchanged() {
        let table = SegmentTable::new(vec![
            Segment::still(info("a.png")),
            Segment::still(info("b.png")),
        ])
        .expect("the table is valid");
        assert_eq!(table.len(), 2);
        assert_eq!(table.animated_segments(), 0);
        for frame in 0..2 {
            let resolved = table.resolve(frame).unwrap();
            assert_eq!(resolved.presentation, 0);
            assert_eq!(resolved.local, 0);
        }
    }

    #[test]
    fn a_sequence_too_long_for_vapoursynth_is_refused() {
        // One millisecond of animation at 2^41 frames a second is about 2.2e9
        // output frames, which is past the count VapourSynth stores in a clip.
        // The segment itself is a handful of bytes; only the count is refused,
        // because frames are built when they are asked for and never in
        // advance.
        let segment = animated(&[1], (1 << 41, 1));
        assert!(segment.frame_count() > i32::MAX as usize);
        assert!(
            SegmentTable::new(vec![segment]).is_err(),
            "a clip longer than i32::MAX frames is refused"
        );
    }
}
