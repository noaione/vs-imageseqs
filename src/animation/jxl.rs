//! Animated jpeg xl.
//!
//! This is the one adapter with real random access. A jpeg xl codestream can be
//! scanned for its visible frames without rendering any of them, and each scan
//! result carries a seek target: a file offset and a parser checkpoint. So a
//! backward request does not replay the file, it seeks to the frame's own
//! checkpoint and renders from there, which is what
//! `docs/improvements/21-animated-images.md` calls the strongest random-access
//! API in the group.
//!
//! The scan also collects the file's own timing, which is stated as ticks of a
//! timescale rather than as milliseconds, so the timeline stays exact without a
//! rounding step.
//!
//! The file's bytes are held for the life of the source. A seek target names an
//! absolute file offset, so the bytes have to be addressable; the alternative is
//! a seek per request on every reopen. For a jpeg xl that cost is the file's own
//! size, which is small next to the frames it decodes to.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use jxl::api::{
    JxlDecoder, JxlDecoderOptions, JxlOutputBuffer, ProcessingResult, VisibleFrameInfo, states,
};

use crate::{
    decoder::{DecodeTimings, DecodedImage, Pixels, image_error},
    error::{ImgSeqError, Result},
    formats::jxl::{Aligned, Header},
};

use super::{AnimationDecoder, AnimationSource, Presentation, Rate, SegmentInfo};

/// Describes an animated jpeg xl's timeline without rendering its frames.
///
/// Returns `None` for a file that is not an animation, which leaves it on the
/// still-image path it was on before.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is an animation whose timeline cannot
/// be read.
pub fn segment_info(
    path: &Path,
    info: crate::decoder::ImageInfo,
    fps: Rate,
) -> Result<Option<SegmentInfo>> {
    // A still is the common case and the scan below reads the whole file to find
    // that out. The codestream states it in the header, so a file with no
    // animation header never reaches the scan; a header this cannot read is the
    // scan's to report, with the words it already had.
    if matches!(crate::formats::jxl::states_animation(path), Ok(false)) {
        return Ok(None);
    }
    let bytes =
        Arc::<[u8]>::from(fs::read(path).map_err(|error| image_error("open", path, error))?);
    let scanned = match scan(&bytes, path)? {
        Some(scanned) => scanned,
        None => return Ok(None),
    };
    let Scan {
        frames,
        timescale,
        header,
    } = scanned;
    if frames.len() < 2 {
        return Ok(None);
    }

    // A jpeg xl states its timeline as ticks of `timescale`, so that is the
    // rate the presentations are counted in: `num` ticks a second.
    let rate = Rate::new(i64::from(timescale.0), i64::from(timescale.1));
    if rate.num <= 0 || rate.den <= 0 {
        return Err(ImgSeqError::new(format!(
            "animated image '{}' states an unusable timescale {}/{}",
            path.display(),
            timescale.0,
            timescale.1
        )));
    }
    let mut presentations = Vec::with_capacity(frames.len());
    let mut timestamp = 0i64;
    for frame in &frames {
        presentations.push(Presentation {
            timestamp,
            duration: Some(i64::from(frame.duration_ticks)),
        });
        timestamp = timestamp
            .checked_add(i64::from(frame.duration_ticks))
            .ok_or_else(|| {
                ImgSeqError::new(format!("the timeline of '{}' overflows", path.display()))
            })?;
    }

    let source = Arc::new(AnimationSource::new(
        path.to_path_buf(),
        Box::new(JxlSource::new(path, bytes, frames, header)),
    ));
    Ok(Some(SegmentInfo {
        info,
        rate,
        presentations,
        decoder: source,
        fps,
    }))
}

/// What a scan pass found.
struct Scan {
    /// The visible frames, in timeline order, with their seek targets.
    frames: Vec<VisibleFrameInfo>,
    /// Ticks a second, as the codestream states it.
    timescale: (u32, u32),
    /// What the codestream states about the image, read while the decoder was
    /// still in its image-info state.
    ///
    /// It is read here rather than per frame because it is a property of the
    /// file: re-reading it from a decoder that has already rendered a frame
    /// picks up that frame's layout instead.
    header: Header,
}

/// Scans a codestream for its visible frames without rendering any of them.
///
/// Returns `None` for a file that is not an animation.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the codestream cannot be read.
fn scan(bytes: &[u8], path: &Path) -> Result<Option<Scan>> {
    let mut options = JxlDecoderOptions::default();
    // The scan parses frame headers and skips the section data, so a long
    // animation is indexed without decoding a pixel of it.
    options.scan_frames_only = true;
    let mut input = bytes;
    let mut decoder = JxlDecoder::<states::Initialized>::new(options);
    let mut with_info = loop {
        match decoder
            .process(&mut input, None)
            .map_err(|error| decode_error(path, error))?
        {
            ProcessingResult::Complete { result } => break result,
            ProcessingResult::NeedsMoreInput { fallback, .. } => decoder = fallback,
        }
    };

    let Some(animation) = with_info.basic_info().animation.as_ref() else {
        return Ok(None);
    };
    let timescale = (animation.tps_numerator, animation.tps_denominator);
    if !with_info.has_more_frames() {
        return Ok(None);
    }
    // The header is read before the scan advances the decoder, because the
    // state it is read from is the one the scan leaves behind.
    let header = Header::read(&with_info, path)?;

    loop {
        let mut with_frame = loop {
            match with_info
                .process(&mut input, None)
                .map_err(|error| decode_error(path, error))?
            {
                ProcessingResult::Complete { result } => break result,
                ProcessingResult::NeedsMoreInput { fallback, .. } => with_info = fallback,
            }
        };
        with_info = loop {
            match with_frame
                .skip_frame(&mut input)
                .map_err(|error| decode_error(path, error))?
            {
                ProcessingResult::Complete { result } => break result,
                ProcessingResult::NeedsMoreInput { fallback, .. } => with_frame = fallback,
            }
        };
        if !with_info.has_more_frames() {
            break;
        }
    }

    Ok(Some(Scan {
        frames: with_info.scanned_frames().to_vec(),
        timescale,
        header,
    }))
}

/// One jpeg xl presentation, rendered from the frame's own checkpoint.
struct JxlSource {
    path: PathBuf,
    bytes: Arc<[u8]>,
    frames: Vec<VisibleFrameInfo>,
    /// What the codestream states, read once while the file was scanned.
    header: Header,
    /// The presentation the next render will produce.
    target: usize,
}

impl JxlSource {
    fn new(path: &Path, bytes: Arc<[u8]>, frames: Vec<VisibleFrameInfo>, header: Header) -> Self {
        Self {
            path: path.to_path_buf(),
            bytes,
            frames,
            header,
            target: 0,
        }
    }

    /// Renders one visible frame from its own checkpoint.
    fn render(&mut self, index: usize) -> Result<DecodedImage> {
        let target = self.frames.get(index).ok_or_else(|| {
            ImgSeqError::new(format!(
                "animated image '{}' holds no presentation {index}",
                self.path.display()
            ))
        })?;
        let offset =
            usize::try_from(target.seek_target.decode_start_file_offset).map_err(|_| {
                ImgSeqError::new(format!(
                    "animated image '{}' seeks past what this build can address",
                    self.path.display()
                ))
            })?;
        let mut input = self.bytes.get(offset..).ok_or_else(|| {
            ImgSeqError::new(format!(
                "animated image '{}' seeks past the end of its own data",
                self.path.display()
            ))
        })?;

        // A decoder is built per presentation and started at that frame's own
        // checkpoint. Walking one decoder through successive frames instead is
        // measurably cheaper, and was tried: it fails on a codestream whose
        // frames are not uniform, with `Invalid channel range`, so the
        // per-presentation decoder is what shipped. What a pass repeats is the
        // decoder's own setup rather than the file's structure, because the
        // header was read once when the file was scanned.
        let options = JxlDecoderOptions::default();
        let mut head = self.bytes.as_ref();
        let mut decoder = JxlDecoder::<states::Initialized>::new(options);
        let mut with_info = loop {
            match decoder
                .process(&mut head, None)
                .map_err(|error| decode_error(&self.path, error))?
            {
                ProcessingResult::Complete { result } => break result,
                ProcessingResult::NeedsMoreInput { fallback, .. } => decoder = fallback,
            }
        };
        with_info
            .set_pixel_format(self.header.pixel_format())
            .map_err(|error| decode_error(&self.path, error))?;
        with_info.start_new_frame(target.seek_target);
        let header = &self.header;

        let with_frame = loop {
            match with_info
                .process(&mut input, None)
                .map_err(|error| decode_error(&self.path, error))?
            {
                ProcessingResult::Complete { result } => break result,
                ProcessingResult::NeedsMoreInput { fallback, .. } => with_info = fallback,
            }
        };

        let rows = usize::try_from(header.height)
            .map_err(|_| ImgSeqError::new("image height does not fit in memory"))?;
        let sample_bytes = header.data_format.bytes_per_sample();
        let row_bytes = usize::try_from(header.width)
            .ok()
            .and_then(|width| width.checked_mul(header.jxl_color_type.samples_per_pixel()))
            .and_then(|row| row.checked_mul(sample_bytes))
            .ok_or_else(|| ImgSeqError::new("image row is too large"))?;
        let size = row_bytes
            .checked_mul(rows)
            .ok_or_else(|| ImgSeqError::new("image is too large"))?;
        let mut pixels = vec![0; size];

        // The decoder writes whole samples, so every row has to start where one
        // can be written; a buffer already aligned for that is used directly.
        if pixels.as_ptr().align_offset(sample_bytes) == 0 {
            render_into(
                with_frame,
                &mut input,
                &mut pixels,
                rows,
                row_bytes,
                &self.path,
            )?;
        } else {
            let mut aligned = Aligned::new(size, sample_bytes);
            render_into(
                with_frame,
                &mut input,
                aligned.bytes(),
                rows,
                row_bytes,
                &self.path,
            )?;
            pixels.copy_from_slice(aligned.bytes());
        }

        Ok(DecodedImage {
            width: header.width,
            height: header.height,
            format: header.format().unwrap_or(crate::pixel::PixelFormat::Rgb8),
            transform: crate::pixel::Transform::IDENTITY,
            pixels: Pixels::Interleaved {
                color_type: header.color_type,
                buffer: pixels,
            },
            timings: DecodeTimings {
                open: std::time::Duration::ZERO,
                metadata: std::time::Duration::ZERO,
                buffer: std::time::Duration::ZERO,
                read: std::time::Duration::ZERO,
            },
        })
    }
}

/// Draws one frame into `buffer`, asking for more input as the decoder wants it.
fn render_into(
    mut decoder: JxlDecoder<states::WithFrameInfo>,
    input: &mut &[u8],
    buffer: &mut [u8],
    rows: usize,
    row_bytes: usize,
    path: &Path,
) -> Result<()> {
    let mut output = JxlOutputBuffer::new(buffer, rows, row_bytes);
    loop {
        match decoder
            .process(&mut *input, std::slice::from_mut(&mut output), None)
            .map_err(|error| decode_error(path, error))?
        {
            ProcessingResult::Complete { .. } => return Ok(()),
            ProcessingResult::NeedsMoreInput { fallback, .. } => {
                if input.is_empty() {
                    return Err(image_error(
                        "decode",
                        path,
                        "the file ended before the picture did",
                    ));
                }
                decoder = fallback;
            }
        }
    }
}

fn decode_error(path: &Path, error: jxl::error::Error) -> ImgSeqError {
    image_error("decode", path, error)
}

impl AnimationDecoder for JxlSource {
    fn seek(&mut self, index: usize) -> Result<()> {
        // A jpeg xl carries a checkpoint per frame, so there is nothing to
        // replay: the next render seeks straight to this frame.
        self.target = index;
        Ok(())
    }

    fn next_presentation(&mut self) -> Result<DecodedImage> {
        let image = self.render(self.target)?;
        self.target += 1;
        Ok(image)
    }
}
