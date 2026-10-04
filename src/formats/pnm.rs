//! Netpbm images, read by this tree's own port of the `image` decoder.
//!
//! The port is [`image 0.25.10`'s `src/codecs/pnm/`][upstream] -- the decoder
//! and the header, and not `autobreak.rs`, which inserts line breaks when a pnm
//! is *written* and has no reading counterpart in this plugin.
//!
//! [upstream]: https://github.com/image-rs/image/blob/v0.25.10/src/codecs/pnm/decoder.rs
//!
//! Three behaviours are inherited, and the last two are the ones a reader gets
//! wrong by being helpful:
//!
//! - **A comment is legal between header fields and illegal in an ASCII
//!   raster.** `#` runs to the end of the line anywhere in the preamble -- that
//!   is how every one of these files is allowed to be annotated -- but inside
//!   the raster of an ASCII file a `#` is not a digit and not a separator, so it
//!   is an error rather than something to skip. The same bytes in a binary
//!   subtype are a sample.
//! - **A sample is rescaled through `f32` whenever `MAXVAL` is not the largest
//!   value its word holds.** The factor is `target / current` in floating point
//!   and the result is rounded, and the cast **saturates**: a one bit file's
//!   `MAXVAL` of one makes the factor 255, and the ASCII reader writes 255 for a
//!   white sample, so `255 * 255` is `65025` and saturates back to 255 rather
//!   than wrapping. That saturation is what makes `P1` readable at all; see
//!   [`rescale`].
//! - **A `P4` row is padded to a whole byte and its bits are inverted.** A set
//!   bit is black in the format and zero in a frame, so the expansion writes
//!   `1 - bit`.
//!
//! Sixteen bit samples are stored big-endian and converted to the word a frame
//! is written from, which is why the rescale below reads them natively.

use std::path::Path;

use crate::{
    decoder::{
        DecodeTimings, DecodedImage, ImageInfo, Pixels, RowSink, RowStream, image_error, image_head,
    },
    error::{ImgSeqError, Result},
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// The magic numbers, and whether they are an ASCII raster.
fn magic(value: &[u8]) -> Option<(u8, bool)> {
    match value {
        b"P1" => Some((1, true)),
        b"P2" => Some((2, true)),
        b"P3" => Some((3, true)),
        b"P4" => Some((4, false)),
        b"P5" => Some((5, false)),
        b"P6" => Some((6, false)),
        b"P7" => Some((7, false)),
        _ => None,
    }
}

/// The layout a header's tuple type stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tuple {
    PbmBit,
    BwBit,
    BwAlphaBit,
    GrayU8,
    GrayAlphaU8,
    GrayU16,
    GrayAlphaU16,
    RgbU8,
    RgbAlphaU8,
    RgbU16,
    RgbAlphaU16,
}

impl Tuple {
    /// The layout a frame is written from.
    const fn color_type(self) -> ColorType {
        match self {
            Self::PbmBit | Self::BwBit | Self::GrayU8 => ColorType::L8,
            Self::BwAlphaBit | Self::GrayAlphaU8 => ColorType::La8,
            Self::GrayU16 | Self::GrayAlphaU16 => ColorType::L16,
            Self::RgbU8 => ColorType::Rgb8,
            Self::RgbAlphaU8 => ColorType::Rgba8,
            Self::RgbU16 => ColorType::Rgb16,
            Self::RgbAlphaU16 => ColorType::Rgba16,
        }
    }

    /// The label property's value, which is narrower than the layout for a one
    /// bit file: the samples are one bit wide however they reach a frame.
    const fn source(self) -> SourceColorType {
        match self {
            Self::PbmBit | Self::BwBit => SourceColorType::L1,
            Self::BwAlphaBit => SourceColorType::La1,
            Self::GrayU8 => SourceColorType::L8,
            Self::GrayAlphaU8 => SourceColorType::La8,
            Self::GrayU16 => SourceColorType::L16,
            Self::GrayAlphaU16 => SourceColorType::La16,
            Self::RgbU8 => SourceColorType::Rgb8,
            Self::RgbAlphaU8 => SourceColorType::Rgba8,
            Self::RgbU16 => SourceColorType::Rgb16,
            Self::RgbAlphaU16 => SourceColorType::Rgba16,
        }
    }

    /// Channels one pixel holds.
    const fn channels(self) -> usize {
        match self {
            Self::PbmBit | Self::BwBit | Self::GrayU8 | Self::GrayU16 => 1,
            Self::BwAlphaBit | Self::GrayAlphaU8 | Self::GrayAlphaU16 => 2,
            Self::RgbU8 | Self::RgbU16 => 3,
            Self::RgbAlphaU8 | Self::RgbAlphaU16 => 4,
        }
    }

    /// Bytes one sample occupies.
    const fn sample_bytes(self) -> usize {
        match self {
            Self::GrayU16 | Self::GrayAlphaU16 | Self::RgbU16 | Self::RgbAlphaU16 => 2,
            _ => 1,
        }
    }

    /// The format a frame is written from.
    const fn format(self) -> PixelFormat {
        match self.sample_bytes() {
            2 => match self.channels() {
                1 => PixelFormat::Gray16,
                _ => PixelFormat::Rgb16,
            },
            _ => match self.channels() {
                1 => PixelFormat::Gray8,
                2 => PixelFormat::Gray8,
                _ => PixelFormat::Rgb8,
            },
        }
    }
}

/// What one netpbm's header states.
#[derive(Debug)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    /// The largest sample the file states, which is what the rescale compares
    /// against the word it widened into.
    maxval: u32,
    tuple: Tuple,
    /// Whether the raster is ASCII rather than packed.
    ascii: bool,
    /// Where the raster begins.
    data_offset: usize,
}

impl Header {
    /// The colour type a frame is written from.
    #[must_use]
    pub const fn color_type(&self) -> ColorType {
        self.tuple.color_type()
    }

    /// The label property's value.
    #[must_use]
    pub const fn source(&self) -> SourceColorType {
        self.tuple.source()
    }

    /// The format a frame is written from.
    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        self.tuple.format()
    }
}

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    // Content first: a file whose bytes say it is something else is that
    // something else however it is named, and the extension is the hint a
    // format with no signature of its own has to fall back on. See
    // [`identify::owns`](crate::formats::identify::owns).
    crate::formats::identify::owns(crate::formats::identify::Format::Pnm, path)
}

/// Whether a byte separates two header fields.
const fn is_separator(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | b' ')
}

/// Reads the next integer, skipping whitespace and comments.
///
/// A `#` disables every byte up to and including the next newline, which is how
/// a file annotates itself between fields. A byte that is neither a digit nor a
/// separator ends the number, so a stray character is an error rather than
/// silently ignored.
fn read_number(data: &[u8], cursor: &mut usize, what: &str) -> Result<u32> {
    let mut value: u32 = 0;
    let mut found = false;
    let mut commenting = false;
    while let Some(&byte) = data.get(*cursor) {
        *cursor += 1;
        if commenting {
            if byte == b'\r' || byte == b'\n' {
                commenting = false;
            }
            continue;
        }
        match byte {
            b'#' => commenting = true,
            byte if is_separator(byte) => {
                if found {
                    return Ok(value);
                }
            }
            b'0'..=b'9' => {
                value = value
                    .checked_mul(10)
                    .and_then(|value| value.checked_add(u32::from(byte - b'0')))
                    .ok_or_else(|| ImgSeqError::new(format!("the {what} is too large")))?;
                found = true;
            }
            other if !other.is_ascii() => {
                return Err(ImgSeqError::new("a non-ascii byte is in the header"));
            }
            other => {
                return Err(ImgSeqError::new(format!(
                    "the header states {:?} where a number belongs",
                    other as char
                )));
            }
        }
    }
    if found {
        Ok(value)
    } else {
        Err(ImgSeqError::new(format!("the header states no {what}")))
    }
}

/// Reads one line, without its terminator.
fn read_line<'a>(data: &'a [u8], cursor: &mut usize) -> Option<&'a [u8]> {
    let start = *cursor;
    while let Some(&byte) = data.get(*cursor) {
        *cursor += 1;
        if byte == b'\n' {
            return Some(&data[start..*cursor - 1]);
        }
    }
    None
}

/// Picks the tuple type a plain (non-`P7`) header's `MAXVAL` asks for.
///
/// The word a sample is read into is decided here rather than by the magic:
/// `MAXVAL` up to a byte is one byte a sample, up to a word is two.
fn plain_tuple(kind: u8, maxval: u32) -> Result<Tuple> {
    if maxval == 0 {
        return Err(ImgSeqError::new("the header states a MAXVAL of zero"));
    }
    if maxval > 0xFFFF {
        return Err(ImgSeqError::new(format!(
            "the header states a MAXVAL of {maxval}, which exceeds a word"
        )));
    }
    let wide = maxval > 0xFF;
    Ok(match kind {
        1 | 4 => Tuple::PbmBit,
        2 | 5 => {
            if wide {
                Tuple::GrayU16
            } else {
                Tuple::GrayU8
            }
        }
        _ => {
            if wide {
                Tuple::RgbU16
            } else {
                Tuple::RgbU8
            }
        }
    })
}

/// Parses the header, and reports where the raster begins.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is not a netpbm, when a field is
/// missing or malformed, or when the tuple type is one this reader does not
/// take.
pub fn header(data: &[u8]) -> Result<Header> {
    let Some((kind, ascii)) = data.get(..2).and_then(magic) else {
        return Err(ImgSeqError::new("not a netpbm: no P1 to P7 magic"));
    };
    let mut cursor = 2usize;

    if kind == 7 {
        // `P7` is the one magic that must be followed by a newline before its
        // tags, so a `P7` written as `P7 WIDTH 4` is refused rather than read.
        match data.get(cursor) {
            Some(b'\n') => cursor += 1,
            Some(other) => {
                return Err(ImgSeqError::new(format!(
                    "a P7 header states {:?} where a newline belongs",
                    *other as char
                )));
            }
            None => return Err(ImgSeqError::new("the header is truncated")),
        }
        let (mut width, mut height, mut depth, mut maxval) = (None, None, None, None);
        let mut tupltype = String::new();
        loop {
            let line = read_line(data, &mut cursor)
                .ok_or_else(|| ImgSeqError::new("the header states no ENDHDR"))?;
            if line.is_empty() {
                return Err(ImgSeqError::new("the header ends before ENDHDR"));
            }
            if line[0] == b'#' {
                continue;
            }
            let text = std::str::from_utf8(line)
                .map_err(|_| ImgSeqError::new("a non-ascii byte is in the header"))?;
            let trimmed = text.trim_start();
            let split = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
            let (identifier, rest) = trimmed.split_at(split);
            let value = || {
                rest.trim()
                    .parse::<u32>()
                    .map_err(|_| ImgSeqError::new(format!("{identifier} states no number")))
            };
            match identifier {
                "ENDHDR" => break,
                "WIDTH" => width = Some(value()?),
                "HEIGHT" => height = Some(value()?),
                "DEPTH" => depth = Some(value()?),
                "MAXVAL" => maxval = Some(value()?),
                // A tuple type may be written over several tags, and they are
                // joined with a space because that is what they are.
                "TUPLTYPE" => {
                    if !tupltype.is_empty() {
                        tupltype.push(' ');
                    }
                    tupltype.push_str(rest.trim());
                }
                other => {
                    return Err(ImgSeqError::new(format!(
                        "the header states an unknown tag {other:?}"
                    )));
                }
            }
        }
        let (Some(width), Some(height), Some(depth), Some(maxval)) = (width, height, depth, maxval)
        else {
            return Err(ImgSeqError::new(
                "a P7 header must state WIDTH, HEIGHT, DEPTH and MAXVAL",
            ));
        };
        let tuple = arbitrary_tuple(&tupltype, depth, maxval)?;
        if width == 0 || height == 0 {
            return Err(ImgSeqError::new("the header states no pixels"));
        }
        return Ok(Header {
            width,
            height,
            maxval,
            tuple,
            ascii: false,
            data_offset: cursor,
        });
    }

    let width = read_number(data, &mut cursor, "width")?;
    let height = read_number(data, &mut cursor, "height")?;
    // A bitmap has no `MAXVAL`, and one is what its samples are.
    let maxval = if kind == 1 || kind == 4 {
        1
    } else {
        read_number(data, &mut cursor, "MAXVAL")?
    };
    if width == 0 || height == 0 {
        return Err(ImgSeqError::new("the header states no pixels"));
    }
    // The raster begins after the single separator the last field ended on.
    let tuple = plain_tuple(kind, maxval)?;
    Ok(Header {
        width,
        height,
        maxval,
        tuple,
        ascii,
        data_offset: cursor,
    })
}

/// Picks the tuple type a `P7` header's tags ask for.
///
/// Every arm of the upstream match that has no counterpart here is an error
/// rather than a guess: a tuple type this reader does not know, and a depth or
/// `MAXVAL` its own type does not allow.
fn arbitrary_tuple(tupltype: &str, depth: u32, maxval: u32) -> Result<Tuple> {
    if maxval == 0 {
        return Err(ImgSeqError::new("the header states a MAXVAL of zero"));
    }
    if maxval > 0xFFFF {
        return Err(ImgSeqError::new(format!(
            "the header states a MAXVAL of {maxval}, which exceeds a word"
        )));
    }
    let wide = maxval > 0xFF;
    let bad = || {
        ImgSeqError::new(format!(
            "a {tupltype:?} tuple of depth {depth} and MAXVAL {maxval} is not a layout this reader takes"
        ))
    };
    match tupltype {
        // No tuple type at all falls back on the depth, and only at a byte.
        "" => match depth {
            1 => Ok(Tuple::GrayU8),
            2 => Ok(Tuple::GrayAlphaU8),
            3 => Ok(Tuple::RgbU8),
            4 => Ok(Tuple::RgbAlphaU8),
            _ => Err(bad()),
        },
        "BLACKANDWHITE" => {
            if depth == 1 && maxval == 1 {
                Ok(Tuple::BwBit)
            } else {
                Err(bad())
            }
        }
        "BLACKANDWHITE_ALPHA" => {
            if depth == 2 && maxval == 1 {
                Ok(Tuple::BwAlphaBit)
            } else {
                Err(bad())
            }
        }
        "GRAYSCALE" => {
            if depth != 1 {
                Err(bad())
            } else if wide {
                Ok(Tuple::GrayU16)
            } else {
                Ok(Tuple::GrayU8)
            }
        }
        "GRAYSCALE_ALPHA" => {
            if depth != 2 {
                Err(bad())
            } else if wide {
                Ok(Tuple::GrayAlphaU16)
            } else {
                Ok(Tuple::GrayAlphaU8)
            }
        }
        "RGB" => {
            if depth != 3 {
                Err(bad())
            } else if wide {
                Ok(Tuple::RgbU16)
            } else {
                Ok(Tuple::RgbU8)
            }
        }
        "RGB_ALPHA" => {
            if depth != 4 {
                Err(bad())
            } else if wide {
                Ok(Tuple::RgbAlphaU16)
            } else {
                Ok(Tuple::RgbAlphaU8)
            }
        }
        other => Err(ImgSeqError::new(format!(
            "the tuple type {other:?} is not one this reader implements"
        ))),
    }
}

/// Reads the next ASCII sample.
///
/// A comment is *not* skipped: `#` is neither a digit nor a separator, so it
/// ends the number with no digits read, which is the refusal the format asks
/// for in a raster rather than in the preamble.
fn read_ascii_number(data: &[u8], cursor: &mut usize) -> Result<u16> {
    while let Some(&byte) = data.get(*cursor) {
        if !is_separator(byte) {
            break;
        }
        *cursor += 1;
    }
    let mut value: u16 = 0;
    let mut found = false;
    while let Some(&byte) = data.get(*cursor) {
        let digit = match byte {
            b'0'..=b'9' => u16::from(byte - b'0'),
            // A separator ends the sample; anything else is not a number.
            byte if is_separator(byte) => {
                if found {
                    return Ok(value);
                }
                *cursor += 1;
                continue;
            }
            other => {
                return Err(ImgSeqError::new(format!(
                    "the raster states {:?} where a sample belongs",
                    other as char
                )));
            }
        };
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(digit))
            .ok_or_else(|| ImgSeqError::new("a sample is too large"))?;
        found = true;
        *cursor += 1;
    }
    if found {
        Ok(value)
    } else {
        Err(ImgSeqError::new("the raster ends before its samples do"))
    }
}

/// Rescales a raster in place when `MAXVAL` is not the largest value its word
/// holds.
///
/// The multiplication is floating point and the result is rounded, and the cast
/// back **saturates** rather than wrapping. That matters for a one bit file:
/// its `MAXVAL` of one makes the factor 255, and the ASCII reader writes 255 for
/// a white sample, so the product is 65025 and saturates back to 255. A wrapping
/// conversion would turn every white pixel of a `P1` into 1.
fn rescale(data: &mut [u8], maxval: u32, sample_bytes: usize) {
    let target = if sample_bytes == 1 { 0xFF } else { 0xFFFF };
    if maxval == target {
        return;
    }
    let factor = target as f32 / maxval as f32;
    if sample_bytes == 1 {
        for value in data.iter_mut() {
            *value = (f32::from(*value) * factor).round() as u8;
        }
    } else {
        for chunk in data.as_chunks_mut::<2>().0.iter_mut() {
            let value = u16::from_ne_bytes(*chunk);
            let scaled = (f32::from(value) * factor).round() as u16;
            chunk.copy_from_slice(&scaled.to_ne_bytes());
        }
    }
}

/// Reads the raster into one native-endian buffer.
fn raster(header: &Header, data: &[u8]) -> Result<Vec<u8>> {
    let pixels = (header.width as usize)
        .checked_mul(header.height as usize)
        .ok_or_else(|| ImgSeqError::new("the image is too large"))?;
    let channels = header.tuple.channels();
    let sample_bytes = header.tuple.sample_bytes();
    let size = pixels
        .checked_mul(channels)
        .and_then(|count| count.checked_mul(sample_bytes))
        .ok_or_else(|| ImgSeqError::new("the image is too large"))?;
    // The buffer is allocated by the arm that needs one. The binary raster is
    // the file's own bytes and only has to be copied, so it takes the payload
    // directly: `vec![0u8; size]` followed by a full overwrite zeroes the whole
    // picture for nothing, and the allocator has to re-zero a reused block every
    // file. This reader measured 1.19x slower than the one it replaced before
    // the two were separated. See `docs/improvements/26`.
    let mut out;
    let body = data
        .get(header.data_offset..)
        .ok_or_else(|| ImgSeqError::new("the raster is truncated"))?;
    let mut cursor = 0usize;

    if header.tuple == Tuple::PbmBit && !header.ascii {
        out = vec![0u8; size];
        // One bit a sample, most significant first, each row padded to a whole
        // byte, and a set bit is black so the expansion is inverted.
        let width = header.width as usize;
        let height = header.height as usize;
        let line = width.div_ceil(8);
        let needed = line * height;
        let packed = body
            .get(..needed)
            .ok_or_else(|| ImgSeqError::new("the raster is truncated"))?;
        for y in (0..height).rev() {
            for x in (0..width).rev() {
                let bit = (packed[y * line + x / 8] >> (7 - (x % 8))) & 1;
                out[y * width + x] = 1 - bit;
            }
        }
    } else if header.ascii {
        out = vec![0u8; size];
        if header.tuple == Tuple::PbmBit {
            // One bit a sample, and the format calls a set bit black, so
            // the ASCII digit maps the opposite way to its own value: a nought
            // is white and a one is black. The packed subtype inverts too, in
            // its own branch.
            for value in out.iter_mut() {
                *value = match read_ascii_number(body, &mut cursor)? {
                    0 => 255,
                    1 => 0,
                    other => {
                        return Err(ImgSeqError::new(format!(
                            "the raster states {other} where a bit belongs"
                        )));
                    }
                };
            }
        } else if sample_bytes == 2 {
            for chunk in out.as_chunks_mut::<2>().0.iter_mut() {
                let sample = read_ascii_number(body, &mut cursor)?;
                chunk.copy_from_slice(&sample.to_ne_bytes());
            }
        } else {
            for value in out.iter_mut() {
                *value = read_ascii_number(body, &mut cursor)? as u8;
            }
        }
    } else {
        let raw = body
            .get(..size)
            .ok_or_else(|| ImgSeqError::new("the raster is truncated"))?;
        out = raw.to_vec();
        if sample_bytes == 2 {
            // The file is big-endian; a frame is written from the native word.
            for chunk in out.as_chunks_mut::<2>().0.iter_mut() {
                let value = u16::from_be_bytes(*chunk);
                chunk.copy_from_slice(&value.to_ne_bytes());
            }
        }
    }

    rescale(&mut out, header.maxval, sample_bytes);
    Ok(out)
}

/// A packed eight bit RGB raster whose rows are the file's own bytes.
///
/// This is the one netpbm shape that needs no work between the file and the
/// frame: a `P6` at `MAXVAL` 255 holds `width * 3` bytes a row with nothing to
/// unpack and nothing to rescale, so the row is a slice of the file and
/// [`RowSink::place_rgb8`] writes it. Every other form -- ASCII, a word a
/// sample, a `MAXVAL` that rescales, `P4`, `P5`, `P7` -- keeps the buffered
/// path, which is the rule a stream is only answered with when it can fill
/// every frame of the call.
#[derive(Debug)]
pub struct Rows {
    path: std::path::PathBuf,
}

impl RowStream for Rows {
    fn has_alpha(&self) -> bool {
        false
    }

    fn fill(&mut self, mut sink: RowSink<'_>) -> Result<DecodeTimings> {
        let open_started = std::time::Instant::now();
        let data = std::fs::read(&self.path).map_err(|e| image_error("open", &self.path, e))?;
        let open = open_started.elapsed();
        let header = header(&data).map_err(|e| image_error("decode", &self.path, e))?;

        let read_started = std::time::Instant::now();
        let row_bytes = header.width as usize * 3;
        for row in 0..header.height as usize {
            let at = header.data_offset + row * row_bytes;
            let line = data
                .get(at..at + row_bytes)
                .ok_or_else(|| ImgSeqError::new(format!("the raster ends in row {row}")))?;
            sink.place_rgb8(line, row)
                .ok_or_else(|| ImgSeqError::new("the frame holds no three colour planes"))?;
        }
        let read = read_started.elapsed();
        Ok(DecodeTimings {
            open,
            metadata: std::time::Duration::ZERO,
            buffer: std::time::Duration::ZERO,
            read,
        })
    }

    fn duplicate(&self) -> Box<dyn RowStream> {
        Box::new(Self {
            path: self.path.clone(),
        })
    }
}

/// Prepares a raster whose rows will be written straight into the frames.
///
/// Answers `None` for every shape the stream does not cover, so the caller
/// falls back to [`decode`].
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the header cannot be read back.
pub fn stream(info: &ImageInfo) -> Result<Option<DecodedImage>> {
    let data = image_head(&info.path).map_err(|e| image_error("open", &info.path, e))?;
    let header = header(&data).map_err(|e| image_error("decode", &info.path, e))?;
    if header.ascii || header.tuple != Tuple::RgbU8 || header.maxval != 0xFF {
        return Ok(None);
    }
    Ok(Some(DecodedImage {
        width: info.width,
        height: info.height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Stream(Box::new(Rows {
            path: info.path.clone(),
        })),
        timings: DecodeTimings {
            open: std::time::Duration::ZERO,
            metadata: std::time::Duration::ZERO,
            buffer: std::time::Duration::ZERO,
            read: std::time::Duration::ZERO,
        },
    }))
}
/// What a netpbm states, when this module reads the file.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info(path: &Path, _apply_rotation: bool) -> Result<Option<ImageInfo>> {
    if !owns(path) {
        return Ok(None);
    }
    let data = image_head(path).map_err(|error| image_error("open", path, error))?;
    // A file whose magic is not one of the seven is declined rather than
    // refused, so something else may still read it.
    if data.get(..2).and_then(magic).is_none() {
        return Ok(None);
    }
    let header = header(&data).map_err(|error| image_error("identify", path, error))?;
    Ok(Some(ImageInfo {
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type: header.color_type(),
        original_color_type: header.source(),
        has_icc_profile: false,
        icc_profile: None,
        cicp: None,
        chroma_location: None,
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: header.format(),
    }))
}

/// Decodes a netpbm into one interleaved buffer.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or is malformed.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = std::time::Instant::now();
    let data = std::fs::read(&info.path).map_err(|error| image_error("open", &info.path, error))?;
    let open = open_started.elapsed();

    let header = header(&data).map_err(|error| image_error("decode", &info.path, error))?;
    if (header.width, header.height) != (info.width, info.height) {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {}x{}, now {}x{})",
            info.path.display(),
            info.width,
            info.height,
            header.width,
            header.height,
        )));
    }

    let read_started = std::time::Instant::now();
    let buffer =
        raster(&header, &data).map_err(|error| image_error("decode", &info.path, error))?;
    let read = read_started.elapsed();

    Ok(DecodedImage {
        width: header.width,
        height: header.height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Interleaved {
            color_type: header.color_type(),
            buffer,
        },
        timings: DecodeTimings {
            open,
            metadata: std::time::Duration::ZERO,
            buffer: std::time::Duration::ZERO,
            read,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn decoded(name: &str) -> Vec<u8> {
        let info = image_info(&fixture(name), true)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
            .unwrap_or_else(|| panic!("{name} is taken over"));
        match decode(&info)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
            .pixels
        {
            Pixels::Interleaved { buffer, .. } => buffer,
            _ => panic!("a netpbm hands out one interleaved buffer"),
        }
    }

    /// Every fixture, and the layout its header asks for.
    #[test]
    fn the_fixtures_state_the_layouts_the_plan_names() {
        for (name, width, height, color_type, source) in [
            ("pnm-p1.pbm", 37, 23, ColorType::L8, SourceColorType::L1),
            ("pnm-p2.pgm", 37, 23, ColorType::L8, SourceColorType::L8),
            ("pnm-p3.ppm", 37, 23, ColorType::Rgb8, SourceColorType::Rgb8),
            ("pnm-p4.pbm", 37, 23, ColorType::L8, SourceColorType::L1),
            ("pnm-p5.pgm", 37, 23, ColorType::L8, SourceColorType::L8),
            (
                "pnm-p5-16.pgm",
                37,
                23,
                ColorType::L16,
                SourceColorType::L16,
            ),
            ("pnm-p6.ppm", 37, 23, ColorType::Rgb8, SourceColorType::Rgb8),
            (
                "pnm-p6-16.ppm",
                37,
                23,
                ColorType::Rgb16,
                SourceColorType::Rgb16,
            ),
            (
                "pnm-p7-rgb.pam",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            (
                "pnm-p7-rgba.pam",
                37,
                23,
                ColorType::Rgba8,
                SourceColorType::Rgba8,
            ),
            (
                "pnm-p7-gray.pam",
                37,
                23,
                ColorType::L8,
                SourceColorType::L8,
            ),
            (
                "pnm-p7-gray-alpha.pam",
                37,
                23,
                ColorType::La8,
                SourceColorType::La8,
            ),
            // A MAXVAL that is not one less than a power of two.
            (
                "pnm-p7-maxval31.pam",
                37,
                23,
                ColorType::L8,
                SourceColorType::L8,
            ),
            (
                "pnm-p7-rgb16.pam",
                37,
                23,
                ColorType::Rgb16,
                SourceColorType::Rgb16,
            ),
            // A comment between header fields is legal.
            (
                "pnm-comment.pgm",
                37,
                23,
                ColorType::L8,
                SourceColorType::L8,
            ),
        ] {
            let info = image_info(&fixture(name), true)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .unwrap_or_else(|| panic!("{name} is taken over"));
            assert_eq!((info.width, info.height), (width, height), "{name}");
            assert_eq!(info.color_type, color_type, "{name}");
            assert_eq!(info.original_color_type, source, "{name}");
            assert_eq!(info.transform, Transform::IDENTITY, "{name}");
        }
    }

    /// The two one bit subtypes hold the same pattern through different
    /// encodings, and both reach a frame as nought and full. The saturating
    /// rescale is what makes the ASCII one agree with the packed one.
    #[test]
    fn the_two_bitmap_subtypes_agree_and_saturate() {
        let ascii = decoded("pnm-p1.pbm");
        let packed = decoded("pnm-p4.pbm");
        assert_eq!(ascii, packed, "P1 and P4 hold the same pattern");
        let distinct: std::collections::BTreeSet<u8> = ascii.iter().copied().collect();
        assert_eq!(
            distinct,
            std::collections::BTreeSet::from([0, 255]),
            "a one bit file reaches a frame as nought and full, not nought and one"
        );
    }

    /// The rescale is floating point and rounds, so two files that hold the
    /// same picture at different `MAXVAL`s agree where their samples saturate
    /// and differ where the rounding bites.
    #[test]
    fn a_maxval_that_is_not_a_word_is_rescaled() {
        let exact = decoded("pnm-p6-16.ppm");
        let scaled = decoded("pnm-p7-rgb16.pam");
        assert_eq!(exact.len(), scaled.len());
        // Blue is only ever nought or full in this picture, so it saturates to
        // the same word either way.
        let blue = |buffer: &[u8]| -> Vec<u8> {
            buffer
                .as_chunks::<2>()
                .0
                .iter()
                .enumerate()
                .filter(|(index, _)| index % 3 == 2)
                .flat_map(|(_, chunk)| chunk.to_vec())
                .collect()
        };
        assert_eq!(blue(&exact), blue(&scaled), "a saturated channel agrees");
        assert_ne!(exact, scaled, "and a rounding one does not");
    }

    /// A comment is legal between header fields and illegal inside an ASCII
    /// raster, which is the distinction the two `comment` fixtures hold.
    #[test]
    fn a_comment_is_legal_in_the_preamble_and_not_in_an_ascii_raster() {
        // Two comments, one whole line and one between two fields.
        let with = image_info(&fixture("pnm-comment.pgm"), true)
            .expect("read")
            .expect("taken over");
        assert_eq!((with.width, with.height), (37, 23));
        assert!(decode(&with).is_ok());

        let error = decode(
            &image_info(&fixture("pnm-ascii-comment.pgm"), true)
                .expect("read")
                .expect("taken over"),
        )
        .expect_err("a comment inside an ASCII raster is refused");
        assert!(
            error.to_string().contains("where a sample belongs"),
            "{error}"
        );
    }

    /// A tuple type this reader does not know, and a magic that is not one of
    /// the seven, are both refused or declined by name.
    #[test]
    fn an_unknown_tuple_type_is_refused_by_name() {
        let pam = b"P7\nWIDTH 2\nHEIGHT 2\nDEPTH 3\nMAXVAL 255\nTUPLTYPE CMYK\nENDHDR\n";
        let error = header(pam).expect_err("CMYK is not a tuple this reader takes");
        assert!(error.to_string().contains("CMYK"), "{error}");

        assert!(!owns(Path::new("a.png")));
        assert!(owns(Path::new("a.pgm")));
        assert!(owns(Path::new("a.PPM")));
        assert!(
            image_info(&fixture("cicp-rgb8.png"), true)
                .expect("a png is not ours")
                .is_none()
        );
    }
}
