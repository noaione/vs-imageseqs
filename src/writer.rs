//! `PNGWrite`: request-driven PNG export.
//!
//! `PNGWrite` is the other end of `Read`. It is a video node whose frames are
//! the frames of the clip it was given, so a graph can sit behind it unchanged,
//! and evaluating one of its frames writes that frame to a PNG before the
//! request returns. Creating the node writes nothing: a frame that is never
//! requested is never written.
//!
//! A PNG holds integer gray, r,g,b or an index into a palette, so a frame that
//! is yuv or float has to be converted before it can be written. That
//! conversion is done here rather than left to the caller, because the readers
//! hand a lossy webp, a colour avif and a colour heic out as the yuv planes
//! their container coded: writing one of those to a PNG is the ordinary case.
//! What the conversion does, and every choice it makes, is `crate::convert`'s
//! to state.
//!
//! An integer gray or rgb frame of nine to fifteen bits is written as a sixteen
//! bit PNG whose samples carry the source precision in their high bits, which is
//! exact rather than approximate and needs no conversion stage. The `depth`
//! argument asks for another word instead: a lower one gives up precision by
//! rounding, and one below eight bits packs a gray frame whose samples are
//! already the codes that word holds.
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

use crate::{
    convert::{self, Converter, Plane},
    error::{ImgSeqError, Result},
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

/// Arguments accepted by `PNGWrite`.
const PNG_WRITE_ARGS: &CStr = c"clip:vnode;output_path:data;alpha:vnode:opt;always_save:int:opt;compression:int:opt;overwrite:int:opt;start_number:int:opt;icc_profile:int:opt;depth:int:opt;matrix:int:opt;debug:int:opt;";

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

/// The `png` bit depth that names `bits` stored bits a sample.
///
/// Only eight and sixteen are depths a PNG row can hold a whole sample in; one,
/// two and four pack several samples into a byte, and the encoder says which by
/// the same number.
const fn bit_depth_of(bits: u32) -> BitDepth {
    match bits {
        1 => BitDepth::One,
        2 => BitDepth::Two,
        4 => BitDepth::Four,
        8 => BitDepth::Eight,
        _ => BitDepth::Sixteen,
    }
}

/// Spreads a sample of `bits` bits across a sixteen bit word.
///
/// The high bits are the sample itself and the low ones repeat it, so the
/// largest sample of `bits` bits becomes `u16::MAX` exactly: a ten bit picture
/// whose white is 1023 does not come out at 98% of the scale, and a ten bit
/// alpha of 1023 is fully opaque. `sample >> (16 - bits)` recovers the source
/// value without loss.
///
/// It is the exact scale-up for every depth from eight to sixteen, so an eight
/// bit frame asked for at `depth=16` becomes a sixteen bit PNG of the same
/// picture rather than one whose samples sit in the low half of their word.
#[must_use]
const fn widen(sample: u16, bits: u32) -> u16 {
    let shift = 16 - bits;
    let high = (sample as u32) << shift;
    let low = (sample as u32) >> (2 * bits - 16);
    (high | low) as u16
}

/// Moves a sample of `from` bits down to `to` bits by rounding.
///
/// The value is scaled across the two ranges rather than shifted out of the top
/// of its word, so the largest sample stays the largest one: a sixteen bit frame
/// written as an eight bit PNG keeps its white white, and a ten bit one keeps
/// its whole range rather than stopping short of the end of the smaller word.
#[must_use]
fn reduce(sample: u16, from: u32, to: u32) -> u16 {
    let source = (1u32 << from) - 1;
    let target = (1u32 << to) - 1;
    let scaled = (u32::from(sample) * target * 2 + source) / (2 * source);
    u16::try_from(scaled).unwrap_or(u16::MAX)
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
#[derive(Debug)]
struct FramePlan {
    /// The layout the PNG states.
    color_type: ColorType,
    bit_depth: BitDepth,
    /// Bits of one colour sample before it is stored, which is what `sBIT`
    /// states when the stored word is wider than it.
    bits: u32,
    /// How a colour sample of `bits` bits reaches the stored word.
    storage: Storage,
    /// Where the colour samples of a row come from.
    source: Source,
    /// Planes of the colour frame: one for gray, three for rgb or yuv.
    color_planes: usize,
    /// The alpha clip's own precision, which the one word PNG gives every
    /// sample has to share rather than share a value with.
    alpha: Option<Alpha>,
    width: usize,
    height: usize,
}

/// How a sample of one precision reaches the word the PNG stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Storage {
    /// The stored word is the sample's own precision.
    Direct,
    /// The stored word is wider, so the sample is spread across it.
    Widen,
    /// The stored word is narrower, so the sample is scaled down by rounding.
    Reduce,
}

/// How a sample of `bits` bits reaches a `stored` bit word.
const fn storage_of(bits: u32, stored: u32) -> Storage {
    if bits == stored {
        Storage::Direct
    } else if bits < stored {
        Storage::Widen
    } else {
        Storage::Reduce
    }
}

/// The alpha clip of a plan.
#[derive(Debug, Clone, Copy)]
struct Alpha {
    bits: u32,
    storage: Storage,
}

/// Where the colour samples of a row come from.
#[derive(Debug)]
enum Source {
    /// Integer samples read straight out of the frame's own planes, already at
    /// the frame's own precision.
    Planes,
    /// Float planes whose samples are values in `[0, 1]`.
    Float,
    /// Yuv planes a conversion turns into r, g and b.
    Yuv(Box<Converter>),
}

impl Source {
    /// The word for the log: what the samples were before they were stored.
    const fn name(&self) -> &'static str {
        match self {
            Self::Planes => "read",
            Self::Float => "float",
            Self::Yuv(_) => "yuv",
        }
    }
}

/// The kind of samples a format holds, which is what decides whether there is
/// anything to convert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// An integer gray or rgb format, which is stored as it stands.
    Integer,
    /// A float format, whose samples are values rather than codes.
    Float,
    /// An integer yuv format, which has to become r, g and b.
    Yuv,
}

/// The kind of samples `format` holds and how many colour planes that is.
///
/// `None` for a format VapourSynth calls undefined, which is what a clip of
/// frames of different formats reports: only a frame can settle what such a
/// clip holds, so the frame check asks again. Everything else is refused here,
/// before a frame is ever requested, because the format already settles it.
fn kind_of(format: &ffi::VSVideoFormat) -> Result<Option<(Kind, usize)>> {
    if format.color_family == ColorFamily::Undefined {
        return Ok(None);
    }
    let planes = match format.color_family {
        ColorFamily::Gray if format.num_planes == 1 => 1,
        ColorFamily::RGB if format.num_planes == 3 => 3,
        ColorFamily::YUV if format.num_planes == 3 => 3,
        ColorFamily::Gray | ColorFamily::RGB | ColorFamily::YUV => {
            return Err(ImgSeqError::new(format!(
                "PNGWrite needs a one plane gray or three plane rgb or yuv frame, got {} planes",
                format.num_planes
            )));
        }
        family => return Err(unsupported_family(family, format.sample_type)),
    };
    let subsampled = format.sub_sampling_w != 0 || format.sub_sampling_h != 0;
    if subsampled && format.color_family != ColorFamily::YUV {
        return Err(ImgSeqError::new(
            "PNGWrite needs a gray or rgb frame without chroma subsampling",
        ));
    }
    if !(0..=1).contains(&format.sub_sampling_w) || !(0..=1).contains(&format.sub_sampling_h) {
        return Err(ImgSeqError::new(format!(
            "PNGWrite converts 4:4:4, 4:2:2 and 4:2:0 yuv frames, and this one subsamples by {} and {} bits an axis",
            format.sub_sampling_w, format.sub_sampling_h
        )));
    }
    let kind = match (format.color_family, format.sample_type) {
        (ColorFamily::YUV, SampleType::Integer) => Kind::Yuv,
        (ColorFamily::YUV, SampleType::Float) => {
            return Err(ImgSeqError::new(
                "PNGWrite converts an integer yuv frame, and a float one holds no matrix it could read: convert it upstream with core.resize.Bicubic(clip, format=vs.RGBS, matrix_in_s=...)",
            ));
        }
        (_, SampleType::Float) => Kind::Float,
        (_, SampleType::Integer) => Kind::Integer,
    };
    match kind {
        Kind::Float if format.bits_per_sample != 32 => {
            return Err(ImgSeqError::new(format!(
                "PNGWrite reads a float frame as the 32 bit samples VapourSynth stores, and this one is {} bits",
                format.bits_per_sample
            )));
        }
        Kind::Float => {}
        _ if !(8..=16).contains(&format.bits_per_sample) => {
            return Err(ImgSeqError::new(format!(
                "PNGWrite writes 8 to 16 bit integer frames, got {} bit samples",
                format.bits_per_sample
            )));
        }
        _ => {}
    }
    Ok(Some((kind, planes)))
}

/// Validates the alpha clip's format against the colour frame's.
///
/// PNG gives every sample of a pixel one word, so an alpha at another depth
/// would need a conversion of its own; requiring the colour clip's own depth
/// and sample type is what keeps that decision out of this writer.
fn check_alpha(colour: &ffi::VSVideoFormat, alpha: &ffi::VSVideoFormat) -> Result<()> {
    if alpha.color_family != ColorFamily::Undefined {
        if alpha.color_family != ColorFamily::Gray || alpha.num_planes != 1 {
            return Err(ImgSeqError::new(
                "PNGWrite needs a one plane gray alpha clip",
            ));
        }
        if alpha.sample_type != colour.sample_type {
            return Err(ImgSeqError::new(format!(
                "PNGWrite needs the alpha clip to hold {} samples like the color clip",
                if colour.sample_type == SampleType::Float {
                    "float"
                } else {
                    "integer"
                }
            )));
        }
        if colour.sample_type == SampleType::Integer
            && alpha.bits_per_sample != colour.bits_per_sample
        {
            return Err(ImgSeqError::new(format!(
                "PNGWrite needs the alpha clip at the color clip's depth, got {} bit alpha beside {} bit color",
                alpha.bits_per_sample, colour.bits_per_sample
            )));
        }
    }
    Ok(())
}

/// The matrix a yuv frame is read with.
///
/// The caller's `matrix` argument wins when it is given, which is what
/// `resize`'s own `matrix_in` does and is the only way to correct a frame whose
/// container states a matrix the picture was not coded with. Otherwise it is
/// the frame's own `_Matrix`, and a frame that states none, or states
/// `unspecified`, is refused rather than converted under a default this writer
/// picked.
fn yuv_matrix(
    frame: &VideoFrame,
    given: Option<ffi::VSMatrixCoefficients>,
) -> Result<ffi::VSMatrixCoefficients> {
    if let Some(matrix) = given {
        return Ok(matrix);
    }
    match property_int(frame, key!(c"_Matrix"))
        .map(convert::matrix_of)
        .transpose()?
    {
        Some(matrix) if matrix != ffi::VSMatrixCoefficients::VSC_MATRIX_UNSPECIFIED => Ok(matrix),
        _ => Err(ImgSeqError::new(
            "PNGWrite needs to know the matrix this yuv frame was coded with and it states none; pass matrix= with the h.273 code the file's own colour metadata names, or convert it upstream with core.resize.Bicubic(clip, format=vs.RGB24, matrix_in_s=\"601\")",
        )),
    }
}

/// The chroma sample position a frame states, when it names one.
///
/// The property is VapourSynth's own code point, so this is a lookup rather
/// than a translation, and a code the enum has no name for is no statement.
fn chroma_location(frame: &VideoFrame) -> Option<ffi::VSChromaLocation> {
    use ffi::VSChromaLocation as C;
    match property_int(frame, key!(c"_ChromaLocation"))? {
        0 => Some(C::VSC_CHROMA_LEFT),
        1 => Some(C::VSC_CHROMA_CENTER),
        2 => Some(C::VSC_CHROMA_TOP_LEFT),
        3 => Some(C::VSC_CHROMA_TOP),
        4 => Some(C::VSC_CHROMA_BOTTOM_LEFT),
        5 => Some(C::VSC_CHROMA_BOTTOM),
        _ => None,
    }
}

impl FramePlan {
    /// Validates the two frames of one request against the supported scope.
    ///
    /// `depth` is the caller's `depth` argument: `None` writes at the frame's
    /// own precision, and a number names the word the PNG stores instead.
    /// `matrix` is its `matrix` argument, which is what a yuv frame that
    /// states no usable matrix of its own is read with.
    fn of(
        color: &VideoFrame,
        alpha: Option<&VideoFrame>,
        depth: Option<u32>,
        matrix: Option<ffi::VSMatrixCoefficients>,
    ) -> Result<Self> {
        let format = color.get_video_format();
        let (kind, color_planes) = kind_of(format)?.ok_or_else(|| {
            ImgSeqError::new(
                "PNGWrite needs a frame whose format its clip states, and this one is undefined",
            )
        })?;
        // The precision the frame's own samples carry. A float sample is a
        // value rather than a code, so what it carries is the whole of the
        // word it is about to be stored in.
        let source_bits = if kind == Kind::Float {
            16
        } else {
            u32::try_from(format.bits_per_sample)
                .map_err(|_| ImgSeqError::new("PNGWrite got a frame of a negative depth"))?
        };
        let stored = depth.unwrap_or(if source_bits > 8 { 16 } else { 8 });
        // A word below eight bits is one gray plane with no alpha, which is the
        // only shape png packs that way. The samples are scaled into it like
        // every other depth rather than having to be its codes already, which is
        // what makes writing back the picture a one bit reader expanded work.
        if stored < 8 && (color_planes != 1 || alpha.is_some()) {
            return Err(ImgSeqError::new(format!(
                "PNGWrite stores {stored} bits a sample as one gray plane with no alpha, which this frame is not: write it at 8 or 16 bits, or convert it first"
            )));
        }

        let width = plane_size(color.frame_width(0), "width")?;
        let height = plane_size(color.frame_height(0), "height")?;
        for plane in 0..color_planes {
            // A yuv frame's chroma planes are the luma one divided down, which
            // is the shape the conversion reads them at; every other family has
            // planes of one size.
            let (across, down) = if format.color_family == ColorFamily::YUV && plane > 0 {
                (
                    1usize << format.sub_sampling_w,
                    1usize << format.sub_sampling_h,
                )
            } else {
                (1, 1)
            };
            let (across, down) = (width / across, height / down);
            let (plane_width, plane_height) = (
                color.frame_width(plane as i32),
                color.frame_height(plane as i32),
            );
            if plane_width as usize != across || plane_height as usize != down {
                return Err(ImgSeqError::new(format!(
                    "PNGWrite needs a frame whose planes are the size its format states, and plane {plane} is {plane_width}x{plane_height} rather than {across}x{down}"
                )));
            }
        }

        // The precision the samples the PNG stores carry. An integer plane's
        // samples carry the frame's own, and the storage below is what moves
        // them to the word the PNG holds. A conversion quantises to the
        // precision the frame has and never to more, so a ten bit frame written
        // as a sixteen bit PNG keeps ten bits of picture, which is what lets
        // `sBIT` say so.
        let bits = if kind == Kind::Integer {
            source_bits
        } else {
            source_bits.min(stored)
        };
        let source = match kind {
            Kind::Integer => Source::Planes,
            Kind::Float => Source::Float,
            Kind::Yuv => Source::Yuv(Box::new(Converter::new(
                format.bits_per_sample,
                bits,
                (format.sub_sampling_w, format.sub_sampling_h),
                width,
                height,
                yuv_matrix(color, matrix)?,
                property_int(color, key!(c"_Range")) == Some(1),
                chroma_location(color),
            )?)),
        };

        let alpha = match alpha {
            None => None,
            Some(alpha) => {
                let alpha_format = alpha.get_video_format();
                check_alpha(format, alpha_format)?;
                let (alpha_width, alpha_height) = (alpha.frame_width(0), alpha.frame_height(0));
                if alpha_width as usize != width || alpha_height as usize != height {
                    return Err(ImgSeqError::new(format!(
                        "PNGWrite needs the alpha clip at the color clip's size, got {alpha_width}x{alpha_height} beside {width}x{height}"
                    )));
                }
                let bits = if alpha_format.sample_type == SampleType::Float {
                    source_bits.min(stored)
                } else {
                    u32::try_from(alpha_format.bits_per_sample).unwrap_or(source_bits)
                };
                Some(Alpha {
                    bits,
                    storage: storage_of(bits, stored),
                })
            }
        };

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

        Ok(Self {
            color_type,
            bit_depth: bit_depth_of(stored),
            bits,
            storage: storage_of(bits, stored),
            source,
            color_planes,
            alpha,
            width,
            height,
        })
    }

    /// Samples one output pixel holds, which is what the PNG row is made of.
    const fn channels(&self) -> usize {
        self.color_planes + self.alpha.is_some() as usize
    }

    /// Bits one stored sample holds in the PNG.
    const fn stored_bits(&self) -> u32 {
        self.bit_depth as u32
    }

    /// Bytes one stored sample holds, which is zero when a row packs several of
    /// them into a byte.
    const fn sample_bytes(&self) -> usize {
        match self.bit_depth {
            BitDepth::Eight => 1,
            BitDepth::Sixteen => 2,
            _ => 0,
        }
    }

    /// Bytes one PNG row holds.
    ///
    /// A depth of eight or sixteen is a whole number of bytes a sample; one, two
    /// and four pack the row's samples into bytes most significant first, and
    /// PNG pads the last byte of a row rather than the row itself.
    fn row_bytes(&self) -> Result<usize> {
        self.width
            .checked_mul(self.channels())
            .and_then(|samples| samples.checked_mul(self.stored_bits() as usize))
            .map(|bits| bits.div_ceil(8))
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
    /// The word the PNG stores, when the caller named one.
    depth: Option<u32>,
    /// The matrix a yuv frame that states none is read with.
    matrix: Option<ffi::VSMatrixCoefficients>,
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
        let depth = read_optional_int(input, key!(c"depth"), "depth")?
            .map(|depth| {
                u32::try_from(depth)
                    .ok()
                    .filter(|depth| matches!(depth, 1 | 2 | 4 | 8 | 16))
                    .ok_or_else(|| {
                        ImgSeqError::new(format!(
                            "the PNGWrite depth must be 1, 2, 4, 8 or 16, got {depth}"
                        ))
                    })
            })
            .transpose()?;
        let matrix = read_optional_int(input, key!(c"matrix"), "matrix")?
            .map(convert::matrix_of)
            .transpose()?;

        let info = color.info().clone();
        check_output_format(&info, alpha.as_ref().map(|alpha| alpha.info()), depth)?;
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
            depth,
            matrix,
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
            depth: self.depth,
            matrix: self.matrix,
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
fn check_output_format(
    info: &VideoInfo,
    alpha: Option<&VideoInfo>,
    depth: Option<u32>,
) -> Result<()> {
    // A depth below eight bits is one gray plane with no alpha, and nothing
    // here chooses the threshold or the palette that would reach one, so a clip
    // that is not already that shape is refused before a frame is requested.
    if let Some(depth) = depth
        && depth < 8
        && info.format.color_family != ColorFamily::Undefined
        && (info.format.color_family != ColorFamily::Gray
            || info.format.num_planes != 1
            || alpha.is_some())
    {
        return Err(ImgSeqError::new(format!(
            "PNGWrite stores {depth} bits a sample as one gray plane with no alpha, which this clip is not: write it at 8 or 16 bits, or convert it first"
        )));
    }
    // The rest of the format is what a frame check would ask again, so a clip
    // that can never be stored is refused here where the message can name the
    // clip rather than a frame.
    kind_of(&info.format)?;
    let Some(alpha) = alpha else {
        return Ok(());
    };
    if alpha.num_frames != info.num_frames {
        return Err(ImgSeqError::new(format!(
            "PNGWrite needs the alpha clip to have the same frame count, got {} frames beside {}",
            alpha.num_frames, info.num_frames
        )));
    }
    if info.format.color_family != ColorFamily::Undefined
        && alpha.format.color_family != ColorFamily::Undefined
    {
        check_alpha(&info.format, &alpha.format)?;
        if alpha.width != info.width || alpha.height != info.height {
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
    /// The word the PNG stores, when the caller named one.
    depth: Option<u32>,
    /// The matrix a yuv frame that states none is read with.
    matrix: Option<ffi::VSMatrixCoefficients>,
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
        let plan = FramePlan::of(&color, alpha.as_ref(), self.depth, self.matrix)?;
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
        let cicp = frame_cicp(&color, matches!(plan.source, Source::Yuv(_)));
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
                    "PNGWrite frame {n} '{}': {}x{} {} at {} bit from {}, compression={}, encode={} total={}",
                    destination.display(),
                    plan.width,
                    plan.height,
                    color_type_name(plan.color_type),
                    plan.bits,
                    plan.source.name(),
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
    // A conversion writes the samples it makes here and the row storage moves
    // them into the PNG. The buffer belongs to the caller because a row is not
    // the place to allocate one, and it serves every row of the frame.
    let mut staging = vec![0u16; plan.width * plan.color_planes];
    for y in 0..plan.height {
        // A row that packs several samples into a byte shares those bytes
        // between them, so it starts from nothing; a row of whole samples
        // writes every byte it has.
        if plan.sample_bytes() == 0 {
            row.fill(0);
        }
        pack_row(plan, color, alpha, y, &mut row, &mut staging);
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
/// The chunk states how many of the stored bits are meaningful, so it is
/// written when the stored word is wider than the precision a channel carries:
/// a sixteen bit frame needs none, because the PNG depth is the frame's own,
/// while a ten bit frame written as a sixteen bit PNG does, because the low bits
/// of that word are not source information. A channel whose samples were
/// narrowed instead states the whole of its word, which is the same as stating
/// nothing about it.
fn significant_bits(plan: &FramePlan) -> Option<Vec<u8>> {
    let stored = u8::try_from(plan.stored_bits()).ok()?;
    let mut sbit = Vec::with_capacity(plan.channels());
    for _ in 0..plan.color_planes {
        sbit.push(u8::try_from(plan.bits.min(plan.stored_bits())).ok()?);
    }
    if let Some(alpha) = plan.alpha {
        sbit.push(u8::try_from(alpha.bits.min(plan.stored_bits())).ok()?);
    }
    if sbit.iter().all(|bits| *bits == stored) {
        return None;
    }
    Some(sbit)
}

/// Runs one channel of a row through [`put_row`] with the storage the plan asks
/// for.
///
/// The three ways a produced value reaches the stored word become three
/// monomorphisations rather than three branches inside the loop, which is what
/// keeps this writer's hottest loop tight: every sample of a row reaches the
/// word the same way, so the decision belongs to the row and not to the
/// sample.
macro_rules! put_channel {
    ($row:expr, $plan:expr, $channel:expr, $count:expr, $bits:expr, $storage:expr, $sample:expr) => {
        match $storage {
            Storage::Direct => put_row($row, $plan, $channel, $count, $sample, |value| value),
            Storage::Widen => put_row($row, $plan, $channel, $count, $sample, |value| {
                widen(value, $bits)
            }),
            Storage::Reduce => put_row($row, $plan, $channel, $count, $sample, |value| {
                reduce(value, $bits, $plan.stored_bits())
            }),
        }
    };
}

/// Packs one frame row of every plane into one PNG row.
///
/// `staging` is where a conversion puts the samples it makes before they reach
/// the row: the plane it reads first, then the next, each as wide as the row,
/// which is the layout `convert::Converter::rgb_row` fills. It belongs to the
/// caller so that a row is never the place to allocate one.
fn pack_row(
    plan: &FramePlan,
    color: &VideoFrame,
    alpha: Option<&VideoFrame>,
    y: usize,
    row: &mut [u8],
    staging: &mut [u16],
) {
    match &plan.source {
        Source::Yuv(converter) => {
            // SAFETY: the plan was built from this frame, so its three planes
            // are the ones the conversion's geometry was resolved against.
            let planes = [
                unsafe { conversion_plane(color, 0) },
                unsafe { conversion_plane(color, 1) },
                unsafe { conversion_plane(color, 2) },
            ];
            converter.rgb_row(y, planes, staging);
            for channel in 0..plan.color_planes {
                let read = |x: usize| staging[channel * plan.width + x];
                put_channel!(
                    row,
                    plan,
                    channel,
                    plan.width,
                    plan.bits,
                    plan.storage,
                    read
                );
            }
        }
        Source::Float => {
            for plane in 0..plan.color_planes {
                // SAFETY: one row of the plane the plan was built from, which
                // holds whole float samples across its width.
                let source = unsafe { row_samples(color, plane, y, plan.width * 4) };
                let read =
                    |x: usize| convert::quantise(convert::float_sample(source, x * 4), plan.bits);
                put_channel!(row, plan, plane, plan.width, plan.bits, plan.storage, read);
            }
        }
        Source::Planes => {
            let bytes = if plan.bits <= 8 { 1 } else { 2 };
            for plane in 0..plan.color_planes {
                // SAFETY: as above, one row of the plane's own samples.
                let source = unsafe { row_samples(color, plane, y, plan.width * bytes) };
                let read = |x: usize| -> u16 {
                    if bytes == 1 {
                        u16::from(source[x])
                    } else {
                        u16::from_ne_bytes([source[x * 2], source[x * 2 + 1]])
                    }
                };
                put_channel!(row, plan, plane, plan.width, plan.bits, plan.storage, read);
            }
        }
    }
    if let (Some(alpha), Some(channel)) = (alpha, plan.alpha) {
        let float = alpha.get_video_format().sample_type == SampleType::Float;
        let bytes = if float {
            // A float sample is four bytes whatever depth it is stored at.
            4
        } else if channel.bits <= 8 {
            1
        } else {
            2
        };
        // SAFETY: the alpha frame was checked against the colour frame, so its
        // plane is as wide as the row being built.
        let source = unsafe { row_samples(alpha, 0, y, plan.width * bytes) };
        let read = |x: usize| -> u16 {
            if float {
                convert::quantise(convert::float_sample(source, x * 4), channel.bits)
            } else if bytes == 1 {
                u16::from(source[x])
            } else {
                u16::from_ne_bytes([source[x * 2], source[x * 2 + 1]])
            }
        };
        put_channel!(
            row,
            plan,
            plan.color_planes,
            plan.width,
            channel.bits,
            channel.storage,
            read
        );
    }
}

/// Writes one channel of one row into the interleaved PNG row.
///
/// The stored word's width is decided once here, and `move_sample` is what the
/// channel's storage asks for: the alpha clip carries a precision of its own,
/// and this is where the two meet.
fn put_row<S, T>(
    row: &mut [u8],
    plan: &FramePlan,
    channel: usize,
    count: usize,
    sample: S,
    move_sample: T,
) where
    S: Fn(usize) -> u16,
    T: Fn(u16) -> u16,
{
    let channels = plan.channels();
    match plan.sample_bytes() {
        2 => {
            for index in 0..count {
                let at = (index * channels + channel) * 2;
                let value = move_sample(sample(index));
                row[at..at + 2].copy_from_slice(&value.to_be_bytes());
            }
        }
        1 => {
            for index in 0..count {
                let value = move_sample(sample(index));
                debug_assert!(
                    value <= u16::from(u8::MAX),
                    "an eight bit sample was scaled into its word"
                );
                row[index * channels + channel] = value as u8;
            }
        }
        _ => {
            // A row of fewer than eight bits a sample packs them most
            // significant first, and the storage above has already scaled the
            // value into that word. Such a row is one gray plane, so the flat
            // sample index is the one the loop counts.
            let bits = plan.stored_bits() as usize;
            for index in 0..count {
                let value = move_sample(sample(index));
                debug_assert!(
                    u32::from(value) < (1 << bits),
                    "a packed sample was scaled into its word"
                );
                let at = index * bits;
                row[at / 8] |= (value as u8) << (8 - bits - at % 8);
            }
        }
    }
}

/// One row of one of a frame's planes.
///
/// # Safety
///
/// The frame has to be the one the plan was built from, so the row is inside
/// the plane and `bytes` is no wider than its own row.
unsafe fn row_samples(frame: &VideoFrame, plane: usize, y: usize, bytes: usize) -> &[u8] {
    let stride = frame.stride(plane as i32) as usize;
    // SAFETY: the caller promises the row is the frame's own, so the plane's
    // stride addresses it and it holds at least `bytes`.
    unsafe { std::slice::from_raw_parts(frame.plane(plane as i32).add(y * stride), bytes) }
}

/// One of a frame's planes as a conversion reads it.
///
/// # Safety
///
/// The frame has to be the one the plan was built from, so its plane `plane` is
/// the shape the conversion's geometry was resolved against and holds
/// `stride * height` bytes.
unsafe fn conversion_plane(frame: &VideoFrame, plane: i32) -> Plane<'_> {
    let stride = frame.stride(plane) as usize;
    let height = frame.frame_height(plane) as usize;
    Plane {
        // SAFETY: the caller promises the plane is the one the plan validated.
        data: unsafe { std::slice::from_raw_parts(frame.plane(plane), stride * height) },
        stride,
    }
}

/// The `cICP` payload the pixels of this write describe, if any.
///
/// PNG states matrices with the field fixed at zero, so a frame whose `_Matrix`
/// names a conversion is one this chunk cannot describe and is left alone; the
/// same goes for a frame whose samples are not full range, because the chunk's
/// flag would then describe pixels the file does not hold. A frame that leaves
/// both unset gets the chunk whenever it states primaries and transfer, since
/// those two are exactly what the file's samples mean.
///
/// `converted` is what writing a yuv frame changes: the file holds r,g,b of the
/// frame's own primaries and transfer, so those two are what it states, whatever
/// the frame's `_Matrix` and `_Range` describe -- those describe the yuv planes
/// the file does not hold.
fn frame_cicp(frame: &VideoFrame, converted: bool) -> Option<[u8; 4]> {
    let primaries = u8::try_from(property_int(frame, key!(c"_Primaries"))?).ok()?;
    let transfer = u8::try_from(property_int(frame, key!(c"_Transfer"))?).ok()?;
    if primaries == UNSPECIFIED || transfer == UNSPECIFIED {
        return None;
    }
    if !converted {
        if property_int(frame, key!(c"_Matrix")).is_some_and(|matrix| matrix != 0) {
            return None;
        }
        if property_int(frame, key!(c"_Range")).is_some_and(|range| range != 1) {
            return None;
        }
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

    /// A plan of one colour shape, which is what the storage and `sBIT` rules
    /// are decided from without a frame to read them off.
    fn plan_of(color_type: ColorType, planes: usize, bits: u32, stored: u32) -> FramePlan {
        FramePlan {
            color_type,
            bit_depth: bit_depth_of(stored),
            bits,
            storage: storage_of(bits, stored),
            source: Source::Planes,
            color_planes: planes,
            alpha: None,
            width: 4,
            height: 2,
        }
    }

    #[test]
    fn a_sixteen_bit_word_is_not_a_meaningful_one() {
        // The chunk is written exactly when the stored word is wider than the
        // samples are: an eight bit frame is an eight bit PNG and a ten bit one
        // is a sixteen bit PNG whose low bits are not source information.
        for bits in 8..=16u32 {
            let stored = if bits > 8 { 16 } else { 8 };
            let plan = plan_of(ColorType::Rgb, 3, bits, stored);
            if bits == stored {
                assert!(significant_bits(&plan).is_none(), "{bits} bit needs none");
            } else {
                assert_eq!(
                    significant_bits(&plan),
                    Some(vec![bits as u8; 3]),
                    "{bits} bit states itself"
                );
            }
        }
        assert_eq!(
            bit_depth_of(1),
            BitDepth::One,
            "a depth below a byte is one png has"
        );
        // A depth the caller asked for is a decision rather than a storage
        // detail: sixteen bits from ten is still a widening, and eight bits
        // from sixteen is a narrowing that leaves nothing to state.
        let widened = plan_of(ColorType::Rgb, 3, 10, 16);
        assert_eq!(widened.storage, Storage::Widen);
        assert_eq!(significant_bits(&widened), Some(vec![10, 10, 10]));
        let narrowed = plan_of(ColorType::Rgb, 3, 16, 8);
        assert_eq!(narrowed.storage, Storage::Reduce);
        assert!(significant_bits(&narrowed).is_none());
        // The alpha channel states its own precision, which is the colour's
        // whenever a clip gave the two the same depth.
        let mut with_alpha = plan_of(ColorType::Rgba, 3, 10, 16);
        with_alpha.alpha = Some(Alpha {
            bits: 10,
            storage: Storage::Widen,
        });
        assert_eq!(significant_bits(&with_alpha), Some(vec![10, 10, 10, 10]));
    }

    #[test]
    fn a_sample_moves_between_the_depths_it_is_stored_at() {
        // Both ends of every pair are exact: zero is zero and the largest
        // sample of the source is the largest of the target, which is what
        // keeps a white white whether the word grew or shrank.
        // The packed words are the same rule one level down: a gray frame asked
        // for at one bit a sample is scaled into it rather than having to be its
        // codes already, which is what writes back a picture the reader
        // expanded.
        for (from, to) in [
            (16, 8),
            (12, 8),
            (10, 8),
            (9, 8),
            (8, 16),
            (10, 16),
            (8, 4),
            (8, 2),
            (8, 1),
        ] {
            let source_max = (1u32 << from) - 1;
            let target_max = (1u32 << to) - 1;
            let moved = |sample: u16| {
                if to > from {
                    widen(sample, from)
                } else {
                    reduce(sample, from, to)
                }
            };
            assert_eq!(moved(0), 0, "{from} to {to}");
            assert_eq!(
                u32::from(moved(source_max as u16)),
                target_max,
                "{from} to {to}"
            );
        }
        // Rounding is to nearest rather than a truncation, which is what makes
        // the middle of a sixteen bit word the middle of an eight bit one.
        assert_eq!(reduce(0, 16, 8), 0);
        assert_eq!(reduce(32768, 16, 8), 128);
        assert_eq!(reduce(65535, 16, 8), 255);
        assert_eq!(reduce(4095, 12, 8), 255);
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
