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
    color::set_frame_properties,
    decoder::{self, ImageInfo, Pixels},
    error::{ImgSeqError, Result},
    pixel::{PixelFormat, write_alpha, write_decoded_planes, write_opaque_alpha, write_planar},
    prefetch::{self, Prefetcher},
};

/// Arguments accepted by `Read` and `ReadAlpha`.
const SEQUENCE_ARGS: &CStr = c"files:data[];fpsnum:int:opt;fpsden:int:opt;mismatch:int:opt;debug:int:opt;prefetch:int:opt;prefetch_memory:int:opt;";

/// One of the clips an image sequence hands out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Clip {
    /// Color planes of every frame.
    Color,
    /// Alpha plane of every frame, filled with the opaque value when the file
    /// has no alpha channel.
    Alpha,
}

impl Clip {
    /// Pixel format this clip uses for an image decoded as `format`.
    const fn pixel_format(self, format: PixelFormat) -> PixelFormat {
        match self {
            Self::Color => format,
            Self::Alpha => format.alpha_format(),
        }
    }

    /// `ImgSeqAlpha` property of this clip's frames; color clips have none.
    const fn alpha_marker(self) -> Option<bool> {
        match self {
            Self::Color => None,
            Self::Alpha => Some(true),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Alpha => "alpha",
        }
    }
}

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
    prefetcher: Arc<Prefetcher>,
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
    prefetcher: Arc<Prefetcher>,
    format: PixelFormat,
    width: i32,
    height: i32,
    fps_num: i64,
    fps_den: i64,
    num_frames: i32,
    /// True when the sequence is not one single size and format.
    variable: bool,
    debug: bool,
    prefetch_workers: usize,
    timings: SetupTimings,
}

impl SequenceArgs {
    /// Reads, probes, and validates the arguments of one call.
    fn read(input: &MapRef) -> Result<Self> {
        let setup_started = Instant::now();
        let files = read_files(input)?;
        let fps_num = read_optional_int(input, key!(c"fpsnum"), "fpsnum")?.unwrap_or(24);
        let fps_den = read_optional_int(input, key!(c"fpsden"), "fpsden")?.unwrap_or(1);
        let mismatch = read_optional_int(input, key!(c"mismatch"), "mismatch")?.unwrap_or(0) != 0;
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
            .map(|path| decoder::probe(path))
            .collect::<Result<Vec<_>>>()?;
        let probe = probe_started.elapsed();

        let validate_started = Instant::now();
        let images: Arc<[ImageInfo]> = validate_images(images, mismatch)?.into();
        let validate = validate_started.elapsed();
        let num_frames = i32::try_from(images.len())
            .map_err(|_| ImgSeqError::new("the image sequence has too many frames"))?;
        let variable = mismatch && has_format_mismatch(&images);
        let (width, height) = if variable {
            (0, 0)
        } else {
            (
                i32::try_from(images[0].width)
                    .map_err(|_| ImgSeqError::new("image width does not fit VapourSynth"))?,
                i32::try_from(images[0].height)
                    .map_err(|_| ImgSeqError::new("image height does not fit VapourSynth"))?,
            )
        };

        Ok(Self {
            format: images[0].format,
            prefetcher: Arc::new(Prefetcher::new(
                Arc::clone(&images),
                prefetch_workers,
                prefetch_memory,
            )),
            images,
            width,
            height,
            fps_num,
            fps_den,
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
        let args = SequenceArgs::read(&input)?;
        log_create(&mut core, &args, &[Clip::Color]);
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
        let args = SequenceArgs::read(&input)?;
        log_create(&mut core, &args, &[Clip::Color, Clip::Alpha]);
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

/// Writes one frame of `clip` from the state the clips of a call share.
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
    let decode_started = Instant::now();
    let decoded = sequence.prefetcher.fetch(n)?;
    let decode = decode_started.elapsed();

    let format = clip.pixel_format(image.format);
    let allocation_started = Instant::now();
    let mut frame = core.new_video_frame(
        &query_format(&core, format),
        i32::try_from(decoded.width)
            .map_err(|_| ImgSeqError::new("image width does not fit VapourSynth"))?,
        i32::try_from(decoded.height)
            .map_err(|_| ImgSeqError::new("image height does not fit VapourSynth"))?,
        None,
    );
    let allocation = allocation_started.elapsed();

    let write_timings = match (clip, &decoded.pixels) {
        (Clip::Color, Pixels::Planar(planes)) => write_decoded_planes(
            &mut frame,
            decoded.format,
            decoded.width,
            decoded.height,
            planes,
        )?,
        (Clip::Color, Pixels::Interleaved { color_type, buffer }) => write_planar(
            &mut frame,
            *color_type,
            decoded.width,
            decoded.height,
            buffer,
        )?,
        // Planar decodes have no alpha channel to read: the format that decodes
        // to planes only takes files without one.
        (Clip::Alpha, Pixels::Planar(_)) => {
            write_opaque_alpha(&mut frame, format, decoded.width, decoded.height)?
        }
        (Clip::Alpha, Pixels::Interleaved { color_type, buffer }) => write_alpha(
            &mut frame,
            *color_type,
            decoded.width,
            decoded.height,
            buffer,
        )?,
    };
    let properties_started = Instant::now();
    set_frame_properties(&mut frame, image, index, format, clip.alpha_marker())?;
    let properties = properties_started.elapsed();

    if sequence.debug {
        log_debug(
            &mut core,
            format_args!(
                "frame {} '{}' ({}): decode={} (open={} metadata={} buffer={} read={}) format={} allocate={} convert={} properties={} total={}",
                n,
                image.path.display(),
                clip.name(),
                format_duration(decode),
                format_duration(decoded.timings.open),
                format_duration(decoded.timings.metadata),
                format_duration(decoded.timings.buffer),
                format_duration(decoded.timings.read),
                format.name(),
                format_duration(allocation),
                format_duration(write_timings.deinterleave),
                format_duration(properties),
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
            "create: frames={} clips={} probe={} validate={} prefetch={} prefetch_memory={} total={}",
            args.images.len(),
            clips,
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
            if image.width != first.width
                || image.height != first.height
                || image.format != first.format
            {
                return Err(ImgSeqError::new(format!(
                    "frame {index} ('{}') has {}x{} {}, expected frame 0 ('{}') to be {}x{} {}",
                    image.path.display(),
                    image.width,
                    image.height,
                    image.format.name(),
                    first.path.display(),
                    first.width,
                    first.height,
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
        image.width != first.width || image.height != first.height || image.format != first.format
    })
}

fn query_format(core: &CoreRef, format: PixelFormat) -> vapoursynth4_rs::frame::VideoFormat {
    let (sub_sampling_w, sub_sampling_h) = format.sub_sampling();
    core.query_video_format(
        format.color_family(),
        format.sample_type(),
        format.bits_per_sample(),
        sub_sampling_w,
        sub_sampling_h,
    )
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
    use super::{Clip, gcd, reduce_fps, resolve_prefetch_memory, resolve_prefetch_workers};
    use crate::{pixel::PixelFormat, prefetch};

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
