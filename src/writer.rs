//! `PNGWrite`: request-driven PNG export.
//!
//! `PNGWrite` is the other end of `Read`. It is a video node whose frames are
//! the frames of the clip it was given, so a graph can sit behind it unchanged,
//! and evaluating one of its frames writes that frame to a PNG before the
//! request returns. Creating the node writes nothing: a frame that is never
//! requested is never written.
//!
//! The first scope is the one `docs/improvements/38-png-write.md` recommends:
//! integer Gray and RGB at eight to sixteen bits, optionally beside a matching
//! Gray alpha clip. A frame at nine to fifteen bits is written as a sixteen bit
//! PNG whose samples carry the source precision in their high bits, which is
//! exact rather than approximate and needs no conversion stage. YUV and float
//! are refused with a message that names the upstream conversion, because the
//! matrix, range, dithering and transfer choices a conversion needs are the
//! caller's and not this writer's.
//!
//! Success is a per-instance ledger keyed by frame number; the frame properties
//! are receipts for the caller and never the thing that decides whether a frame
//! is written. A destination is published by encoding a uniquely named
//! temporary file beside it and then renaming or linking it into place, so a
//! failed encode never leaves a partial file where a reader could take it for a
//! finished one.

use std::{
    borrow::Cow,
    ffi::{CStr, c_void},
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use png::{BitDepth, ColorType, DeflateCompression, Encoder, Filter as PngFilter, Info};
use vapoursynth4_rs::{
    ColorFamily, SampleType, VideoInfo,
    core::CoreRef,
    ffi,
    frame::{Frame, FrameContext, VideoFrame},
    key,
    map::{AppendMode, KeyStr, MapPropertyError, MapRef, Value},
    node::{Dependencies, Filter, FilterMode, Node, VideoNode},
};

use crate::error::{ImgSeqError, Result};

/// Arguments accepted by `PNGWrite`.
const PNG_WRITE_ARGS: &CStr = c"clip:vnode;output_path:data;alpha:vnode:opt;always_save:int:opt;compression:int:opt;overwrite:int:opt;start_number:int:opt;icc_profile:int:opt;debug:int:opt;";

/// The compression level a caller gets when it asks for none.
const DEFAULT_COMPRESSION: i64 = 6;

/// The largest zero padding a numbered template may ask for.
///
/// A frame number is at most twenty digits long, so anything past this is a typo
/// rather than a layout, and rendering it would allocate for a path no file
/// system accepts.
const MAX_PADDING: usize = 64;

/// Bytes buffered between the PNG compressor and the file.
const FILE_BUFFER: usize = 64 * 1024;

/// The code every H.273 family uses for "unspecified".
const UNSPECIFIED: u8 = 2;

/// The `png` bit depth that names `bits` significant bits.
///
/// Nine through fifteen are stored as sixteen bit samples whose high bits hold
/// the source precision, which is what [`widen`] is for.
const fn png_depth(bits: i32) -> BitDepth {
    if bits <= 8 {
        BitDepth::Eight
    } else {
        BitDepth::Sixteen
    }
}

/// Spreads a sample of `bits` bits across a sixteen bit word.
///
/// The high bits are the sample itself and the low ones repeat it, so the
/// largest sample of `bits` bits becomes `u16::MAX` exactly: a ten bit picture
/// whose white is 1023 does not come out at 98% of the scale, and a ten bit
/// alpha of 1023 is fully opaque. `sample >> (16 - bits)` recovers the source
/// value without loss.
#[must_use]
const fn widen(sample: u16, bits: u32) -> u16 {
    let shift = 16 - bits;
    let high = (sample as u32) << shift;
    let low = (sample as u32) >> (2 * bits - 16);
    (high | low) as u16
}

/// One literal or one substitution of an output path template.
#[derive(Debug, Eq, PartialEq)]
enum Piece {
    /// Text copied into every destination.
    Text(String),
    /// The frame number, zero padded to `width` digits when `width` is not 0.
    Number { width: usize },
}

/// The output path of a call, resolved once and rendered per frame.
#[derive(Debug, Eq, PartialEq)]
struct Template {
    pieces: Vec<Piece>,
}

impl Template {
    /// Reads the template grammar out of an already resolved path.
    ///
    /// The grammar is deliberately small: `%d` and `%0Nd` are the number, `%%`
    /// is a literal percent sign, and every other use of `%` is refused rather
    /// than handed to a formatting function whose conversion a caller could not
    /// have known this writer understands.
    fn parse(text: &str) -> Result<Self> {
        let mut pieces = Vec::new();
        let mut literal = String::new();
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            if character != '%' {
                literal.push(character);
                continue;
            }
            match characters.next() {
                Some('%') => literal.push('%'),
                Some('d') => {
                    push_literal(&mut pieces, &mut literal);
                    pieces.push(Piece::Number { width: 0 });
                }
                Some('0') => {
                    let mut digits = String::new();
                    while let Some(digit) = characters.peek().copied().filter(char::is_ascii_digit)
                    {
                        digits.push(digit);
                        characters.next();
                    }
                    if characters.next() != Some('d') {
                        return Err(ImgSeqError::new(
                            "an output path substitution after '%0' has to end in 'd', as in 'frame%06d.png'",
                        ));
                    }
                    let width = if digits.is_empty() {
                        0
                    } else {
                        digits.parse::<usize>().map_err(|_| {
                            ImgSeqError::new(
                                "an output path number is padded to more digits than a path can hold",
                            )
                        })?
                    };
                    if width > MAX_PADDING {
                        return Err(ImgSeqError::new(format!(
                            "an output path number may be padded to at most {MAX_PADDING} digits, got {width}"
                        )));
                    }
                    push_literal(&mut pieces, &mut literal);
                    pieces.push(Piece::Number { width });
                }
                Some(other) => {
                    return Err(ImgSeqError::new(format!(
                        "'%{other}' is not a substitution PNGWrite knows; use '%d', '%0Nd' or '%%'"
                    )));
                }
                None => {
                    return Err(ImgSeqError::new(
                        "an output path ends with an incomplete '%' substitution",
                    ));
                }
            }
        }
        push_literal(&mut pieces, &mut literal);
        Ok(Self { pieces })
    }

    /// Whether the template names a number at all.
    fn numbered(&self) -> bool {
        self.pieces
            .iter()
            .any(|piece| matches!(piece, Piece::Number { .. }))
    }

    /// The destination of one numbered frame.
    fn render(&self, number: i64) -> String {
        let mut rendered = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Text(text) => rendered.push_str(text),
                Piece::Number { width } => {
                    if *width == 0 {
                        rendered.push_str(&number.to_string());
                    } else {
                        rendered.push_str(&format!("{number:0>width$}", width = *width));
                    }
                }
            }
        }
        rendered
    }
}

/// Keeps the literal run that precedes the next substitution, if it has one.
fn push_literal(pieces: &mut Vec<Piece>, literal: &mut String) {
    if !literal.is_empty() {
        pieces.push(Piece::Text(std::mem::take(literal)));
    }
}

/// Which frames one writer instance has published.
///
/// The bit is set only after a frame has been published, and the set lives
/// beside the filter rather than inside frame properties, because a property is
/// a receipt the caller can read while this ledger is what decides whether work
/// is done. It grows to the highest frame number that was actually written, so
/// a writer over a long clip that is asked for a handful of frames allocates a
/// handful of bits.
#[derive(Debug, Default)]
struct Ledger {
    bits: Vec<u64>,
}

impl Ledger {
    fn contains(&self, index: usize) -> bool {
        self.bits
            .get(index / 64)
            .is_some_and(|word| word & (1 << (index % 64)) != 0)
    }

    fn insert(&mut self, index: usize) {
        let word = index / 64;
        if self.bits.len() <= word {
            self.bits.resize(word + 1, 0);
        }
        self.bits[word] |= 1 << (index % 64);
    }
}

/// What one frame's pixels are written as.
#[derive(Debug, Clone, Copy)]
struct FramePlan {
    color_type: ColorType,
    bit_depth: BitDepth,
    /// Significant bits of one source sample, which is what `sBIT` states.
    bits: i32,
    /// Whether samples hold fewer bits than the sixteen bit word they go into.
    spread: bool,
    /// Planes of the color frame: one for gray, three for rgb.
    color_planes: usize,
    /// Whether the alpha clip's plane is interleaved after each pixel.
    alpha: bool,
    width: usize,
    height: usize,
}

impl FramePlan {
    /// Validates the two frames of one request against the supported scope.
    fn of(color: &VideoFrame, alpha: Option<&VideoFrame>) -> Result<Self> {
        let format = color.get_video_format();
        let color_planes = match format.color_family {
            ColorFamily::Gray if format.num_planes == 1 => 1,
            ColorFamily::RGB if format.num_planes == 3 => 3,
            ColorFamily::Gray | ColorFamily::RGB => {
                return Err(ImgSeqError::new(format!(
                    "PNGWrite needs a one plane gray or three plane rgb frame, got {} planes",
                    format.num_planes
                )));
            }
            family => return Err(unsupported_family(family, format.sample_type)),
        };
        if format.sample_type != SampleType::Integer {
            return Err(unsupported_family(format.color_family, format.sample_type));
        }
        if !(8..=16).contains(&format.bits_per_sample) {
            return Err(ImgSeqError::new(format!(
                "PNGWrite accepts 8 to 16 bit integer frames, got {} bit samples",
                format.bits_per_sample
            )));
        }
        if format.sub_sampling_w != 0 || format.sub_sampling_h != 0 {
            return Err(ImgSeqError::new(
                "PNGWrite needs a frame without chroma subsampling",
            ));
        }

        let width = plane_size(color.frame_width(0), "width")?;
        let height = plane_size(color.frame_height(0), "height")?;
        for plane in 0..color_planes {
            if color.frame_width(plane as i32) as usize != width
                || color.frame_height(plane as i32) as usize != height
            {
                return Err(ImgSeqError::new(
                    "PNGWrite needs planes of one size, which this frame does not have",
                ));
            }
        }

        let color_type = match (color_planes, alpha.is_some()) {
            (1, false) => ColorType::Grayscale,
            (1, true) => ColorType::GrayscaleAlpha,
            (3, false) => ColorType::Rgb,
            (3, true) => ColorType::Rgba,
            _ => {
                return Err(ImgSeqError::new(
                    "PNGWrite lost track of the output color type",
                ));
            }
        };

        if let Some(alpha) = alpha {
            let alpha_format = alpha.get_video_format();
            if alpha_format.color_family != ColorFamily::Gray || alpha_format.num_planes != 1 {
                return Err(ImgSeqError::new(
                    "PNGWrite needs a one plane gray alpha clip",
                ));
            }
            if alpha_format.sample_type != SampleType::Integer {
                return Err(ImgSeqError::new(
                    "PNGWrite needs an integer alpha clip, so a float alpha cannot be interleaved losslessly",
                ));
            }
            if alpha_format.bits_per_sample != format.bits_per_sample {
                return Err(ImgSeqError::new(format!(
                    "PNGWrite needs the alpha clip at the color clip's depth, got {} bit alpha beside {} bit color",
                    alpha_format.bits_per_sample, format.bits_per_sample
                )));
            }
            if alpha.frame_width(0) as usize != width || alpha.frame_height(0) as usize != height {
                return Err(ImgSeqError::new(format!(
                    "PNGWrite needs the alpha clip at the color clip's size, got {}x{} beside {width}x{height}",
                    alpha.frame_width(0),
                    alpha.frame_height(0)
                )));
            }
        }

        Ok(Self {
            color_type,
            bit_depth: png_depth(format.bits_per_sample),
            bits: format.bits_per_sample,
            spread: (9..=15).contains(&format.bits_per_sample),
            color_planes,
            alpha: alpha.is_some(),
            width,
            height,
        })
    }

    /// Samples one output pixel holds, which is what the PNG row is made of.
    const fn channels(&self) -> usize {
        self.color_planes + self.alpha as usize
    }

    /// Bytes one output pixel holds.
    const fn pixel_bytes(&self) -> usize {
        self.channels() * self.sample_bytes()
    }

    /// Bytes one stored sample holds in the PNG.
    const fn sample_bytes(&self) -> usize {
        if matches!(self.bit_depth, BitDepth::Eight) {
            1
        } else {
            2
        }
    }

    /// Bytes one PNG row holds.
    fn row_bytes(&self) -> Result<usize> {
        self.width
            .checked_mul(self.pixel_bytes())
            .ok_or_else(|| ImgSeqError::new("one PNG row does not fit in memory"))
    }
}

/// Turns a VapourSynth plane length into a non-zero count.
fn plane_size(value: i32, what: &str) -> Result<usize> {
    usize::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| ImgSeqError::new(format!("PNGWrite got an empty frame {what}")))
}

/// The message a family this writer does not convert is refused with.
fn unsupported_family(family: ColorFamily, sample_type: SampleType) -> ImgSeqError {
    let family = match family {
        ColorFamily::Gray => "gray",
        ColorFamily::RGB => "rgb",
        ColorFamily::YUV => "yuv",
        ColorFamily::Undefined => "undefined",
    };
    let sample_type = match sample_type {
        SampleType::Integer => "integer",
        SampleType::Float => "float",
    };
    ImgSeqError::new(format!(
        "PNGWrite writes integer gray and rgb frames, got a {sample_type} {family} frame; convert it upstream, for example core.resize.Bicubic(clip, format=vs.RGB48, matrix_in_s=\"709\")"
    ))
}

/// The output path and the frame numbering a call was created with.
#[derive(Debug)]
struct Destinations {
    template: Template,
    start_number: i64,
}

impl Destinations {
    /// Reads, resolves and checks the template of one call.
    ///
    /// A relative path is resolved here, while the filter is being created,
    /// because a later frame request must not depend on the process directory
    /// another part of the script may have moved.
    fn read(text: &str, start_number: i64, frames: i64) -> Result<Self> {
        if text.is_empty() {
            return Err(ImgSeqError::new("PNGWrite needs a non-empty output_path"));
        }
        if text.contains('\0') {
            return Err(ImgSeqError::new(
                "PNGWrite needs an output_path without a NUL byte",
            ));
        }
        let resolved = if Path::new(text).is_absolute() {
            PathBuf::from(text)
        } else {
            std::env::current_dir()
                .map_err(ImgSeqError::from_display)?
                .join(text)
        };
        let resolved = resolved
            .to_str()
            .ok_or_else(|| ImgSeqError::new("the PNGWrite output_path is not valid Unicode"))?;
        let template = Template::parse(resolved)?;
        if frames > 1 && !template.numbered() {
            return Err(ImgSeqError::new(format!(
                "PNGWrite over {frames} frames needs a numbered output_path such as 'frame%06d.png', because a literal path names one destination"
            )));
        }
        let destinations = Self {
            template,
            start_number,
        };
        destinations.check_parent(start_number)?;
        if frames > 1 {
            // The last destination is checked too, so a start_number that
            // cannot be numbered that far is refused before a frame is
            // requested rather than wrapping into a path of its own.
            let last = start_number.checked_add(frames - 1).ok_or_else(|| {
                ImgSeqError::new(
                    "the PNGWrite start_number and frame count cannot be numbered without overflow",
                )
            })?;
            destinations.check_parent(last)?;
        }
        Ok(destinations)
    }

    /// The destination one frame index is written to.
    fn of(&self, index: usize) -> Result<PathBuf> {
        let number = i64::try_from(index)
            .ok()
            .and_then(|index| self.start_number.checked_add(index))
            .ok_or_else(|| ImgSeqError::new("the PNGWrite frame number overflowed"))?;
        Ok(PathBuf::from(self.template.render(number)))
    }

    /// Refuses a destination whose directory does not exist yet.
    ///
    /// Creating the directory is the caller's, because a writer that made one
    /// would have to guess whether a typo in the path was meant to make a tree.
    fn check_parent(&self, number: i64) -> Result<()> {
        let destination = PathBuf::from(self.template.render(number));
        let parent = destination
            .parent()
            .ok_or_else(|| ImgSeqError::new("the PNGWrite output_path has no directory"))?;
        if !parent.is_dir() {
            return Err(ImgSeqError::new(format!(
                "the PNGWrite output directory '{}' does not exist",
                parent.display()
            )));
        }
        Ok(())
    }
}

/// Validated arguments of one `PNGWrite` call.
struct WriteArgs {
    color: VideoNode,
    alpha: Option<VideoNode>,
    destinations: Destinations,
    always_save: bool,
    overwrite: bool,
    compression: u8,
    export_icc_profile: bool,
    debug: bool,
}

impl WriteArgs {
    fn read(input: &MapRef) -> Result<Self> {
        let color = input
            .get_video_node(key!(c"clip"), 0)
            .map_err(|error| map_error("clip", 0, error))?;
        let alpha = match input.get_video_node(key!(c"alpha"), 0) {
            Ok(alpha) => Some(alpha),
            Err(MapPropertyError::KeyNotFound) => None,
            Err(error) => return Err(map_error("alpha", 0, error)),
        };
        let path = input
            .get_utf8(key!(c"output_path"), 0)
            .map_err(|error| map_error("output_path", 0, error))?;

        let always_save =
            read_optional_int(input, key!(c"always_save"), "always_save")?.unwrap_or(0) != 0;
        let overwrite =
            read_optional_int(input, key!(c"overwrite"), "overwrite")?.unwrap_or(0) != 0;
        let export_icc_profile =
            read_optional_int(input, key!(c"icc_profile"), "icc_profile")?.unwrap_or(0) != 0;
        let debug = read_optional_int(input, key!(c"debug"), "debug")?.unwrap_or(0) != 0;
        let compression = read_optional_int(input, key!(c"compression"), "compression")?
            .unwrap_or(DEFAULT_COMPRESSION);
        if !(0..=9).contains(&compression) {
            return Err(ImgSeqError::new(format!(
                "compression must be between 0 and 9, got {compression}"
            )));
        }
        let start_number =
            read_optional_int(input, key!(c"start_number"), "start_number")?.unwrap_or(0);
        if start_number < 0 {
            return Err(ImgSeqError::new(format!(
                "start_number must not be negative, got {start_number}"
            )));
        }

        let info = color.info().clone();
        check_output_format(&info, alpha.as_ref().map(|alpha| alpha.info()))?;
        let destinations = Destinations::read(path, start_number, i64::from(info.num_frames))?;

        Ok(Self {
            color,
            alpha,
            destinations,
            always_save,
            overwrite,
            compression: u8::try_from(compression).expect("a level of 0 to 9 fits a byte"),
            export_icc_profile,
            debug,
        })
    }

    /// Keeps what frame requests need and drops the setup-only fields.
    fn into_writer(self) -> PNGWrite {
        PNGWrite {
            color: self.color,
            alpha: self.alpha,
            destinations: self.destinations,
            always_save: self.always_save,
            overwrite: self.overwrite,
            compression: self.compression,
            export_icc_profile: self.export_icc_profile,
            debug: self.debug,
            ledger: Mutex::new(Ledger::default()),
            counter: AtomicU64::new(0),
        }
    }
}

/// Refuses a clip whose format is not one this writer can hand out.
///
/// An undefined format is what a clip of varying frames reports, so it is left
/// for the frame check rather than refused here; everything else is a decision
/// that can be made before a frame is ever requested.
fn check_output_format(info: &VideoInfo, alpha: Option<&VideoInfo>) -> Result<()> {
    let format = &info.format;
    if format.color_family != ColorFamily::Undefined {
        if format.sample_type != SampleType::Integer {
            return Err(unsupported_family(format.color_family, format.sample_type));
        }
        match format.color_family {
            ColorFamily::Gray if format.num_planes == 1 => {}
            ColorFamily::RGB if format.num_planes == 3 => {}
            ColorFamily::YUV => {
                return Err(unsupported_family(format.color_family, format.sample_type));
            }
            family => {
                return Err(ImgSeqError::new(format!(
                    "PNGWrite writes a one plane gray or three plane rgb clip, got a {family:?} clip"
                )));
            }
        }
        if !(8..=16).contains(&format.bits_per_sample) {
            return Err(ImgSeqError::new(format!(
                "PNGWrite writes 8 to 16 bit integer frames, got a {} bit clip",
                format.bits_per_sample
            )));
        }
        if format.sub_sampling_w != 0 || format.sub_sampling_h != 0 {
            return Err(ImgSeqError::new(
                "PNGWrite writes a clip without chroma subsampling",
            ));
        }
    }
    if let Some(alpha) = alpha {
        let format = &alpha.format;
        if format.color_family != ColorFamily::Undefined {
            if format.color_family != ColorFamily::Gray || format.num_planes != 1 {
                return Err(ImgSeqError::new(
                    "PNGWrite needs a one plane gray alpha clip",
                ));
            }
            if format.sample_type != SampleType::Integer {
                return Err(ImgSeqError::new("PNGWrite needs an integer alpha clip"));
            }
            if info.format.color_family != ColorFamily::Undefined
                && format.bits_per_sample != info.format.bits_per_sample
            {
                return Err(ImgSeqError::new(format!(
                    "PNGWrite needs the alpha clip at the color clip's depth, got {} bit alpha beside {} bit color",
                    format.bits_per_sample, info.format.bits_per_sample
                )));
            }
        }
        if alpha.num_frames != info.num_frames {
            return Err(ImgSeqError::new(format!(
                "PNGWrite needs the alpha clip to have the same frame count, got {} frames beside {}",
                alpha.num_frames, info.num_frames
            )));
        }
        if info.format.color_family != ColorFamily::Undefined
            && alpha.format.color_family != ColorFamily::Undefined
            && (alpha.width != info.width || alpha.height != info.height)
        {
            return Err(ImgSeqError::new(format!(
                "PNGWrite needs the alpha clip at the color clip's size, got {}x{} beside {}x{}",
                alpha.width, alpha.height, info.width, info.height
            )));
        }
    }
    Ok(())
}

/// `PNGWrite` filter instance: writes one PNG per frame request.
pub struct PNGWrite {
    color: VideoNode,
    alpha: Option<VideoNode>,
    destinations: Destinations,
    always_save: bool,
    overwrite: bool,
    compression: u8,
    export_icc_profile: bool,
    debug: bool,
    /// Which frames this instance has published. Shared by the threads
    /// VapourSynth may complete frames on, and separate from the pixel cache
    /// because a frame going out of cache must not forget a finished write.
    ledger: Mutex<Ledger>,
    /// Names this instance's temporary files apart from its own earlier ones.
    counter: AtomicU64,
}

impl PNGWrite {
    /// Locks the ledger, ignoring a poisoning that no writer can act on.
    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A temporary file name beside `destination`, unique to this instance.
    ///
    /// The publication below needs the temporary file on the same file system
    /// as the destination, which is why it is named in the destination's own
    /// directory rather than in a temporary directory.
    fn temporary(&self, destination: &Path) -> PathBuf {
        let counter = self.counter.fetch_add(1, Ordering::Relaxed);
        let name = destination
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        destination.with_file_name(format!(".{name}.{}-{counter}.pngwrite", std::process::id()))
    }

    /// Produces one output frame, writing it first unless it is already saved.
    fn write_frame(&self, n: i32, ctx: &mut FrameContext, mut core: CoreRef) -> Result<VideoFrame> {
        let started = Instant::now();
        let color = self.color.get_frame_filter(n, ctx);
        let alpha = self
            .alpha
            .as_ref()
            .map(|alpha| alpha.get_frame_filter(n, ctx));
        let plan = FramePlan::of(&color, alpha.as_ref())?;
        let index =
            usize::try_from(n).map_err(|_| ImgSeqError::new(format!("invalid frame {n}")))?;
        let destination = self.destinations.of(index)?;

        // A frame this instance already published needs no second write, which
        // is what `always_save` turns off.
        let saved_before = !self.always_save && self.ledger().contains(index);
        let mut output = core.copy_frame(&color);
        if saved_before {
            set_receipts(&mut output, &destination, true, false)?;
            if self.debug {
                log_debug(
                    &mut core,
                    format_args!(
                        "PNGWrite frame {n} '{}': already saved, skipped ({})",
                        destination.display(),
                        format_duration(started.elapsed())
                    ),
                );
            }
            return Ok(output);
        }

        let icc = self
            .export_icc_profile
            .then(|| frame_icc_profile(&color))
            .flatten();
        let cicp = frame_cicp(&color);
        let encode_started = Instant::now();
        self.write_destination(
            &destination,
            &plan,
            &color,
            alpha.as_ref(),
            icc.as_deref(),
            cicp,
        )
        .map_err(|error| {
            ImgSeqError::new(format!(
                "PNGWrite could not write frame {n} to '{}': {error}",
                destination.display()
            ))
        })?;
        let encode = encode_started.elapsed();
        self.ledger().insert(index);
        set_receipts(&mut output, &destination, true, true)?;

        if self.debug {
            log_debug(
                &mut core,
                format_args!(
                    "PNGWrite frame {n} '{}': {}x{} {} at {} bit, compression={}, encode={} total={}",
                    destination.display(),
                    plan.width,
                    plan.height,
                    color_type_name(plan.color_type),
                    plan.bits,
                    self.compression,
                    format_duration(encode),
                    format_duration(started.elapsed())
                ),
            );
        }
        Ok(output)
    }

    /// Encodes one frame to `destination` through a temporary file.
    fn write_destination(
        &self,
        destination: &Path,
        plan: &FramePlan,
        color: &VideoFrame,
        alpha: Option<&VideoFrame>,
        icc: Option<&[u8]>,
        cicp: Option<[u8; 4]>,
    ) -> Result<()> {
        let temporary = self.temporary(destination);
        let outcome = self
            .encode_to(&temporary, plan, color, alpha, icc, cicp)
            .and_then(|()| {
                publish(&temporary, destination, self.overwrite).map_err(ImgSeqError::from_display)
            });
        if outcome.is_err() {
            // Only this writer's own temporary file is removed, and the
            // destination, if it exists, is left exactly as it was.
            let _ = fs::remove_file(&temporary);
        }
        outcome
    }

    /// Streams one frame into `temporary` and flushes it to the file system.
    fn encode_to(
        &self,
        temporary: &Path,
        plan: &FramePlan,
        color: &VideoFrame,
        alpha: Option<&VideoFrame>,
        icc: Option<&[u8]>,
        cicp: Option<[u8; 4]>,
    ) -> Result<()> {
        let file = File::create(temporary).map_err(|error| {
            ImgSeqError::new(format!(
                "cannot create the temporary file '{}': {error}",
                temporary.display()
            ))
        })?;
        let mut buffered = BufWriter::with_capacity(FILE_BUFFER, file);
        encode_png(
            &mut buffered,
            plan,
            color,
            alpha,
            self.compression,
            icc,
            cicp,
        )?;
        buffered.flush().map_err(ImgSeqError::from_display)?;
        // The file is closed before it is published, which is what Windows
        // needs before the rename and what makes an I/O error surface here
        // rather than after success was recorded.
        drop(buffered);
        Ok(())
    }
}

impl Filter for PNGWrite {
    type Error = ImgSeqError;
    type FrameType = VideoFrame;
    type FilterData = ();

    const NAME: &'static CStr = c"PNGWrite";
    const ARGS: &'static CStr = PNG_WRITE_ARGS;
    const RETURN_TYPE: &'static CStr = c"clip:vnode;";
    /// Frames may be requested and completed on several threads at once, so a
    /// consumer that pulls ahead (a `frames(prefetch=n)` walk, `get_frame_async`,
    /// or `vspipe`) gets several encodes in flight rather than one.
    ///
    /// Nothing here needs serializing for that to be correct: VapourSynth never
    /// calls this for the same frame number concurrently, the ledger is behind a
    /// mutex, the temporary file name is unique per write, and the publication is
    /// a rename or a linking create that cannot be raced. The workspace one
    /// encode holds is a row buffer and the compressor's own, which is
    /// width-bounded rather than frame-bounded.
    const FILTER_MODE: FilterMode = FilterMode::Parallel;

    fn create(
        input: MapRef,
        output: MapRef,
        _data: Option<Box<Self::FilterData>>,
        mut core: CoreRef,
    ) -> Result<()> {
        let args = WriteArgs::read(&input)?;
        let info = args.color.info().clone();
        let mut dependencies = Vec::with_capacity(2);
        for node in std::iter::once(&args.color).chain(args.alpha.iter()) {
            dependencies.push(ffi::VSFilterDependency {
                source: node.as_ptr(),
                request_pattern: ffi::VSRequestPattern::StrictSpatial,
            });
        }
        let dependencies =
            Dependencies::new(&dependencies).expect("a writer has one or two dependencies");
        if args.debug {
            let first = args.destinations.of(0)?;
            log_debug(
                &mut core,
                format_args!(
                    "PNGWrite create: frames={} alpha={} always_save={} overwrite={} compression={} first='{}'",
                    info.num_frames,
                    args.alpha.is_some(),
                    args.always_save,
                    args.overwrite,
                    args.compression,
                    first.display()
                ),
            );
        }
        core.create_video_filter(
            output,
            Self::NAME,
            &info,
            Box::new(args.into_writer()),
            dependencies,
        );
        Ok(())
    }

    fn get_frame(
        &self,
        n: i32,
        activation_reason: ffi::VSActivationReason,
        _frame_data: *mut *mut c_void,
        mut frame_ctx: FrameContext,
        core: CoreRef,
    ) -> Result<Option<Self::FrameType>> {
        match activation_reason {
            // The two stage request is what makes this a writer rather than a
            // source: the frame is asked for here and the writing happens once
            // the upstream graph has produced it.
            ffi::VSActivationReason::Initial => {
                frame_ctx.request_frame_filter(n, &self.color);
                if let Some(alpha) = &self.alpha {
                    frame_ctx.request_frame_filter(n, alpha);
                }
                Ok(None)
            }
            ffi::VSActivationReason::AllFramesReady => {
                self.write_frame(n, &mut frame_ctx, core).map(Some)
            }
            _ => Ok(None),
        }
    }
}

/// Writes one frame as a PNG into `sink`.
fn encode_png<W: Write>(
    sink: W,
    plan: &FramePlan,
    color: &VideoFrame,
    alpha: Option<&VideoFrame>,
    compression: u8,
    icc: Option<&[u8]>,
    cicp: Option<[u8; 4]>,
) -> Result<()> {
    let mut info: Info<'_> = Info::with_size(
        u32::try_from(plan.width).map_err(|_| ImgSeqError::new("the frame is too wide for PNG"))?,
        u32::try_from(plan.height)
            .map_err(|_| ImgSeqError::new("the frame is too tall for PNG"))?,
    );
    info.color_type = plan.color_type;
    info.bit_depth = plan.bit_depth;
    if let Some(icc) = icc {
        info.icc_profile = Some(Cow::Borrowed(icc));
    }

    let mut encoder = Encoder::with_info(sink, info).map_err(ImgSeqError::from_display)?;
    encoder.set_deflate_compression(match compression {
        0 => DeflateCompression::NoCompression,
        level => DeflateCompression::Level(level),
    });
    if compression == 0 {
        // Every level is lossless and only the effort changes, but the level
        // that means "store it" also means "do not filter it".
        encoder.set_filter(PngFilter::NoFilter);
    }
    let mut writer = encoder.write_header().map_err(ImgSeqError::from_display)?;
    // `png` reads these two fields of `Info` but writes neither chunk, so a
    // caller that wants the source precision or the colour statement named has
    // to put the chunks in itself. Both must precede IDAT, which the stream
    // writer below starts.
    if let Some(sbit) = significant_bits(plan) {
        writer
            .write_chunk(png::chunk::sBIT, &sbit)
            .map_err(ImgSeqError::from_display)?;
    }
    if let Some(cicp) = cicp {
        writer
            .write_chunk(png::chunk::cICP, &cicp)
            .map_err(ImgSeqError::from_display)?;
    }

    let mut stream = writer.stream_writer().map_err(ImgSeqError::from_display)?;
    let mut row = vec![0u8; plan.row_bytes()?];
    for y in 0..plan.height {
        pack_row(plan, color, alpha, y, &mut row);
        stream.write_all(&row).map_err(ImgSeqError::from_display)?;
    }
    // Finishing the stream writes the last compressed block; finishing the
    // parent writer writes IEND and flushes, which is where a compression or I/O
    // error a drop would swallow is reported instead.
    stream.finish().map_err(ImgSeqError::from_display)?;
    writer.finish().map_err(ImgSeqError::from_display)?;
    Ok(())
}

/// The `sBIT` payload of one frame, or `None` when nothing was lost.
///
/// The chunk states how many of the stored bits are meaningful. A sixteen bit
/// frame needs none, because the PNG depth is the frame's own; a frame of nine
/// to fifteen bits does, because the sixteen bit PNG it becomes holds repeated
/// low bits that are not source information.
fn significant_bits(plan: &FramePlan) -> Option<Vec<u8>> {
    if !plan.spread {
        return None;
    }
    let bits = u8::try_from(plan.bits).ok()?;
    let mut sbit = Vec::with_capacity(plan.channels());
    for _ in 0..plan.color_planes {
        sbit.push(bits);
    }
    if plan.alpha {
        sbit.push(bits);
    }
    Some(sbit)
}

/// Packs one frame row of every plane into one PNG row.
fn pack_row(
    plan: &FramePlan,
    color: &VideoFrame,
    alpha: Option<&VideoFrame>,
    y: usize,
    row: &mut [u8],
) {
    let channels = plan.channels();
    let bytes = plan.sample_bytes();
    for plane in 0..plan.color_planes {
        let source = color.plane(plane as i32);
        let stride = color.stride(plane as i32) as usize;
        // SAFETY: the row index is inside the frame the plan was built from,
        // and the plane's own stride is what addresses it.
        let source = unsafe { source.add(y * stride) };
        pack_plane(row, plane, channels, plan, source, bytes);
    }
    if let (Some(alpha), true) = (alpha, plan.alpha) {
        let source = alpha.plane(0);
        let stride = alpha.stride(0) as usize;
        // SAFETY: as above, for the alpha frame the plan validated.
        let source = unsafe { source.add(y * stride) };
        pack_plane(row, plan.color_planes, channels, plan, source, bytes);
    }
}

/// Interleaves one plane's row into the PNG row at `channel`.
fn pack_plane(
    row: &mut [u8],
    channel: usize,
    channels: usize,
    plan: &FramePlan,
    source: *const u8,
    bytes: usize,
) {
    let width = plan.width;
    if bytes == 1 {
        for x in 0..width {
            // SAFETY: the plan's width is the plane's own width, so every
            // sample read is inside the row the caller addressed.
            row[x * channels + channel] = unsafe { *source.add(x) };
        }
        return;
    }
    let spread = plan.spread;
    let bits = u32::try_from(plan.bits).unwrap_or(16);
    for x in 0..width {
        // SAFETY: as above, two bytes a sample.
        let sample = unsafe { u16::from_ne_bytes([*source.add(2 * x), *source.add(2 * x + 1)]) };
        let stored = if spread { widen(sample, bits) } else { sample };
        let at = (x * channels + channel) * 2;
        row[at..at + 2].copy_from_slice(&stored.to_be_bytes());
    }
}

/// The `cICP` payload a frame's colour properties describe, if any.
///
/// PNG states matrices with the field fixed at zero, so a frame whose `_Matrix`
/// names a conversion is one this chunk cannot describe and is left alone; the
/// same goes for a frame whose samples are not full range, because the chunk's
/// flag would then describe pixels the file does not hold. A frame that leaves
/// both unset gets the chunk whenever it states primaries and transfer, since
/// those two are exactly what the file's samples mean.
fn frame_cicp(frame: &VideoFrame) -> Option<[u8; 4]> {
    let primaries = u8::try_from(property_int(frame, key!(c"_Primaries"))?).ok()?;
    let transfer = u8::try_from(property_int(frame, key!(c"_Transfer"))?).ok()?;
    if primaries == UNSPECIFIED || transfer == UNSPECIFIED {
        return None;
    }
    if property_int(frame, key!(c"_Matrix")).is_some_and(|matrix| matrix != 0) {
        return None;
    }
    if property_int(frame, key!(c"_Range")).is_some_and(|range| range != 1) {
        return None;
    }
    Some([primaries, transfer, 0, 1])
}

/// The embedded profile of a frame, when it has one.
fn frame_icc_profile(frame: &VideoFrame) -> Option<Vec<u8>> {
    frame
        .properties()?
        .get_binary(key!(c"ICCProfile"), 0)
        .ok()
        .map(<[u8]>::to_vec)
}

/// Reads one integer frame property, treating a missing one as unset.
fn property_int(frame: &VideoFrame, key: &KeyStr) -> Option<i64> {
    frame.properties()?.get_int(key, 0).ok()
}

/// Publishes a finished temporary file as `destination`.
///
/// `overwrite` is a rename, which replaces whatever was there. Without it the
/// destination is created by linking, which is the one primitive that both
/// refuses to clobber an existing file and cannot be raced by another writer
/// between a check and a create. A file system without links falls back to an
/// exclusively created destination and a copy: it still never clobbers, but a
/// failure part way through leaves a partial file behind, which is why the link
/// is tried first.
fn publish(temporary: &Path, destination: &Path, overwrite: bool) -> io::Result<()> {
    if overwrite {
        return fs::rename(temporary, destination);
    }
    match fs::hard_link(temporary, destination) {
        Ok(()) => {
            let _ = fs::remove_file(temporary);
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
        Err(_) => {
            let mut source = File::open(temporary)?;
            let mut target = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?;
            io::copy(&mut source, &mut target)?;
            target.flush()?;
            drop(target);
            drop(source);
            let _ = fs::remove_file(temporary);
            Ok(())
        }
    }
}

/// Reports what one evaluation did with the frame it produced.
fn set_receipts(
    frame: &mut VideoFrame,
    destination: &Path,
    saved: bool,
    performed: bool,
) -> Result<()> {
    let Some(mut properties) = frame.properties_mut() else {
        return Err(ImgSeqError::new("VapourSynth frame has no property map"));
    };
    let path = destination.to_string_lossy();
    properties
        .set(
            key!(c"ImgSeqPNGWritePath"),
            Value::Utf8(&path),
            AppendMode::Replace,
        )
        .and_then(|()| {
            properties.set(
                key!(c"ImgSeqPNGWriteSaved"),
                Value::Int(i64::from(saved)),
                AppendMode::Replace,
            )
        })
        .and_then(|()| {
            properties.set(
                key!(c"ImgSeqPNGWritePerformed"),
                Value::Int(i64::from(performed)),
                AppendMode::Replace,
            )
        })
        .map_err(ImgSeqError::from_display)
}

/// The name of a PNG color type, for the log.
const fn color_type_name(color_type: ColorType) -> &'static str {
    match color_type {
        ColorType::Grayscale => "gray",
        ColorType::GrayscaleAlpha => "gray+alpha",
        ColorType::Rgb => "rgb",
        ColorType::Rgba => "rgba",
        _ => "other",
    }
}

/// Reads one optional integer argument, distinguishing absent from zero.
fn read_optional_int(input: &MapRef, key: &KeyStr, name: &str) -> Result<Option<i64>> {
    match input.get_int(key, 0) {
        Ok(value) => Ok(Some(value)),
        Err(MapPropertyError::KeyNotFound) => Ok(None),
        Err(error) => Err(map_error(name, 0, error)),
    }
}

fn map_error(key: &str, index: i32, error: MapPropertyError) -> ImgSeqError {
    ImgSeqError::new(format!("invalid {key}[{index}] argument: {error}"))
}

fn log_debug(core: &mut CoreRef<'_>, message: impl std::fmt::Display) {
    let Ok(message) = std::ffi::CString::new(format!("[imgseqs][debug] {message}")) else {
        return;
    };
    core.log(ffi::VSMessageType::Information, &message);
}

fn format_duration(duration: Duration) -> String {
    format!("{:.3} ms", duration.as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_number_substitutions() {
        let template = Template::parse("frame%06d.png").unwrap();
        assert!(template.numbered());
        assert_eq!(template.render(42), "frame000042.png");
        assert_eq!(template.render(1_000_000), "frame1000000.png");

        let plain = Template::parse("page%d.png").unwrap();
        assert_eq!(plain.render(7), "page7.png");

        let escaped = Template::parse("100%%%04d.png").unwrap();
        assert_eq!(escaped.render(3), "100%0003.png");

        let literal = Template::parse("cover.png").unwrap();
        assert!(!literal.numbered());
        assert_eq!(literal.render(0), "cover.png");
    }

    #[test]
    fn refuses_a_substitution_it_cannot_render() {
        for text in ["a%sd.png", "a%", "a%x.png", "a%6d.png"] {
            assert!(Template::parse(text).is_err(), "{text} was accepted");
        }
        assert!(Template::parse(&format!("a%0{}d.png", MAX_PADDING + 1)).is_err());
        assert!(Template::parse(&format!("a%0{}d.png", MAX_PADDING)).is_ok());
        // A padding wider than a number can be written into is a typo, not a
        // silent fallback to an unpadded number.
        assert!(Template::parse("a%099999999999999999999999999d.png").is_err());
        // `%0d` asks for zero padding of no width, which is `%d`.
        assert_eq!(Template::parse("a%0d.png").unwrap().render(4), "a4.png");
    }

    #[test]
    fn spreads_every_sample_of_every_intermediate_depth() {
        // The formula has to be exact at both ends for every depth the scope
        // names: the smallest sample stays the smallest, the largest is the
        // full scale, and the source value is recoverable from the high bits.
        for bits in 9..=15u32 {
            let maximum = (1u32 << bits) - 1;
            assert_eq!(widen(0, bits), 0, "{bits} bit zero");
            assert_eq!(widen(maximum as u16, bits), u16::MAX, "{bits} bit maximum");
            for sample in 0..=maximum as u16 {
                let wide = widen(sample, bits);
                assert_eq!(wide >> (16 - bits), sample, "{bits} bit round trip");
                // The low bits repeat the sample's own high bits, which is
                // what makes the widening the exact scaling of the sample
                // range rather than a shift that leaves white short.
                let low = wide & ((1 << (16 - bits)) - 1);
                assert_eq!(low, sample >> (2 * bits - 16), "{bits} bit replication");
            }
        }
    }

    #[test]
    fn a_sixteen_bit_frame_needs_no_spread() {
        assert_eq!(png_depth(8), BitDepth::Eight);
        assert_eq!(png_depth(16), BitDepth::Sixteen);
        let plan = FramePlan {
            color_type: ColorType::Rgb,
            bit_depth: BitDepth::Sixteen,
            bits: 16,
            spread: false,
            color_planes: 3,
            alpha: false,
            width: 4,
            height: 2,
        };
        assert!(significant_bits(&plan).is_none());
        let spread = FramePlan {
            bits: 10,
            spread: true,
            ..plan
        };
        assert_eq!(significant_bits(&spread), Some(vec![10, 10, 10]));
    }

    #[test]
    fn a_ledger_remembers_only_what_it_was_given() {
        let mut ledger = Ledger::default();
        assert!(!ledger.contains(0));
        assert!(!ledger.contains(4096));
        ledger.insert(4096);
        assert!(ledger.contains(4096));
        assert!(!ledger.contains(4095));
        assert!(!ledger.contains(4097));
        // A frame number far into a clip must not allocate for the frames
        // before it that were never written.
        assert_eq!(ledger.bits.len(), 4096 / 64 + 1);
    }

    #[test]
    fn destinations_check_the_number_and_the_directory() {
        let directory = std::env::temp_dir();
        let text = directory.join("imgseqs-writer-%03d.png");
        let destinations = Destinations::read(&text.to_string_lossy(), 5, 3).unwrap();
        let first = destinations.of(0).unwrap();
        assert!(first.to_string_lossy().ends_with("imgseqs-writer-005.png"));
        let last = destinations.of(2).unwrap();
        assert!(last.to_string_lossy().ends_with("imgseqs-writer-007.png"));

        let missing = directory.join("imgseqs-writer-missing").join("%03d.png");
        assert!(Destinations::read(&missing.to_string_lossy(), 0, 1).is_err());
        // A literal destination names one frame, so a sequence is refused.
        let literal = directory.join("imgseqs-writer.png");
        assert!(Destinations::read(&literal.to_string_lossy(), 0, 1).is_ok());
        assert!(Destinations::read(&literal.to_string_lossy(), 0, 2).is_err());
        assert!(Destinations::read("", 0, 1).is_err());
    }
}
