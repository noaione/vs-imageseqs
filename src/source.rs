use std::{ffi::c_void, path::PathBuf};

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
};

pub struct ImageSequence {
    images: Box<[ImageInfo]>,
}

impl Filter for ImageSequence {
    type Error = ImgSeqError;
    type FrameType = VideoFrame;
    type FilterData = ();

    const NAME: &'static std::ffi::CStr = c"Read";
    const ARGS: &'static std::ffi::CStr =
        c"files:data[];fpsnum:int:opt;fpsden:int:opt;mismatch:int:opt;";
    const RETURN_TYPE: &'static std::ffi::CStr = c"clip:vnode;";

    fn create(
        input: MapRef,
        output: MapRef,
        _data: Option<Box<Self::FilterData>>,
        mut core: CoreRef,
    ) -> Result<()> {
        let files = read_files(&input)?;
        let fps_num = read_optional_int(&input, key!(c"fpsnum"), "fpsnum")?.unwrap_or(24);
        let fps_den = read_optional_int(&input, key!(c"fpsden"), "fpsden")?.unwrap_or(1);
        let mismatch = read_optional_int(&input, key!(c"mismatch"), "mismatch")?.unwrap_or(0) != 0;
        let (fps_num, fps_den) = reduce_fps(fps_num, fps_den)?;
        let images = files
            .iter()
            .map(|path| decoder::probe(path))
            .collect::<Result<Vec<_>>>()?;
        let images = validate_images(images, mismatch)?;
        let num_frames = i32::try_from(images.len())
            .map_err(|_| ImgSeqError::new("the image sequence has too many frames"))?;
        let variable = mismatch && has_format_mismatch(&images);

        let format = if variable {
            undefined_video_format()
        } else {
            query_format(&core, images[0].format)
        };
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
        core.create_video_filter(
            output,
            Self::NAME,
            &video_info,
            Box::new(Self {
                images: images.into_boxed_slice(),
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
        core: CoreRef,
    ) -> Result<Option<Self::FrameType>> {
        if activation_reason != ffi::VSActivationReason::Initial {
            return Ok(None);
        }
        let index = usize::try_from(n)
            .map_err(|_| ImgSeqError::new(format!("requested invalid frame {n}")))?;
        let image = self.images.get(index).ok_or_else(|| {
            ImgSeqError::new(format!(
                "requested frame {n}, but the clip has {} frames",
                self.images.len()
            ))
        })?;
        let decoded = decoder::decode(image)?;

        let format = query_format(&core, image.format);
        let mut frame = core.new_video_frame(
            &format,
            i32::try_from(decoded.width)
                .map_err(|_| ImgSeqError::new("image width does not fit VapourSynth"))?,
            i32::try_from(decoded.height)
                .map_err(|_| ImgSeqError::new("image height does not fit VapourSynth"))?,
            None,
        );
        write_planar(
            &mut frame,
            decoded.color_type,
            decoded.width,
            decoded.height,
            &decoded.pixels,
        )?;
        set_frame_properties(&mut frame, image, index)?;
        Ok(Some(frame))
    }
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
    use super::{gcd, reduce_fps};

    #[test]
    fn reduces_frame_rate() {
        assert_eq!(reduce_fps(60000, 1001).unwrap(), (60000, 1001));
        assert_eq!(reduce_fps(120, 4).unwrap(), (30, 1));
        assert_eq!(gcd(24, 18), 6);
    }
}
