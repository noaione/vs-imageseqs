use std::{
    ffi::{CStr, CString, c_void},
    fmt::Display,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use vapoursynth4_rs::{
    ColorFamily, SampleType, VideoInfo,
    core::CoreRef,
    ffi,
    frame::{FrameContext, VideoFrame},
    key,
    map::{AppendMode, MapPropertyError, MapRef},
    node::{Dependencies, Filter},
};

use crate::{
    clip::{Clip, FrameBuilder, READ_ALPHA_CLIPS, READ_CLIPS, query_format},
    decoder::{self, ImageInfo},
    error::{ImgSeqError, Result},
    pixel::PixelFormat,
    prefetch::{self, Prefetcher},
};

/// Arguments accepted by `Read` and `ReadAlpha`.
const SEQUENCE_ARGS: &CStr = c"files:data[];fpsnum:int:opt;fpsden:int:opt;mismatch:int:opt;apply_rotation:int:opt;debug:int:opt;prefetch:int:opt;prefetch_memory:int:opt;";

/// `Read` filter instance: the color clip of one image sequence.
pub struct Read {
    sequence: Arc<Sequence>,
}

/// `ReadAlpha` filter instance: the alpha clip of one image sequence.
///
/// The color clip of the same call is built from the same [`Sequence`], so a
/// file is decoded once no matter how many clips ask for it.
pub struct ReadAlpha {
    sequence: Arc<Sequence>,
}

/// State shared by every clip of one `Read` or `ReadAlpha` call.
struct Sequence {
    images: Arc<[ImageInfo]>,
    /// Holds finished frames, so a request never decodes or writes pixels.
    prefetcher: Arc<Prefetcher<FrameBuilder>>,
    debug: bool,
}

/// Timings of the work done while one instance is created.
struct SetupTimings {
    probe: Duration,
    validate: Duration,
    total: Duration,
}

/// Validated arguments of one `Read` or `ReadAlpha` call.
struct SequenceArgs {
    images: Arc<[ImageInfo]>,
    prefetcher: Arc<Prefetcher<FrameBuilder>>,
    format: PixelFormat,
    width: i32,
    height: i32,
    fps_num: i64,
    fps_den: i64,
    /// Whether the `mismatch` argument allowed a clip of varying frames.
    mismatch: bool,
    /// Whether the exif orientation was applied to the pixels.
    apply_rotation: bool,
    num_frames: i32,
    /// True when the sequence is not one single size and format.
    variable: bool,
    debug: bool,
    prefetch_workers: usize,
    timings: SetupTimings,
}

impl SequenceArgs {
    /// Reads, probes, and validates the arguments of one call.
    ///
    /// `clips` are the clips the call hands out, which are also the frames its
    /// lookahead workers build.
    fn read(input: &MapRef, clips: &[Clip], core: CoreRef) -> Result<Self> {
        let setup_started = Instant::now();
        let files = read_files(input)?;
        let fps_num = read_optional_int(input, key!(c"fpsnum"), "fpsnum")?.unwrap_or(24);
        let fps_den = read_optional_int(input, key!(c"fpsden"), "fpsden")?.unwrap_or(1);
        let mismatch = read_optional_int(input, key!(c"mismatch"), "mismatch")?.unwrap_or(0) != 0;
        // Rotating is the default because that is what every other source of a
        // single picture does with the orientation a file states, and
        // `apply_rotation=0` is there for the callers that would rather move
        // the samples themselves.
        let apply_rotation =
            read_optional_int(input, key!(c"apply_rotation"), "apply_rotation")?.unwrap_or(1) != 0;
        let debug = read_optional_int(input, key!(c"debug"), "debug")?.unwrap_or(0) != 0;
        let prefetch_workers =
            resolve_prefetch_workers(read_optional_int(input, key!(c"prefetch"), "prefetch")?)?;
        let prefetch_memory = resolve_prefetch_memory(read_optional_int(
            input,
            key!(c"prefetch_memory"),
            "prefetch_memory",
        )?)?;
        let (fps_num, fps_den) = reduce_fps(fps_num, fps_den)?;

        let probe_started = Instant::now();
        let images = files
            .iter()
            .map(|path| decoder::probe(path, apply_rotation))
            .collect::<Result<Vec<_>>>()?;
        let probe = probe_started.elapsed();

        let validate_started = Instant::now();
        let images: Arc<[ImageInfo]> = validate_images(images, mismatch)?.into();
        let validate = validate_started.elapsed();
        let num_frames = i32::try_from(images.len())
            .map_err(|_| ImgSeqError::new("the image sequence has too many frames"))?;
        let variable = mismatch && has_format_mismatch(&images);
        // The clip is the size the frames are written as, which a transposing
        // orientation swaps relative to the size the files hold. Deciding it
        // here is what lets `mismatch=0` reject a folder that mixes a rotated
        // page with upright ones before a frame is ever requested.
        let (width, height) = if variable {
            (0, 0)
        } else {
            (
                i32::try_from(images[0].output_width())
                    .map_err(|_| ImgSeqError::new("image width does not fit VapourSynth"))?,
                i32::try_from(images[0].output_height())
                    .map_err(|_| ImgSeqError::new("image height does not fit VapourSynth"))?,
            )
        };

        Ok(Self {
            format: images[0].format,
            prefetcher: Arc::new(Prefetcher::new(
                Arc::clone(&images),
                FrameBuilder::new(core, clips),
                prefetch_workers,
                prefetch_memory,
            )),
            images,
            width,
            height,
            fps_num,
            fps_den,
            mismatch,
            apply_rotation,
            num_frames,
            variable,
            debug,
            prefetch_workers,
            timings: SetupTimings {
                probe,
                validate,
                total: setup_started.elapsed(),
            },
        })
    }

    /// Output format of one clip of this sequence.
    fn video_info(&self, core: &CoreRef, clip: Clip) -> VideoInfo {
        if self.variable {
            return VideoInfo {
                format: undefined_video_format(),
                fps_num: self.fps_num,
                fps_den: self.fps_den,
                width: 0,
                height: 0,
                num_frames: self.num_frames,
            };
        }
        VideoInfo {
            format: query_format(core, clip.pixel_format(self.format)),
            fps_num: self.fps_num,
            fps_den: self.fps_den,
            width: self.width,
            height: self.height,
            num_frames: self.num_frames,
        }
    }

    /// Keeps the state frame requests need and drops the setup-only fields.
    fn into_sequence(self) -> Arc<Sequence> {
        Arc::new(Sequence {
            images: self.images,
            prefetcher: self.prefetcher,
            debug: self.debug,
        })
    }
}

impl Filter for Read {
    type Error = ImgSeqError;
    type FrameType = VideoFrame;
    type FilterData = ();

    const NAME: &'static CStr = c"Read";
    const ARGS: &'static CStr = SEQUENCE_ARGS;
    const RETURN_TYPE: &'static CStr = c"clip:vnode;";

    fn create(
        input: MapRef,
        output: MapRef,
        _data: Option<Box<Self::FilterData>>,
        mut core: CoreRef,
    ) -> Result<()> {
        let args = SequenceArgs::read(&input, READ_CLIPS, core)?;
        log_create(&mut core, &args, READ_CLIPS);
        let info = args.video_info(&core, Clip::Color);
        add_filter(
            &mut core,
            output,
            Self::NAME,
            &info,
            Read {
                sequence: args.into_sequence(),
            },
        );
        Ok(())
    }

    fn get_frame(
        &self,
        n: i32,
        activation_reason: ffi::VSActivationReason,
        _frame_data: *mut *mut c_void,
        _frame_ctx: FrameContext,
        core: CoreRef,
    ) -> Result<Option<Self::FrameType>> {
        clip_frame(&self.sequence, Clip::Color, n, activation_reason, core)
    }
}

impl Filter for ReadAlpha {
    type Error = ImgSeqError;
    type FrameType = VideoFrame;
    type FilterData = ();

    const NAME: &'static CStr = c"ReadAlpha";
    const ARGS: &'static CStr = SEQUENCE_ARGS;
    const RETURN_TYPE: &'static CStr = c"clip:vnode;alpha:vnode;";

    fn create(
        input: MapRef,
        mut output: MapRef,
        _data: Option<Box<Self::FilterData>>,
        mut core: CoreRef,
    ) -> Result<()> {
        let args = SequenceArgs::read(&input, READ_ALPHA_CLIPS, core)?;
        log_create(&mut core, &args, READ_ALPHA_CLIPS);
        let color_info = args.video_info(&core, Clip::Color);
        let alpha_info = args.video_info(&core, Clip::Alpha);
        let sequence = args.into_sequence();

        // A filter node can only be created under the "clip" key, so the alpha
        // clip is built first, taken out of the output map, and published once
        // the color clip owns the primary key.
        add_filter(
            &mut core,
            output,
            Self::NAME,
            &alpha_info,
            ReadAlpha {
                sequence: Arc::clone(&sequence),
            },
        );
        let alpha_node = output
            .get_video_node(key!(c"clip"), 0)
            .map_err(ImgSeqError::from_display)?;
        output.delete_key(key!(c"clip"));
        add_filter(
            &mut core,
            output,
            Self::NAME,
            &color_info,
            Read { sequence },
        );
        output
            .consume_node(key!(c"alpha"), alpha_node, AppendMode::Replace)
            .map_err(ImgSeqError::from_display)?;
        Ok(())
    }

    fn get_frame(
        &self,
        n: i32,
        activation_reason: ffi::VSActivationReason,
        _frame_data: *mut *mut c_void,
        _frame_ctx: FrameContext,
        core: CoreRef,
    ) -> Result<Option<Self::FrameType>> {
        clip_frame(&self.sequence, Clip::Alpha, n, activation_reason, core)
    }
}

/// Adds the node that produces one clip of a sequence to the output map.
fn add_filter<F: Filter>(
    core: &mut CoreRef<'_>,
    output: MapRef,
    name: &CStr,
    info: &VideoInfo,
    filter: F,
) {
    let dependencies = Dependencies::new(&[]).expect("an empty dependency list is valid");
    core.create_video_filter(output, name, info, Box::new(filter), dependencies);
}

/// Returns one frame of `clip` from the state the clips of a call share.
///
/// The pool only hands out frames a worker has already decoded and written, so
/// this adds nothing but the timings it reports under `debug`.
fn clip_frame(
    sequence: &Sequence,
    clip: Clip,
    n: i32,
    activation_reason: ffi::VSActivationReason,
    mut core: CoreRef,
) -> Result<Option<VideoFrame>> {
    if activation_reason != ffi::VSActivationReason::Initial {
        return Ok(None);
    }
    let frame_started = Instant::now();
    let index =
        usize::try_from(n).map_err(|_| ImgSeqError::new(format!("requested invalid frame {n}")))?;
    let image = sequence.images.get(index).ok_or_else(|| {
        ImgSeqError::new(format!(
            "requested frame {n}, but the clip has {} frames",
            sequence.images.len()
        ))
    })?;
    let fetch_started = Instant::now();
    let frames = sequence.prefetcher.fetch(n)?;
    let fetch = fetch_started.elapsed();
    let frame = frames.frame(clip)?;

    if sequence.debug {
        let decode = frames.decode_timings();
        let work = frames.frame_timings(clip);
        log_debug(
            &mut core,
            format_args!(
                "frame {} '{}' ({}): fetch={} decode={} (open={} metadata={} buffer={} read={}) format={} allocate={} convert={} properties={} total={}",
                n,
                image.path.display(),
                clip.name(),
                format_duration(fetch),
                format_duration(decode.open + decode.metadata + decode.buffer + decode.read),
                format_duration(decode.open),
                format_duration(decode.metadata),
                format_duration(decode.buffer),
                format_duration(decode.read),
                clip.pixel_format(image.format).name(),
                format_duration(work.map_or(Duration::ZERO, |work| work.allocate)),
                format_duration(work.map_or(Duration::ZERO, |work| work.write.deinterleave)),
                format_duration(work.map_or(Duration::ZERO, |work| work.properties)),
                format_duration(frame_started.elapsed()),
            ),
        );
    }
    Ok(Some(frame))
}

/// Reports how long setting up one instance took.
fn log_create(core: &mut CoreRef<'_>, args: &SequenceArgs, clips: &[Clip]) {
    if !args.debug {
        return;
    }
    let clips = clips
        .iter()
        .map(|clip| clip.name())
        .collect::<Vec<_>>()
        .join("+");
    log_debug(
        core,
        format_args!(
            "create: frames={} clips={} mismatch={} variable={} apply_rotation={} probe={} validate={} prefetch={} prefetch_memory={} total={}",
            args.images.len(),
            clips,
            args.mismatch,
            args.variable,
            args.apply_rotation,
            format_duration(args.timings.probe),
            format_duration(args.timings.validate),
            args.prefetch_workers,
            format_memory(args.prefetcher.byte_budget()),
            format_duration(args.timings.total),
        ),
    );
}

fn log_debug(core: &mut CoreRef<'_>, message: impl Display) {
    let Ok(message) = CString::new(format!("[imgseqs][debug] {message}")) else {
        return;
    };
    core.log(ffi::VSMessageType::Information, &message);
}

fn format_duration(duration: std::time::Duration) -> String {
    format!("{:.3} ms", duration.as_secs_f64() * 1000.0)
}

fn format_memory(bytes: usize) -> String {
    format!("{:.0} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn read_files(input: &MapRef) -> Result<Vec<PathBuf>> {
    let key = key!(c"files");
    let count = input
        .num_elements(key)
        .ok_or_else(|| ImgSeqError::new("Read requires at least one file in the files argument"))?;
    if count <= 0 {
        return Err(ImgSeqError::new("Read requires at least one file"));
    }

    (0..count)
        .map(|index| {
            input
                .get_utf8(key, index)
                .map(PathBuf::from)
                .map_err(|error| map_error("files", index, error))
        })
        .collect()
}

fn read_optional_int(
    input: &MapRef,
    key: &vapoursynth4_rs::map::KeyStr,
    name: &str,
) -> Result<Option<i64>> {
    match input.get_int(key, 0) {
        Ok(value) => Ok(Some(value)),
        Err(MapPropertyError::KeyNotFound) => Ok(None),
        Err(error) => Err(map_error(name, 0, error)),
    }
}

fn map_error(key: &str, index: i32, error: MapPropertyError) -> ImgSeqError {
    ImgSeqError::new(format!("invalid {key}[{index}] argument: {error}"))
}

fn reduce_fps(fps_num: i64, fps_den: i64) -> Result<(i64, i64)> {
    if fps_num <= 0 || fps_den <= 0 {
        return Err(ImgSeqError::new(format!(
            "fpsnum and fpsden must be positive, got {fps_num}/{fps_den}"
        )));
    }
    let divisor = gcd(fps_num, fps_den);
    Ok((fps_num / divisor, fps_den / divisor))
}

fn resolve_prefetch_workers(requested: Option<i64>) -> Result<usize> {
    match requested {
        None => Ok(prefetch::automatic_workers()),
        Some(value) if value < 0 => Err(ImgSeqError::new(format!(
            "prefetch must be zero or a positive number of worker threads, got {value}"
        ))),
        Some(0) => Ok(0),
        Some(value) => Ok(usize::try_from(value)
            .unwrap_or(usize::MAX)
            .min(prefetch::MAX_WORKERS)),
    }
}

/// Decoded data budget for the lookahead pool, in bytes.
///
/// `None` lets the pool size it from the worker count and the largest frame of
/// the sequence, which is what `prefetch_memory` exists to override.
fn resolve_prefetch_memory(requested: Option<i64>) -> Result<Option<usize>> {
    match requested {
        None => Ok(None),
        Some(value) if value < 1 => Err(ImgSeqError::new(format!(
            "prefetch_memory must be at least 1 MiB, got {value}; use prefetch=0 to disable lookahead decoding"
        ))),
        Some(value) => Ok(Some(
            usize::try_from(value)
                .unwrap_or(usize::MAX)
                .saturating_mul(1024 * 1024),
        )),
    }
}

fn gcd(mut left: i64, mut right: i64) -> i64 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left.abs()
}

fn validate_images(images: Vec<ImageInfo>, mismatch: bool) -> Result<Vec<ImageInfo>> {
    let first = images
        .first()
        .ok_or_else(|| ImgSeqError::new("Read requires at least one file"))?;
    if !mismatch {
        for (index, image) in images.iter().enumerate().skip(1) {
            // The size the frames will be is the size the image is handed out
            // as, so that is what a folder has to agree on.
            if image.output_width() != first.output_width()
                || image.output_height() != first.output_height()
                || image.format != first.format
            {
                return Err(ImgSeqError::new(format!(
                    "frame {index} ('{}') has {}x{} {}, expected frame 0 ('{}') to be {}x{} {}",
                    image.path.display(),
                    image.output_width(),
                    image.output_height(),
                    image.format.name(),
                    first.path.display(),
                    first.output_width(),
                    first.output_height(),
                    first.format.name(),
                )));
            }
        }
    }
    Ok(images)
}

fn has_format_mismatch(images: &[ImageInfo]) -> bool {
    let first = &images[0];
    images.iter().skip(1).any(|image| {
        image.output_width() != first.output_width()
            || image.output_height() != first.output_height()
            || image.format != first.format
    })
}

fn undefined_video_format() -> vapoursynth4_rs::frame::VideoFormat {
    vapoursynth4_rs::frame::VideoFormat {
        color_family: ColorFamily::Undefined,
        sample_type: SampleType::Integer,
        bits_per_sample: 0,
        bytes_per_sample: 0,
        sub_sampling_w: 0,
        sub_sampling_h: 0,
        num_planes: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{gcd, reduce_fps, resolve_prefetch_memory, resolve_prefetch_workers};
    use crate::{clip::Clip, pixel::PixelFormat, prefetch};

    #[test]
    fn maps_clip_formats() {
        assert_eq!(
            Clip::Color.pixel_format(PixelFormat::Rgb8),
            PixelFormat::Rgb8
        );
        assert_eq!(
            Clip::Alpha.pixel_format(PixelFormat::Rgb8),
            PixelFormat::Gray8
        );
        assert_eq!(
            Clip::Alpha.pixel_format(PixelFormat::Rgb32F),
            PixelFormat::Gray32F
        );
        assert_eq!(Clip::Color.alpha_marker(), None);
        assert_eq!(Clip::Alpha.alpha_marker(), Some(true));
        assert_eq!(Clip::Alpha.name(), "alpha");
    }

    #[test]
    fn reduces_frame_rate() {
        assert_eq!(reduce_fps(60000, 1001).unwrap(), (60000, 1001));
        assert_eq!(reduce_fps(120, 4).unwrap(), (30, 1));
        assert_eq!(gcd(24, 18), 6);
    }

    #[test]
    fn resolves_prefetch_workers() {
        assert_eq!(resolve_prefetch_workers(Some(0)).unwrap(), 0);
        assert_eq!(resolve_prefetch_workers(Some(3)).unwrap(), 3);
        assert_eq!(
            resolve_prefetch_workers(Some(10_000)).unwrap(),
            prefetch::MAX_WORKERS
        );
        assert!(resolve_prefetch_workers(Some(-1)).is_err());
        assert!(resolve_prefetch_workers(None).unwrap() <= prefetch::AUTO_MAX_WORKERS);
    }

    #[test]
    fn resolves_prefetch_memory() {
        assert_eq!(resolve_prefetch_memory(None).unwrap(), None);
        assert_eq!(
            resolve_prefetch_memory(Some(64)).unwrap(),
            Some(64 * 1024 * 1024)
        );
        assert!(resolve_prefetch_memory(Some(0)).is_err());
        assert!(resolve_prefetch_memory(Some(-1)).is_err());
    }
}
