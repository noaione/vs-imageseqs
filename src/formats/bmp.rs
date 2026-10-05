//! Windows bitmaps, read by this tree's own port of the `image` decoder.
//!
//! The port is [`image 0.25.10`'s `src/codecs/bmp/decoder.rs`][upstream], kept
//! because no candidate crate passes the corpus: `zune-bmp` fails every palette
//! and run-length file. What is kept is the behaviour, not the shape -- the
//! reader here works on a byte slice and hands its samples to one interleaved
//! buffer, where the original streamed from a reader into `ImageBuffer`.
//!
//! [upstream]: https://github.com/image-rs/image/blob/v0.25.10/src/codecs/bmp/decoder.rs
//!
//! Four behaviours are *not* what the obvious reading of the format gives, and
//! every fixture in `tests/fixtures/bmp-*.bmp` exists to keep one of them:
//!
//! - **A thirty-two bit `BI_RGB` bitmap drops its fourth byte.** No alpha is
//!   handed out however that byte is set, and a V4 or V5 header does not change
//!   it: the alpha flag is decided before the masks are read, and a `BI_RGB`
//!   image never reads any. See [`Header::image_type`].
//! - **Alpha arrives only when a bitfield alpha mask is non-zero.** A
//!   `BI_BITFIELDS` file whose alpha mask is zero is `Rgb8` with an opaque
//!   clip, exactly like the `BI_RGB` one.
//! - **A bitfield's expansion is round-to-nearest, not bit replication.** Five
//!   bits of `3` become `25`, where shifting and or-ing gives `24`. See
//!   [`expand`].
//! - **A palette is expanded to colour, not handed out as indices.** The
//!   upstream decoder reports `L8` for an indexed bitmap; the reader that feeds
//!   a frame asks for colour, so the palette lookup happens here.
//!
//! `BITMAPCOREHEADER` is read here too. It is the older of the format's two
//! headers and differs in three places that matter: both dimensions are signed
//! sixteen bit values, there is no compression field so only the uncompressed
//! form exists, and a palette entry is three bytes rather than four. It
//! cannot be stored top-down, and a negative height there is refused by name.
//!
//! Subtypes the corpus does not show are refused by name rather than guessed
//! at: the one- and two-bit depths, `BI_JPEG` and `BI_PNG` compression, and the
//! CMYK bit counts.

use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, RowSink, RowStream, image_error},
    error::{ImgSeqError, Result},
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

#[cfg(test)]
use crate::decoder::image_head;

/// Byte counts that name a DIB header, from the size field inside it.
const CORE_HEADER: u32 = 12;
const INFO_HEADER: u32 = 40;
/// The V2 and V3 headers are the information header with the masks appended.
const V3_HEADER: u32 = 56;
const V4_HEADER: u32 = 108;
const V5_HEADER: u32 = 124;

/// Compression methods, as the header stores them.
const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;
const BI_RLE4: u32 = 2;
const BI_BITFIELDS: u32 = 3;
const BI_JPEG: u32 = 4;
const BI_PNG: u32 = 5;

/// The five-five-five layout a sixteen bit `BI_RGB` bitmap uses.
const MASK_555: (u32, u32, u32) = (0x7C00, 0x03E0, 0x001F);

/// Escapes that introduce something other than a run, in a run-length bitmap.
const RLE_EOL: u8 = 0;
const RLE_EOF: u8 = 1;
const RLE_DELTA: u8 = 2;

/// The widest a bitmap may be, either way, because the header stores `i32`.
const MAX_SIDE: i32 = 0xFFFF;

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    // Content first: a file whose bytes say it is something else is that
    // something else however it is named, and the extension is the hint a
    // format with no signature of its own has to fall back on. See
    // [`identify::owns`](crate::formats::identify::owns).
    crate::formats::identify::owns(crate::formats::identify::Format::Bmp, path)
}

/// One channel of a bitfield: where its bits start and how many there are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bitfield {
    shift: u32,
    len: u32,
}

impl Bitfield {
    /// A bitfield for `mask`, or `None` when the mask is not a contiguous run
    /// of at most eight bits inside `max_len`.
    fn from_mask(mask: u32, max_len: u32) -> Option<Self> {
        if mask == 0 {
            return Some(Self { shift: 0, len: 0 });
        }
        let mut shift = mask.trailing_zeros();
        let mut len = (!(mask >> shift)).trailing_zeros();
        // A mask with a hole in it has no single shift and width.
        if len != mask.count_ones() || len + shift > max_len {
            return None;
        }
        if len > 8 {
            // More than a byte wide: keep the top eight bits, which is what the
            // upstream reader does rather than refusing the file.
            shift += len - 8;
            len = 8;
        }
        Some(Self { shift, len })
    }

    /// The eight bit value this field of `data` stands for.
    fn read(self, data: u32) -> u8 {
        expand(
            ((data >> self.shift) & ((1 << self.len) - 1).max(1)) as u16,
            self.len,
        )
    }
}

/// Widens `value`, `len` bits wide, to eight bits.
///
/// Round to nearest, which is what the upstream tables hold: five bits of `3`
/// become `25` rather than the `24` a shift and or would give, and two bits of
/// `1` become `85` rather than `85` from `0x55`. The tables were generated as
/// `round(value * 255 / (2**len - 1))`, which is what this computes.
pub fn expand(value: u16, len: u32) -> u8 {
    match len {
        0 => 0,
        8 => value as u8,
        _ => {
            let max = (1u32 << len) - 1;
            let scaled = u32::from(value) * 255;
            ((scaled + max / 2) / max) as u8
        }
    }
}

/// Which kind of pixel data a header describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageType {
    /// Indices into a palette, packed at one, two, four or eight bits.
    Palette,
    Rgb16,
    Rgb24,
    Rgb32,
    Bitfields16,
    Bitfields32,
    Rle8,
    Rle4,
}

/// What one bitmap's header states.
#[derive(Debug)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    /// A negative stored height means the rows are already top-down.
    pub top_down: bool,
    bit_count: u16,
    image_type: ImageType,
    /// Where the pixels start.
    pub data_offset: usize,
    /// Blue, green, red entries, when the file has a palette.
    palette: Vec<[u8; 3]>,
    /// The masks a bitfields file states, and the alpha one it may not.
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    alpha: Bitfield,
    /// Whether the fourth byte of a `BI_BITFIELDS` pixel is alpha.
    has_alpha: bool,
}

impl Header {
    /// The colour type a frame is written from.
    ///
    /// Two layouts and no more: a bitmap is either three bytes a pixel or four,
    /// and the palette is expanded rather than handed out as indices.
    #[must_use]
    pub const fn color_type(&self) -> ColorType {
        if self.has_alpha {
            ColorType::Rgba8
        } else {
            ColorType::Rgb8
        }
    }

    /// Channels per pixel in the decoded buffer.
    const fn channels(&self) -> usize {
        if self.has_alpha { 4 } else { 3 }
    }

    /// The format a frame is written from, which is eight bits a channel.
    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        PixelFormat::Rgb8
    }
}

/// Reads a little-endian `u16` at `offset`.
fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Reads a little-endian `i16` at `offset`, which is how the older
/// `BITMAPCOREHEADER` stores both of its dimensions.
fn i16_at(data: &[u8], offset: usize) -> Option<i16> {
    let bytes = data.get(offset..offset + 2)?;
    Some(i16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Reads a little-endian `u32` at `offset`.
fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Reads a little-endian `i32` at `offset`.
fn i32_at(data: &[u8], offset: usize) -> Option<i32> {
    u32_at(data, offset).map(|value| value as i32)
}

/// Parses the header of a bitmap whose DIB starts at `start`.
///
/// `file_header` says whether a fourteen byte `BM` header comes first, which is
/// what tells a `.bmp` from the bare DIB an icon holds.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the header is malformed or names a subtype this
/// reader does not implement.
pub fn header(data: &[u8], start: usize, file_header: bool) -> Result<Header> {
    let mut offset = start;
    if file_header {
        if data.get(start..start + 2) != Some(b"BM") {
            return Err(ImgSeqError::new("not a bitmap: no BM file header"));
        }
        offset += 14;
    }

    let header_size =
        u32_at(data, offset).ok_or_else(|| ImgSeqError::new("the bitmap is truncated"))?;
    // The format has two headers. `BITMAPCOREHEADER` is twelve bytes: two signed
    // sixteen bit dimensions, a plane count and a bit count, with no compression
    // field, no mask offsets and no colour count. `BITMAPINFOHEADER` and its
    // longer successors start at forty and carry all three.
    let core = header_size == CORE_HEADER;
    if !core && header_size < INFO_HEADER {
        return Err(ImgSeqError::new(format!(
            "the bitmap header states {header_size} bytes, which is too small"
        )));
    }

    let (width, stored_height) = if core {
        (
            i32::from(i16_at(data, offset + 4).ok_or_else(truncated)?),
            i32::from(i16_at(data, offset + 6).ok_or_else(truncated)?),
        )
    } else {
        (
            i32_at(data, offset + 4).ok_or_else(truncated)?,
            i32_at(data, offset + 8).ok_or_else(truncated)?,
        )
    };
    if width <= 0 || stored_height == 0 {
        return Err(ImgSeqError::new("the header states no pixels"));
    }
    if width > MAX_SIDE || stored_height.abs() > MAX_SIDE {
        return Err(ImgSeqError::new(
            "the header states a size no decoder accepts",
        ));
    }
    // A top-down bitmap came with the information header; the older one cannot
    // state one, so a negative height there is refused rather than guessed at.
    let top_down = stored_height < 0;
    if core && top_down {
        return Err(ImgSeqError::new(
            "a BITMAPCOREHEADER bitmap cannot be stored top-down",
        ));
    }
    let height = stored_height.unsigned_abs();

    let (planes_at, bits_at) = if core { (8, 10) } else { (12, 14) };
    let planes = u16_at(data, offset + planes_at).ok_or_else(truncated)?;
    if planes != 1 {
        return Err(ImgSeqError::new("the header states more than one plane"));
    }
    let bit_count = u16_at(data, offset + bits_at).ok_or_else(truncated)?;
    // The older header has no compression field, and the format defines only
    // the uncompressed form for it.
    let compression = if core {
        BI_RGB
    } else {
        u32_at(data, offset + 16).ok_or_else(truncated)?
    };
    // A top-down bitmap cannot be compressed, except by the bitfields method,
    // which is not compression in the run-length sense.
    if top_down && compression != BI_RGB && compression != BI_BITFIELDS {
        return Err(ImgSeqError::new(
            "a top-down bitmap cannot use run-length compression",
        ));
    }

    // The alpha flag is read from the masks, so it is false here whatever the
    // bit count, which is what makes a thirty-two bit BI_RGB bitmap drop its
    // fourth byte.
    let image_type = match compression {
        BI_RGB => match bit_count {
            1 | 2 | 4 | 8 => ImageType::Palette,
            16 => ImageType::Rgb16,
            24 => ImageType::Rgb24,
            32 => ImageType::Rgb32,
            other => {
                return Err(ImgSeqError::new(format!(
                    "a {other} bit BI_RGB bitmap is not supported"
                )));
            }
        },
        BI_RLE8 => {
            if bit_count != 8 {
                return Err(ImgSeqError::new("BI_RLE8 needs eight bits a pixel"));
            }
            ImageType::Rle8
        }
        BI_RLE4 => {
            if bit_count != 4 {
                return Err(ImgSeqError::new("BI_RLE4 needs four bits a pixel"));
            }
            ImageType::Rle4
        }
        BI_BITFIELDS => match bit_count {
            16 => ImageType::Bitfields16,
            32 => ImageType::Bitfields32,
            other => {
                return Err(ImgSeqError::new(format!(
                    "a {other} bit bitfields bitmap is not supported"
                )));
            }
        },
        BI_JPEG | BI_PNG => {
            return Err(ImgSeqError::new(
                "an embedded JPEG or PNG bitmap is not supported",
            ));
        }
        11..=13 => return Err(ImgSeqError::new("a CMYK bitmap is not supported")),
        other => {
            return Err(ImgSeqError::new(format!(
                "compression method {other} is not supported"
            )));
        }
    };

    // Where the masks live, and whether a fourth one follows them. The alpha
    // mask exists only in a header with room for it: the information header and
    // the V2 one have three masks after them and no fourth.
    let masks_after_header = matches!(image_type, ImageType::Bitfields16 | ImageType::Bitfields32);
    let alpha_mask_offset = match header_size {
        // The masks begin at byte forty of the DIB header, so the alpha one is
        // twelve bytes further on. Only a header with room for it has one.
        V3_HEADER | V4_HEADER | V5_HEADER => Some(offset + 40 + 12),
        _ => None,
    };
    let (mut red, mut green, mut blue, mut alpha) = (
        Bitfield { shift: 0, len: 0 },
        Bitfield { shift: 0, len: 0 },
        Bitfield { shift: 0, len: 0 },
        Bitfield { shift: 0, len: 0 },
    );
    let mut has_alpha = false;

    if masks_after_header {
        // The three colour masks always sit right after a forty byte
        // information header, whether the header itself is longer.
        let mask_start = if header_size >= V4_HEADER {
            offset + 16 + 24
        } else {
            offset + INFO_HEADER as usize
        };
        let max_len = u32::from(bit_count);
        let parse = |raw: u32| Bitfield::from_mask(raw, max_len);
        let (r, g, b) = (
            u32_at(data, mask_start).ok_or_else(truncated)?,
            u32_at(data, mask_start + 4).ok_or_else(truncated)?,
            u32_at(data, mask_start + 8).ok_or_else(truncated)?,
        );
        red = parse(r).ok_or_else(|| ImgSeqError::new("the red mask is not contiguous"))?;
        green = parse(g).ok_or_else(|| ImgSeqError::new("the green mask is not contiguous"))?;
        blue = parse(b).ok_or_else(|| ImgSeqError::new("the blue mask is not contiguous"))?;
        if red.len == 0 || green.len == 0 || blue.len == 0 {
            return Err(ImgSeqError::new("a colour mask is missing"));
        }
        if let Some(alpha_offset) = alpha_mask_offset {
            let raw = u32_at(data, alpha_offset).ok_or_else(truncated)?;
            // Alpha is handed out only when this mask has bits in it.
            if raw != 0 {
                alpha = parse(raw)
                    .ok_or_else(|| ImgSeqError::new("the alpha mask is not contiguous"))?;
                has_alpha = true;
            }
        }
    }

    // A palette follows the header, and any masks stored after it.
    let palette_start = match image_type {
        ImageType::Bitfields16 | ImageType::Bitfields32 if header_size < V4_HEADER => {
            offset + INFO_HEADER as usize + 12
        }
        _ => offset + header_size as usize,
    };
    let mut palette = Vec::new();
    if matches!(
        image_type,
        ImageType::Palette | ImageType::Rle4 | ImageType::Rle8
    ) {
        // The core header has no colour count, so a palette page states its depth
        // and every entry follows; the information header has one and a writer may
        // leave it at zero.
        let entries = if !core && header_size >= INFO_HEADER {
            let stated = u32_at(data, offset + 32).ok_or_else(truncated)?;
            if stated == 0 {
                1usize << bit_count
            } else {
                stated as usize
            }
        } else {
            1usize << bit_count
        };
        if entries > 256 {
            return Err(ImgSeqError::new("the palette states more than 256 entries"));
        }
        // A core header's entries are blue, green and red. An information header's
        // are the same three with a fourth reserved byte after them, and a table
        // read as the other would step a byte at a time and land on nothing.
        let entry_bytes = if core { 3 } else { 4 };
        let mut at = palette_start;
        for _ in 0..entries {
            let entry = data
                .get(at..at + entry_bytes)
                .ok_or_else(|| ImgSeqError::new("the palette is truncated"))?;
            palette.push([entry[2], entry[1], entry[0]]);
            at += entry_bytes;
        }
        // An empty palette still has to answer, so a file that states none gets
        // the default grey ramp the format implies.
        if palette.is_empty() {
            for index in 0..(1usize << bit_count) {
                let value = expand(index as u16, u32::from(bit_count));
                palette.push([value, value, value]);
            }
        }
        Ok(Header {
            width: width as u32,
            height,
            top_down,
            bit_count,
            image_type,
            data_offset: at,
            palette,
            red,
            green,
            blue,
            alpha,
            has_alpha,
        })
    } else {
        Ok(Header {
            width: width as u32,
            height,
            top_down,
            bit_count,
            image_type,
            data_offset: palette_start,
            palette,
            red,
            green,
            blue,
            alpha,
            has_alpha,
        })
    }
}

/// The header of the bare DIB an icon holds.
///
/// Two adjustments are what make an icon's bitmap different from a `.bmp`, and
/// both are the format's rather than this reader's:
///
/// - The stored height is **halved**. An icon states twice the real height to
///   account for the AND mask that follows the pixels, whether or not one is
///   there, so a 32 by 32 icon holds a DIB that says 64.
/// - The alpha channel is **added**, so a thirty-two bit `BI_RGB` payload keeps
///   its fourth byte. That is the opposite of the rule for a `.bmp`, where the
///   same bytes have the same fourth byte dropped, and it is the difference the
///   plan names.
///
/// A payload that is not thirty-two bits deep is refused here rather than
/// deeper in, so the probe and the decode cannot disagree about whether a file
/// is readable.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the DIB is malformed or too shallow.
pub(crate) fn header_ico(data: &[u8], start: usize) -> Result<Header> {
    let mut header = header(data, start, false)?;
    if !matches!(header.image_type, ImageType::Rgb32 | ImageType::Bitfields32) {
        return Err(ImgSeqError::new(format!(
            "an icon DIB payload of {} bits is not supported",
            header.bit_count
        )));
    }
    header.height /= 2;
    header.has_alpha = true;
    Ok(header)
}
fn truncated() -> ImgSeqError {
    ImgSeqError::new("the bitmap is truncated")
}

/// A window the header of a bitmap is read through.
///
/// A header is at most a hundred and twenty-four bytes plus its masks, so this is
/// far past any real one; a file whose header is larger is refused by the parse
/// rather than read further.
const HEAD_WINDOW: usize = 64 * 1024;

/// Bytes of one padded row of pixels, and of the whole pixel area.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the area does not fit this build.
fn geometry(header: &Header) -> Result<(usize, usize)> {
    // Every uncompressed row is padded to a multiple of four bytes.
    let row_bytes = (header.width as usize * header.bit_count as usize).div_ceil(32) * 4;
    let needed = row_bytes
        .checked_mul(header.height as usize)
        .ok_or_else(|| ImgSeqError::new("the bitmap is too large"))?;
    Ok((row_bytes, needed))
}

/// Opens `path` and reads its header, leaving the reader at the pixels.
///
/// Answers `None` for every shape the stream does not cover, so the caller falls
/// back to [`decode`].
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read, its header cannot be
/// parsed, or it holds fewer pixels than the header states.
fn prepare(path: &Path) -> Result<Option<(Header, BufReader<File>)>> {
    let mut reader = BufReader::new(File::open(path).map_err(|e| image_error("open", path, e))?);
    let mut window = vec![0u8; HEAD_WINDOW];
    let read = reader
        .read(&mut window)
        .map_err(|e| image_error("open", path, e))?;
    window.truncate(read);
    let header = header(&window, 0, true).map_err(|e| image_error("decode", path, e))?;
    if header.has_alpha || matches!(header.image_type, ImageType::Rle4 | ImageType::Rle8) {
        return Ok(None);
    }
    let (_, needed) = geometry(&header)?;
    let length = reader
        .seek(SeekFrom::End(0))
        .map_err(|e| image_error("open", path, e))?;
    let offset = u64::try_from(header.data_offset).unwrap_or(u64::MAX);
    if offset.saturating_add(u64::try_from(needed).unwrap_or(u64::MAX)) > length {
        return Err(ImgSeqError::new(format!(
            "the file holds {} bytes of pixels where the header states {needed}",
            length.saturating_sub(offset)
        )));
    }
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|e| image_error("open", path, e))?;
    Ok(Some((header, reader)))
}

/// A bitmap whose rows can be written straight into the frames.
///
/// The rows are the frame's rows, only possibly in the other order: a bitmap
/// stored top-down is already right, and one stored bottom-up is the same rows
/// reversed, which is a choice of target row rather than a transpose. So the
/// whole-picture buffer that [`pixels`] builds for `pixels` to be copied out of
/// is not needed here.
///
/// `unpack_row` still does the work a row needs -- the palette lookup, the bit
/// expansion, the blue-first exchange -- into one scratch row that is reused,
/// which is what keeps this a saving rather than a second decoder. The forms it
/// does not cover keep the buffered path: a run-length encoded payload, whose
/// runs cross rows, and a four channel one, whose alpha plane needs a placer
/// this does not have.
#[derive(Debug)]
pub struct Rows {
    path: std::path::PathBuf,
    /// What the header states, kept so a read does not parse it again.
    header: Option<Header>,
    /// The reader the preparation opened, positioned at the first byte of the
    /// pixels. `None` is a stream that came from [`RowStream::duplicate`], which
    /// prepares itself on its way into a fill: duplicating cannot report the
    /// failure that opening and parsing can.
    raster: Option<BufReader<File>>,
}

impl Rows {
    /// The header and the reader positioned at the pixels, prepared here when
    /// this stream is a duplicate that has not read anything yet.
    fn raster(&mut self) -> Result<(&Header, &mut BufReader<File>)> {
        if self.raster.is_none() {
            let Some((header, reader)) = prepare(&self.path)? else {
                return Err(ImgSeqError::new(format!(
                    "'{}' is no longer a bitmap this reader walks a row at a time",
                    self.path.display()
                )));
            };
            self.header = Some(header);
            self.raster = Some(reader);
        }
        let header = self
            .header
            .as_ref()
            .ok_or_else(|| ImgSeqError::new("the raster header is missing"))?;
        let reader = self
            .raster
            .as_mut()
            .ok_or_else(|| ImgSeqError::new("the raster reader is missing"))?;
        Ok((header, reader))
    }
}

impl RowStream for Rows {
    fn has_alpha(&self) -> bool {
        false
    }

    fn fill(&mut self, mut sink: RowSink<'_>) -> Result<DecodeTimings> {
        let read_started = std::time::Instant::now();
        let (header, reader) = self.raster()?;
        let width = header.width as usize;
        let height = header.height as usize;
        let (row_bytes, _) = geometry(header)?;
        let mut source = vec![0u8; row_bytes];
        let mut line = vec![0u8; width * 3];
        for row in 0..height {
            reader
                .read_exact(&mut source)
                .map_err(|_| ImgSeqError::new(format!("the bitmap ends in row {row}")))?;
            let target_row = if header.top_down {
                row
            } else {
                height - 1 - row
            };
            unpack_row(header, &source, &mut line, width, 3);
            sink.place_rgb8(&line, target_row)
                .ok_or_else(|| ImgSeqError::new("the frame holds no three colour planes"))?;
        }
        // A stream is read once. Letting the reader go here means a second fill
        // starts from the pixels again rather than from where this one stopped.
        self.raster = None;
        Ok(DecodeTimings {
            open: std::time::Duration::ZERO,
            metadata: std::time::Duration::ZERO,
            buffer: std::time::Duration::ZERO,
            read: read_started.elapsed(),
        })
    }

    fn duplicate(&self) -> Box<dyn RowStream> {
        Box::new(Self {
            path: self.path.clone(),
            header: None,
            raster: None,
        })
    }
}

/// Prepares a bitmap whose rows will be written straight into the frames.
///
/// Answers `None` for every shape the stream does not cover, so the caller falls
/// back to [`decode`].
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the header cannot be read back.
pub fn stream(info: &ImageInfo) -> Result<Option<DecodedImage>> {
    let Some((header, raster)) = prepare(&info.path)? else {
        return Ok(None);
    };
    Ok(Some(DecodedImage {
        width: info.width,
        height: info.height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Stream(Box::new(Rows {
            path: info.path.clone(),
            header: Some(header),
            raster: Some(raster),
        })),
        timings: DecodeTimings {
            open: std::time::Duration::ZERO,
            metadata: std::time::Duration::ZERO,
            buffer: std::time::Duration::ZERO,
            read: std::time::Duration::ZERO,
        },
    }))
}

/// The samples of one bitmap, decoded into `width * height * channels`.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the pixel data is short or malformed.
pub fn pixels(header: &Header, data: &[u8]) -> Result<Vec<u8>> {
    let width = header.width as usize;
    let height = header.height as usize;
    let channels = header.channels();
    let size = width
        .checked_mul(height)
        .and_then(|count| count.checked_mul(channels))
        .ok_or_else(|| ImgSeqError::new("the bitmap is too large"))?;
    let mut out = vec![0u8; size];
    if channels == 4 {
        // A four channel buffer starts opaque, so a payload that states no alpha --
        // the palette case an icon forces one onto -- is not handed out transparent.
        for alpha in out.iter_mut().skip(3).step_by(4) {
            *alpha = 255;
        }
    }

    match header.image_type {
        ImageType::Rle4 | ImageType::Rle8 => {
            rle(header, data, &mut out)?;
        }
        _ => {
            // Every uncompressed row is padded to a multiple of four bytes.
            let row_bits = width * header.bit_count as usize;
            let row_bytes = row_bits.div_ceil(32) * 4;
            let needed = row_bytes
                .checked_mul(height)
                .ok_or_else(|| ImgSeqError::new("the bitmap is too large"))?;
            let body = data
                .get(header.data_offset..header.data_offset + needed)
                .ok_or_else(|| {
                    ImgSeqError::new(format!(
                        "the file holds {} bytes of pixels where the header states {needed}",
                        data.len().saturating_sub(header.data_offset)
                    ))
                })?;
            for row in 0..height {
                let source = &body[row * row_bytes..(row + 1) * row_bytes];
                let target_row = if header.top_down {
                    row
                } else {
                    height - 1 - row
                };
                let target =
                    &mut out[target_row * width * channels..(target_row + 1) * width * channels];
                unpack_row(header, source, target, width, channels);
            }
        }
    }
    Ok(out)
}

/// Expands one stored row into its decoded samples.
fn unpack_row(header: &Header, source: &[u8], target: &mut [u8], width: usize, channels: usize) {
    match header.image_type {
        ImageType::Palette => {
            for x in 0..width {
                let index = match header.bit_count {
                    8 => source[x] as usize,
                    4 => {
                        let byte = source[x / 2];
                        if x % 2 == 0 {
                            (byte >> 4) as usize
                        } else {
                            (byte & 0x0F) as usize
                        }
                    }
                    2 => {
                        let byte = source[x / 4];
                        ((byte >> (6 - 2 * (x % 4))) & 0b11) as usize
                    }
                    _ => {
                        let byte = source[x / 8];
                        ((byte >> (7 - (x % 8))) & 1) as usize
                    }
                };
                let entry = header.palette.get(index).copied().unwrap_or([0, 0, 0]);
                target[x * channels..x * channels + 3].copy_from_slice(&entry);
            }
        }
        ImageType::Rgb24 => {
            for x in 0..width {
                let at = x * 3;
                // Stored blue, green, red.
                target[x * channels] = source[at + 2];
                target[x * channels + 1] = source[at + 1];
                target[x * channels + 2] = source[at];
            }
        }
        ImageType::Rgb32 => {
            for x in 0..width {
                let at = x * 4;
                target[x * channels] = source[at + 2];
                target[x * channels + 1] = source[at + 1];
                target[x * channels + 2] = source[at];
                if channels == 4 {
                    // Only an RGBA32 bitmap reaches here, and its fourth byte
                    // is already the eight bit alpha.
                    target[x * channels + 3] = source[at + 3];
                }
            }
        }
        ImageType::Rgb16 => {
            let (r, g, b) = MASK_555;
            let (red, green, blue) = (
                Bitfield::from_mask(r, 16).unwrap_or(Bitfield { shift: 0, len: 0 }),
                Bitfield::from_mask(g, 16).unwrap_or(Bitfield { shift: 0, len: 0 }),
                Bitfield::from_mask(b, 16).unwrap_or(Bitfield { shift: 0, len: 0 }),
            );
            for x in 0..width {
                let at = x * 2;
                let value = u32::from(u16::from_le_bytes([source[at], source[at + 1]]));
                target[x * channels] = red.read(value);
                target[x * channels + 1] = green.read(value);
                target[x * channels + 2] = blue.read(value);
            }
        }
        ImageType::Bitfields16 => {
            for x in 0..width {
                let at = x * 2;
                let value = u32::from(u16::from_le_bytes([source[at], source[at + 1]]));
                target[x * channels] = header.red.read(value);
                target[x * channels + 1] = header.green.read(value);
                target[x * channels + 2] = header.blue.read(value);
                if channels == 4 {
                    target[x * channels + 3] = header.alpha.read(value);
                }
            }
        }
        ImageType::Bitfields32 => {
            for x in 0..width {
                let at = x * 4;
                let value = u32::from_le_bytes([
                    source[at],
                    source[at + 1],
                    source[at + 2],
                    source[at + 3],
                ]);
                target[x * channels] = header.red.read(value);
                target[x * channels + 1] = header.green.read(value);
                target[x * channels + 2] = header.blue.read(value);
                if channels == 4 {
                    target[x * channels + 3] = header.alpha.read(value);
                }
            }
        }
        ImageType::Rle4 | ImageType::Rle8 => {}
    }
}

/// Expands a run-length encoded bitmap.
///
/// The four escapes are the whole of it: end of line, end of bitmap, a delta
/// that moves the cursor without writing, and an absolute run of literals.
fn rle(header: &Header, data: &[u8], out: &mut [u8]) -> Result<()> {
    let width = header.width as usize;
    let height = header.height as usize;
    let channels = header.channels();
    let four_bit = header.image_type == ImageType::Rle4;

    let mut cursor = header.data_offset;
    let mut x = 0usize;
    // A run-length bitmap is stored bottom-up unless it says otherwise, and the
    // cursor counts rows from the bottom in that case.
    let mut stored_row = 0usize;
    let place = |out: &mut [u8], x: usize, row: usize, index: usize| {
        let target_row = if header.top_down {
            row
        } else {
            height - 1 - row
        };
        if x >= width || target_row >= height {
            return;
        }
        let at = (target_row * width + x) * channels;
        let entry = header.palette.get(index).copied().unwrap_or([0, 0, 0]);
        out[at..at + 3].copy_from_slice(&entry);
    };

    loop {
        let first = *data.get(cursor).ok_or_else(truncated)?;
        cursor += 1;
        if first != 0 {
            // An encoded run: `first` pixels of the next value.
            let value = *data.get(cursor).ok_or_else(truncated)?;
            cursor += 1;
            for step in 0..first as usize {
                let index = if four_bit {
                    if step % 2 == 0 {
                        (value >> 4) as usize
                    } else {
                        (value & 0x0F) as usize
                    }
                } else {
                    value as usize
                };
                place(out, x, stored_row, index);
                x += 1;
            }
            continue;
        }
        let second = *data.get(cursor).ok_or_else(truncated)?;
        cursor += 1;
        match second {
            RLE_EOL => {
                x = 0;
                stored_row += 1;
                if stored_row >= height {
                    // Some encoders omit the end of bitmap after the last line.
                    break;
                }
            }
            RLE_EOF => break,
            RLE_DELTA => {
                let dx = *data.get(cursor).ok_or_else(truncated)?;
                let dy = *data.get(cursor + 1).ok_or_else(truncated)?;
                cursor += 2;
                x += dx as usize;
                stored_row += dy as usize;
            }
            count => {
                // Absolute mode: `count` literal indices, padded to an even
                // number of bytes.
                let count = count as usize;
                let bytes = if four_bit { count.div_ceil(2) } else { count };
                let literals = data.get(cursor..cursor + bytes).ok_or_else(truncated)?;
                cursor += bytes;
                if bytes % 2 == 1 {
                    cursor += 1;
                }
                for step in 0..count {
                    let index = if four_bit {
                        let byte = literals[step / 2];
                        if step % 2 == 0 {
                            (byte >> 4) as usize
                        } else {
                            (byte & 0x0F) as usize
                        }
                    } else {
                        literals[step] as usize
                    };
                    place(out, x, stored_row, index);
                    x += 1;
                }
            }
        }
        if stored_row >= height {
            break;
        }
    }
    Ok(())
}

/// What a bitmap states, when this module reads the file.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
/// The path-taking form, which this module's own tests use. The probe asks
/// through [`image_info_headed`] with a head it already holds, so this is not
/// on the path a probe takes.
#[cfg(test)]
pub fn image_info(
    path: &Path,
    _apply_rotation: bool,
    route: Option<crate::formats::identify::Format>,
) -> Result<Option<ImageInfo>> {
    let data = image_head(path).map_err(|error| image_error("open", path, error))?;
    image_info_headed(path, _apply_rotation, route, &data)
}

/// [`image_info`] from a head the caller has already read.
///
/// A bitmap's header, its masks and its palette are all inside the window, which
/// is why this reader is one of the six that can take it.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info_headed(
    path: &Path,
    _apply_rotation: bool,
    route: Option<crate::formats::identify::Format>,
    data: &[u8],
) -> Result<Option<ImageInfo>> {
    if !route.map_or_else(
        || owns(path),
        |saved| saved == crate::formats::identify::Format::Bmp,
    ) {
        return Ok(None);
    }
    // A `.bmp` that does not start with `BM` is not one however it is named, so
    // it is declined rather than refused: something else can still read it.
    if data.len() < 2 || data[..2] != *b"BM" {
        return Ok(None);
    }
    let header = header(data, 0, true).map_err(|error| image_error("identify", path, error))?;
    Ok(Some(ImageInfo {
        route: None,
        subimage: None,
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type: header.color_type(),
        original_color_type: if header.has_alpha {
            SourceColorType::Rgba8
        } else {
            SourceColorType::Rgb8
        },
        // A bitmap states no profile, no colour description and no orientation.
        has_icc_profile: false,
        icc_profile: None,
        cicp: None,
        chroma_location: None,
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: header.format(),
    }))
}

/// Decodes a bitmap into one interleaved buffer.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or is malformed.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    decode_dib(&info.path, None, info)
}

/// Decodes a bitmap, or the bare DIB an icon holds.
///
/// `payload` is the DIB when the caller already has one, which is how
/// [`crate::formats::ico`] hands over an icon's entry; `None` means read the
/// whole file named by `info.path`.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or is malformed.
pub(crate) fn decode_dib(
    path: &Path,
    payload: Option<&[u8]>,
    info: &ImageInfo,
) -> Result<DecodedImage> {
    let open_started = std::time::Instant::now();
    let owned;
    let data = match payload {
        Some(bytes) => bytes,
        None => {
            owned = std::fs::read(path).map_err(|error| image_error("open", path, error))?;
            &owned
        }
    };
    let open = open_started.elapsed();

    let file_header = payload.is_none();
    let header =
        header(data, 0, file_header).map_err(|error| image_error("decode", path, error))?;
    if (header.width, header.height) != (info.width, info.height) {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {}x{}, now {}x{})",
            path.display(),
            info.width,
            info.height,
            header.width,
            header.height,
        )));
    }

    let read_started = std::time::Instant::now();
    let buffer = pixels(&header, data).map_err(|error| image_error("decode", path, error))?;
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

    /// The preparation leaves the reader at the pixels, and the padded rows it
    /// reads are that pixel area and nothing else.
    #[test]
    fn the_row_sink_reads_the_padded_rows() {
        let path = fixture("bmp-depth24.bmp");
        let (header, mut reader) = prepare(&path)
            .expect("the fixture is readable")
            .expect("a 24 bit bitmap is a row sink");
        let (row_bytes, needed) = geometry(&header).expect("the area fits this build");
        let mut source = vec![0u8; row_bytes];
        let mut rows = Vec::new();
        for _ in 0..header.height as usize {
            reader.read_exact(&mut source).expect("a row of pixels");
            rows.extend_from_slice(&source);
        }
        let file = std::fs::read(&path).expect("the fixture");
        let start = header.data_offset;
        assert_eq!(
            rows,
            file[start..start + needed],
            "the rows are the pixel area"
        );
    }

    /// The older header reads as the picture it holds, in both of the shapes it
    /// differs from the information header in: signed sixteen bit dimensions and
    /// a three byte palette table. The eight bit file is the one that would fail
    /// first if the table were read four bytes at a time.
    ///
    /// The expected pixels are the ones Pillow reads from the same two files,
    /// which is the only independent answer there is for a format this tree
    /// wrote itself.
    #[test]
    fn a_core_header_bitmap_reads_as_the_picture_it_holds() {
        for (name, width, height, bit_count, expected) in [
            ("bmp-core-24.bmp", 1, 1, 24, vec![3, 5, 7]),
            (
                "bmp-core-4.bmp",
                2,
                2,
                4,
                // Top row first, which is the order Pillow hands the same
                // file back in.
                vec![
                    200, 200, 200, 90, 0, 0, //
                    0, 60, 0, 0, 0, 30, //
                ],
            ),
        ] {
            let path = fixture(name);
            let (header, _reader) = prepare(&path)
                .expect("the fixture is readable")
                .expect("a core header bitmap is a row sink");
            assert_eq!((header.width, header.height), (width, height), "{name}");
            assert_eq!(header.bit_count, bit_count, "{name}");
            assert!(
                !header.top_down,
                "{name}: the older header cannot be top-down"
            );
            assert!(!header.has_alpha, "{name}: there is no mask to hand out");

            let file = std::fs::read(&path).expect("the fixture is readable");
            assert_eq!(
                pixels(&header, &file).expect("the rows decode"),
                expected,
                "{name}: the picture is the one Pillow reads"
            );
        }
    }

    /// The round-to-nearest expansion, against the values the upstream tables
    /// hold. Five bits of three is the case that separates this from a shift
    /// and or, which gives 24.
    #[test]
    fn a_narrow_channel_widens_to_the_nearest_eighth() {
        assert_eq!(expand(3, 5), 25);
        assert_eq!(expand(1, 5), 8);
        assert_eq!(expand(31, 5), 255);
        assert_eq!(expand(1, 2), 85);
        assert_eq!(expand(1, 4), 17);
        assert_eq!(expand(1, 6), 4);
        assert_eq!(expand(1, 7), 2);
        assert_eq!(expand(255, 8), 255);
        assert_eq!(expand(0, 0), 0);
        // The whole five bit range, which is what a 555 bitmap walks.
        let table: Vec<u8> = (0..32).map(|v| expand(v, 5)).collect();
        assert_eq!(
            table,
            vec![
                0, 8, 16, 25, 33, 41, 49, 58, 66, 74, 82, 90, 99, 107, 115, 123, 132, 140, 148,
                156, 165, 173, 181, 189, 197, 206, 214, 222, 230, 239, 247, 255,
            ]
        );
    }

    /// Every fixture this module owns, and what its header states. This is the
    /// table the port has to keep, and it is read against the recorded baseline
    /// in `tests/readalpha.vpy`.
    #[test]
    fn the_fixtures_state_the_subtypes_they_are_named_for() {
        for (name, width, height, alpha, bit_count) in [
            ("bmp-depth1.bmp", 37, 23, false, 1),
            ("bmp-depth4.bmp", 37, 23, false, 4),
            ("bmp-depth8.bmp", 37, 23, false, 8),
            ("bmp-depth24.bmp", 37, 23, false, 24),
            ("bmp-rle4.bmp", 37, 23, false, 4),
            ("bmp-rle8.bmp", 37, 23, false, 8),
            ("bmp-topdown24.bmp", 37, 23, false, 24),
            // A thirty-two bit BI_RGB bitmap drops its fourth byte.
            ("bmp-rgb32.bmp", 37, 23, false, 32),
            ("bmp-rgb32-v5.bmp", 37, 23, false, 32),
            // Alpha only when the bitfield mask has bits in it.
            ("bmp-bitfields32.bmp", 37, 23, true, 32),
            ("bmp-bitfields32-noalpha.bmp", 37, 23, false, 32),
            ("bmp-v5-bitfields32.bmp", 37, 23, true, 32),
            ("bmp-bitfields16.bmp", 37, 23, false, 16),
        ] {
            let info = image_info(&fixture(name), true, None)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .unwrap_or_else(|| panic!("{name} is taken over"));
            assert_eq!((info.width, info.height), (width, height), "{name}");
            assert_eq!(info.color_type == ColorType::Rgba8, alpha, "{name}");
            assert_eq!(info.format, PixelFormat::Rgb8, "{name}");
            let data = std::fs::read(fixture(name)).expect("the fixture is read");
            let header = header(&data, 0, true).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(header.bit_count, bit_count, "{name}");
        }
    }

    /// The fixture that separates the two alpha rules: one bitmap whose
    /// bitfield alpha mask is zero, and one whose mask is set.
    #[test]
    fn the_bitfield_alpha_mask_decides_the_alpha_plane() {
        let with = image_info(&fixture("bmp-bitfields32.bmp"), true, None)
            .expect("read")
            .expect("taken over");
        let without = image_info(&fixture("bmp-bitfields32-noalpha.bmp"), true, None)
            .expect("read")
            .expect("taken over");
        assert_eq!(with.color_type, ColorType::Rgba8);
        assert_eq!(without.color_type, ColorType::Rgb8);
        assert_eq!(crate::pixel::alpha_channel(without.color_type), None);
    }

    /// A thirty-two bit `BI_RGB` bitmap states a fourth byte and it is dropped,
    /// while the bitfields one with a mask keeps its alpha. The two files hold
    /// the same picture, so the difference is the rule and not the samples.
    #[test]
    fn a_thirty_two_bit_rgb_bitmap_drops_its_fourth_byte() {
        let info = image_info(&fixture("bmp-rgb32.bmp"), true, None)
            .expect("read")
            .expect("taken over");
        assert_eq!(info.color_type, ColorType::Rgb8);
        let decoded = decode(&info).expect("the file decodes");
        let Pixels::Interleaved {
            color_type, buffer, ..
        } = decoded.pixels
        else {
            panic!("a bitmap hands out one interleaved buffer");
        };
        assert_eq!(color_type, ColorType::Rgb8);
        assert_eq!(buffer.len(), 37 * 23 * 3);
    }

    /// A file that is not a bitmap is declined, and one whose header names an
    /// unported subtype is refused by name rather than guessed at.
    #[test]
    fn an_unported_subtype_is_refused_by_name() {
        // A core header stored top-down, which the format does not define.
        let mut core = b"BM".to_vec();
        core.extend_from_slice(&[0u8; 12]);
        core.extend_from_slice(&12u32.to_le_bytes());
        core.extend_from_slice(&1i16.to_le_bytes());
        core.extend_from_slice(&(-1i16).to_le_bytes());
        core.extend_from_slice(&1u16.to_le_bytes());
        core.extend_from_slice(&24u16.to_le_bytes());
        core.extend_from_slice(&[0u8; 8]);
        let error = header(&core, 0, true).expect_err("a core header cannot be top-down");
        assert!(
            error.to_string().contains("cannot be stored top-down"),
            "{error}"
        );

        // A png named as a bitmap is declined, not refused.
        assert!(
            image_info(&fixture("cicp-rgb8.png"), true, None)
                .expect("a png is not ours")
                .is_none()
        );
        assert!(!owns(Path::new("a.png")));
        assert!(owns(Path::new("a.bmp")));
        assert!(owns(Path::new("a.BMP")));
        assert!(owns(Path::new("a.dib")));
    }
}
