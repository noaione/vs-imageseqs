use std::{
    ffi::{CString, c_void},
    fmt::Display,
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

use vapoursynth4_rs::{
    ColorFamily, SampleType, VideoInfo,
    core::CoreRef,
    ffi,
    frame::{FrameContext, VideoFrame},
    key,
    map::{MapPropertyError, MapRef},
    node::{Dependencies, Filter},
};

use crate::{
    color::set_frame_properties,
    decoder::{self, ImageInfo},
    error::{ImgSeqError, Result},
    pixel::{PixelFormat, write_planar},
    prefetch::{self, Prefetcher},
};

pub struct ImageSequence {
    images: Arc<[ImageInfo]>,
    prefetcher: Prefetcher,
    debug: bool,
}

impl Filter for ImageSequence {
    type Error = ImgSeqError;
    type FrameType = VideoFrame;
    type FilterData = ();

    const NAME: &'static std::ffi::CStr = c"Read";
    const ARGS: &'static std::ffi::CStr =
        c"files:data[];fpsnum:int:opt;fpsden:int:opt;mismatch:int:opt;debug:int:opt;prefetch:int:opt;";
    const RETURN_TYPE: &'static std::ffi::CStr = c"clip:vnode;";

    fn create(
        input: MapRef,
        output: MapRef,
        _data: Option<Box<Self::FilterData>>,
        mut core: CoreRef,
    ) -> Result<()> {
        let setup_started = Instant::now();
        let files = read_files(&input)?;
        let fps_num = read_optional_int(&input, key!(c"fpsnum"), "fpsnum")?.unwrap_or(24);
        let fps_den = read_optional_int(&input, key!(c"fpsden"), "fpsden")?.unwrap_or(1);
        let mismatch = read_optional_int(&input, key!(c"mismatch"), "mismatch")?.unwrap_or(0) != 0;
        let debug = read_optional_int(&input, key!(c"debug"), "debug")?.unwrap_or(0) != 0;
        let prefetch_workers =
            resolve_prefetch_workers(read_optional_int(&input, key!(c"prefetch"), "prefetch")?)?;
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

        let format_started = Instant::now();
        let format = if variable {
            undefined_video_format()
        } else {
            query_format(&core, images[0].format)
        };
        let format_time = format_started.elapsed();
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

        let video_info = VideoInfo {
            format,
            fps_num,
            fps_den,
            width,
            height,
            num_frames,
        };
        let dependencies = Dependencies::new(&[]).expect("an empty dependency list is valid");
        if debug {
            log_debug(
                &mut core,
                format_args!(
                    "create: frames={} probe={} validate={} format={} prefetch={} total={}",
                    images.len(),
                    format_duration(probe),
                    format_duration(validate),
                    format_duration(format_time),
                    prefetch_workers,
                    format_duration(setup_started.elapsed()),
                ),
            );
        }
        core.create_video_filter(
            output,
            Self::NAME,
            &video_info,
            Box::new(Self {
                prefetcher: Prefetcher::new(Arc::clone(&images), prefetch_workers),
                images,
                debug,
            }),
            dependencies,
        );
        Ok(())
    }

    fn get_frame(
        &self,
        n: i32,
        activation_reason: ffi::VSActivationReason,
        _frame_data: *mut *mut c_void,
        _frame_ctx: FrameContext,
        mut core: CoreRef,
    ) -> Result<Option<Self::FrameType>> {
        if activation_reason != ffi::VSActivationReason::Initial {
            return Ok(None);
        }
        let frame_started = Instant::now();
        let index = usize::try_from(n)
            .map_err(|_| ImgSeqError::new(format!("requested invalid frame {n}")))?;
        let image = self.images.get(index).ok_or_else(|| {
            ImgSeqError::new(format!(
                "requested frame {n}, but the clip has {} frames",
                self.images.len()
            ))
        })?;
        let decode_started = Instant::now();
        let decoded = self.prefetcher.fetch(n)?;
        let decode = decode_started.elapsed();

        let format_started = Instant::now();
        let format = query_format(&core, image.format);
        let format_time = format_started.elapsed();

        let allocation_started = Instant::now();
        let mut frame = core.new_video_frame(
            &format,
            i32::try_from(decoded.width)
                .map_err(|_| ImgSeqError::new("image width does not fit VapourSynth"))?,
            i32::try_from(decoded.height)
                .map_err(|_| ImgSeqError::new("image height does not fit VapourSynth"))?,
            None,
        );
        let allocation = allocation_started.elapsed();

        let write_timings = write_planar(
            &mut frame,
            decoded.color_type,
            decoded.width,
            decoded.height,
            &decoded.pixels,
        )?;
        let properties_started = Instant::now();
        set_frame_properties(&mut frame, image, index)?;
        let properties = properties_started.elapsed();

        if self.debug {
            log_debug(
                &mut core,
                format_args!(
                    "frame {} '{}': decode={} (open={} metadata={} buffer={} read={}) format={} allocate={} convert={} properties={} total={}",
                    n,
                    image.path.display(),
                    format_duration(decode),
                    format_duration(decoded.timings.open),
                    format_duration(decoded.timings.metadata),
                    format_duration(decoded.timings.buffer),
                    format_duration(decoded.timings.read),
                    format_duration(format_time),
                    format_duration(allocation),
                    format_duration(write_timings.deinterleave),
                    format_duration(properties),
                    format_duration(frame_started.elapsed()),
                ),
            );
        }
        Ok(Some(frame))
    }
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
    core.query_video_format(
        format.color_family(),
        format.sample_type(),
        format.bits_per_sample(),
        0,
        0,
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
    use super::{gcd, reduce_fps, resolve_prefetch_workers};
    use crate::prefetch;

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
}
