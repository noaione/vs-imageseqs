//! Truevision TGA, read by this tree's own port of the `image` decoder.
//!
//! The port is [`image 0.25.10`'s `src/codecs/tga/decoder.rs`][upstream]. The
//! format is an eighteen byte header, an optional colour map, and pixel data
//! that is either raw or run-length encoded; the whole reader fits in this file.
//!
//! [upstream]: https://github.com/image-rs/image/blob/v0.25.10/src/codecs/tga/decoder.rs
//!
//! Three behaviours are inherited rather than invented:
//!
//! - **A thirty-two bit image stating zero attribute bits is still `Rgba8`.**
//!   The layout follows `(attribute_bits, other_bits, is_colour)`, and both
//!   `(0, 32)` and `(8, 24)` reach four channels. This is the *opposite* of the
//!   rule for a BMP, where the same fourth byte is dropped, and
//!   `tests/fixtures/tga-rgb32-attr0.tga` exists to hold that difference still.
//! - **The samples are stored blue first** and the red and blue bytes are
//!   swapped after any expansion, so a colour map's entries are expanded as they
//!   are stored and fixed up together with everything else.
//! - **The descriptor's two direction bits move pixels, not the header.** A
//!   bottom-up image has its rows flipped and a right-to-left one its columns
//!   flopped, and both happen on the *raw* bytes before any expansion, so the
//!   stride is the stored pixel size rather than the decoded one.
//!
//! A fifteen and sixteen bit image is widened by the same round-to-nearest table
//! the bitmap reader uses: five bits of `3` become `25`, where a shift and or
//! gives `24`. See [`crate::formats::bmp::expand`].

use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use crate::{
    decoder::{
        DecodeTimings, DecodedImage, ImageInfo, Pixels, RowSink, RowStream, image_error, image_head,
    },
    error::{ImgSeqError, Result},
    formats::bmp,
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// Bytes of the fixed header, before the image id and the colour map.
const HEADER: usize = 18;

/// The low nibble of the descriptor counts attribute bits.
const ALPHA_BIT_MASK: u8 = 0b1111;

/// Descriptor bits that say the columns run right to left and the rows top to
/// bottom.
const RIGHT_TO_LEFT: u8 = 1 << 4;
const TOP_TO_BOTTOM: u8 = 1 << 5;

/// The image types the format defines. One to three are raw and nine to eleven
/// are the same three run-length encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageType {
    ColorMapped,
    TrueColor,
    GrayScale,
}

impl ImageType {
    const fn from_number(value: u8) -> Option<(Self, bool)> {
        match value {
            1 => Some((Self::ColorMapped, false)),
            2 => Some((Self::TrueColor, false)),
            3 => Some((Self::GrayScale, false)),
            9 => Some((Self::ColorMapped, true)),
            10 => Some((Self::TrueColor, true)),
            11 => Some((Self::GrayScale, true)),
            _ => None,
        }
    }

    /// Whether the pixel values are colours rather than grey levels.
    ///
    /// This is the `is_color` of the upstream match, and it is what keeps the
    /// four channel arms from applying to a grayscale file.
    const fn is_color(self) -> bool {
        matches!(self, Self::ColorMapped | Self::TrueColor)
    }
}

/// A colour map, as the file stores it: entries of three or four bytes, blue
/// first, beginning at an index the header states.
#[derive(Debug)]
struct ColorMap {
    start: usize,
    entry_size: usize,
    bytes: Vec<u8>,
}

impl ColorMap {
    /// The entry for `index`, or `None` when the file does not hold it.
    fn get(&self, index: usize) -> Option<&[u8]> {
        let at = self
            .entry_size
            .checked_mul(index.checked_sub(self.start)?)?;
        self.bytes.get(at..at + self.entry_size)
    }
}

/// What one targa's header states.
#[derive(Debug)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    /// Bytes one stored pixel occupies, which is what the orientation fixup
    /// strides by.
    raw_bytes_per_pixel: usize,
    encoded: bool,
    /// The decoded layout.
    color_type: ColorType,
    /// The label property's value, which is not always the decoded layout: a
    /// fifteen bit image decodes to r,g,b but is labelled `Rgb5x1`.
    source: SourceColorType,
    descriptor: u8,
    color_map: Option<ColorMap>,
    /// Where the pixel data begins.
    pub data_offset: usize,
}

impl Header {
    /// The colour type a frame is written from.
    #[must_use]
    pub const fn color_type(&self) -> ColorType {
        self.color_type
    }

    /// The colour type the label property reports.
    #[must_use]
    pub const fn source(&self) -> SourceColorType {
        self.source
    }

    /// The format a frame is written from.
    ///
    /// Eight bits a channel, sixteen for a four channel layout, and nothing else
    /// has a depth of its own to report.
    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        match self.color_type {
            ColorType::L8 => PixelFormat::Gray8,
            ColorType::La8 => PixelFormat::Gray8,
            _ => PixelFormat::Rgb8,
        }
    }

    /// Bytes one decoded pixel occupies.
    const fn channels(&self) -> usize {
        match self.color_type {
            ColorType::L8 => 1,
            ColorType::La8 => 2,
            ColorType::Rgba8 => 4,
            _ => 3,
        }
    }
}

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    // Targa has no leading signature -- only an optional footer -- so its
    // extension is not a hint that content can override but the whole answer.
    // A type 2 header begins `00 00 02 00`, which is exactly CUR's magic, so a
    // content-first rule would hand every such Targa to the icon reader.
    crate::formats::identify::from_extension(path) == Some(crate::formats::identify::Format::Tga)
}

/// Parses the header, the image id and the colour map.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is truncated, states a size of zero, or
/// names an image type or a depth this reader does not implement.
pub fn header(data: &[u8]) -> Result<Header> {
    let head = data
        .get(..HEADER)
        .ok_or_else(|| ImgSeqError::new("the targa is truncated"))?;
    let id_length = head[0] as usize;
    let map_type = head[1];
    let Some((image_type, encoded)) = ImageType::from_number(head[2]) else {
        return Err(ImgSeqError::new(format!(
            "image type {} is not one this reader implements",
            head[2]
        )));
    };
    let map_first = u16::from_le_bytes([head[3], head[4]]) as usize;
    let map_length = u16::from_le_bytes([head[5], head[6]]) as usize;
    let map_entry_bits = head[7] as usize;
    let width = u16::from_le_bytes([head[12], head[13]]) as usize;
    let height = u16::from_le_bytes([head[14], head[15]]) as usize;
    let pixel_depth = head[16] as usize;
    let descriptor = head[17];

    if width == 0 || height == 0 {
        return Err(ImgSeqError::new("the header states no pixels"));
    }
    let raw_bytes_per_pixel = pixel_depth.div_ceil(8);

    // A colour-mapped image needs a map, and its indices have to fit the entries
    // they reach: a wider index than entry is refused rather than truncated.
    let color_map = if image_type == ImageType::ColorMapped {
        if map_type != 1 {
            return Err(ImgSeqError::new(
                "a colour mapped image must state a colour map type of one",
            ));
        }
        if !matches!(pixel_depth, 8 | 16) {
            return Err(ImgSeqError::new(
                "the colour map must use one or two byte indices",
            ));
        }
        if pixel_depth > map_entry_bits {
            return Err(ImgSeqError::new(
                "the colour map indices are wider than its entries",
            ));
        }
        let entry_size = map_entry_bits.div_ceil(8);
        if !matches!(entry_size, 3 | 4) {
            return Err(ImgSeqError::new(format!(
                "a colour map entry of {map_entry_bits} bits is not supported"
            )));
        }
        let start = HEADER + id_length;
        let length = map_length
            .checked_mul(entry_size)
            .ok_or_else(|| ImgSeqError::new("the colour map is too large"))?;
        let bytes = data
            .get(start..start + length)
            .ok_or_else(|| ImgSeqError::new("the colour map is truncated"))?
            .to_vec();
        Some(ColorMap {
            start: map_first,
            entry_size,
            bytes,
        })
    } else {
        if map_type != 0 && map_length != 0 {
            // A truecolour or grayscale image may carry an unused map; the data
            // offset still has to step over it.
        }
        None
    };

    // The colour map is bytes in the file whether or not this image uses it, so
    // the pixel data starts past it either way. Getting this wrong reads the map
    // as pixels, which is a wrong picture rather than a refusal.
    let map_bytes = if map_type == 1 {
        map_length * map_entry_bits.div_ceil(8)
    } else {
        0
    };
    let data_offset = HEADER + id_length + map_bytes;

    // The decoded layout, which is the upstream match written out. Attribute
    // bits come from the descriptor and the rest is what the depth leaves.
    let attrib = (descriptor & ALPHA_BIT_MASK) as usize;
    let total_bits = match &color_map {
        Some(map) => map.entry_size * 8,
        None => pixel_depth,
    };
    let other_bits = total_bits
        .checked_sub(attrib)
        .ok_or_else(|| ImgSeqError::new("the header states more alpha bits than pixel bits"))?;

    let is_color = image_type.is_color();
    let (color_type, source) = match (attrib, other_bits, is_color) {
        // The fourth byte is alpha whether or not the descriptor counts any,
        // which is the opposite of the rule for a bitmap.
        (0, 32, true) => (ColorType::Rgba8, SourceColorType::Rgba8),
        (8, 24, true) => (ColorType::Rgba8, SourceColorType::Rgba8),
        (0, 24, true) => (ColorType::Rgb8, SourceColorType::Rgb8),
        // The attribute bit of a five bit channel is not an alpha channel and
        // cannot be read as one.
        (1, 15, true) | (0, 15, true) | (0, 16, true) => (ColorType::Rgb8, SourceColorType::Rgb5x1),
        (8, 8, false) => (ColorType::La8, SourceColorType::La8),
        (0, 8, false) => (ColorType::L8, SourceColorType::L8),
        // An alpha-only image is handed out as grey.
        (8, 0, false) => (ColorType::L8, SourceColorType::A8),
        _ => {
            return Err(ImgSeqError::new(format!(
                "a targa of {pixel_depth} bits with {attrib} attribute bits is not supported"
            )));
        }
    };

    Ok(Header {
        width: width as u32,
        height: height as u32,
        raw_bytes_per_pixel,
        encoded,
        color_type,
        source,
        descriptor,
        color_map,
        data_offset,
    })
}

/// Reads the stored pixels, which is either a copy or a run-length expansion.
fn stored_pixels(header: &Header, data: &[u8]) -> Result<Vec<u8>> {
    let count = (header.width as usize)
        .checked_mul(header.height as usize)
        .and_then(|pixels| pixels.checked_mul(header.raw_bytes_per_pixel))
        .ok_or_else(|| ImgSeqError::new("the targa is too large"))?;
    let body = data
        .get(header.data_offset..)
        .ok_or_else(|| ImgSeqError::new("the targa is truncated"))?;
    let mut out = vec![0u8; count];
    if !header.encoded {
        let raw = body
            .get(..count)
            .ok_or_else(|| ImgSeqError::new("the pixel data is truncated"))?;
        out.copy_from_slice(raw);
        return Ok(out);
    }

    let stride = header.raw_bytes_per_pixel;
    let mut cursor = 0usize;
    let mut index = 0usize;
    while index < count {
        let packet = *body
            .get(cursor)
            .ok_or_else(|| ImgSeqError::new("the run-length data is truncated"))?;
        cursor += 1;
        if packet & 0x80 != 0 {
            // A run: one pixel repeated `packet + 1` times. The tail is clamped
            // rather than refused, because a run that overruns the picture is
            // what several encoders write for the last packet.
            let run = (packet & 0x7F) as usize + 1;
            let pixel = body
                .get(cursor..cursor + stride)
                .ok_or_else(|| ImgSeqError::new("the run-length data is truncated"))?;
            cursor += stride;
            for chunk in out[index..].chunks_exact_mut(stride).take(run) {
                chunk.copy_from_slice(pixel);
            }
            index += run * stride;
        } else {
            // A raw packet: `packet + 1` pixels, clamped at the end.
            let want = (packet as usize + 1) * stride;
            let take = want.min(count - index);
            let raw = body
                .get(cursor..cursor + take)
                .ok_or_else(|| ImgSeqError::new("the run-length data is truncated"))?;
            out[index..index + take].copy_from_slice(raw);
            cursor += take;
            index += take;
        }
    }
    Ok(out)
}

/// Moves the pixels the descriptor's two direction bits ask for.
///
/// Both the flip and the flop work on the **stored** bytes, so the stride is the
/// stored pixel size. Doing this after an expansion would stride by the wrong
/// amount and shear the picture.
fn fix_orientation(header: &Header, pixels: &mut [u8]) {
    let stride = header.raw_bytes_per_pixel;
    let width = header.width as usize;
    let height = header.height as usize;
    let row = width * stride;

    if header.descriptor & TOP_TO_BOTTOM == 0 && height > 1 {
        // Bottom-up, which is the format's default.
        let (top, bottom) = pixels.split_at_mut((height / 2) * row);
        for (up, down) in top
            .chunks_exact_mut(row)
            .zip(bottom.chunks_exact_mut(row).rev())
        {
            up.swap_with_slice(down);
        }
    }

    if header.descriptor & RIGHT_TO_LEFT != 0 && width > 1 {
        for line in pixels.chunks_exact_mut(row) {
            let (left, right) = line.split_at_mut((width / 2) * stride);
            for (a, b) in left
                .chunks_exact_mut(stride)
                .zip(right.chunks_exact_mut(stride).rev())
            {
                a.swap_with_slice(b);
            }
        }
    }
}

/// Expands the stored pixels into the decoded layout.
fn expand(header: &Header, stored: &[u8]) -> Result<Vec<u8>> {
    let pixels = header.width as usize * header.height as usize;
    let channels = header.channels();
    let mut out = vec![0u8; pixels * channels];

    if let Some(map) = &header.color_map {
        // The map is expanded exactly as it is stored, blue first;
        // `reverse_encoding` below is what turns it into r,g,b.
        let stride = header.raw_bytes_per_pixel;
        for (index, chunk) in stored
            .chunks_exact(stride)
            .zip(out.chunks_exact_mut(map.entry_size))
        {
            let value = if stride == 1 {
                u16::from(index[0])
            } else {
                u16::from_le_bytes([index[0], index[1]])
            };
            let entry = map.get(value as usize).ok_or_else(|| {
                ImgSeqError::new("the targa states a colour map index out of range")
            })?;
            chunk.copy_from_slice(entry);
        }
    } else if header.source == SourceColorType::Rgb5x1 {
        // Fifteen or sixteen bits is five bits a channel, in two bytes.
        for (source, target) in stored
            .as_chunks::<2>()
            .0
            .iter()
            .zip(out.as_chunks_mut::<3>().0.iter_mut())
        {
            let value = u16::from_le_bytes(*source);
            target[0] = bmp::expand(value & 0b1_1111, 5);
            target[1] = bmp::expand((value >> 5) & 0b1_1111, 5);
            target[2] = bmp::expand((value >> 10) & 0b1_1111, 5);
        }
    } else {
        let stride = header.raw_bytes_per_pixel;
        if stride != channels {
            return Err(ImgSeqError::new(
                "the stored pixel size does not match the decoded one",
            ));
        }
        let raw = stored
            .get(..out.len())
            .ok_or_else(|| ImgSeqError::new("the pixel data is truncated"))?;
        out.copy_from_slice(raw);
    }
    Ok(out)
}

/// Swaps the blue and red bytes of every colour pixel, in place.
///
/// Targa stores blue first. A grey or grey-with-alpha pixel has nothing to swap.
fn reverse_encoding(header: &Header, pixels: &mut [u8]) {
    let channels = header.channels();
    if matches!(header.color_type, ColorType::Rgb8 | ColorType::Rgba8) {
        for chunk in pixels.chunks_exact_mut(channels) {
            chunk.swap(0, 2);
        }
    }
}

/// What a targa states, when this module reads the file.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info(
    path: &Path,
    _apply_rotation: bool,
    route: Option<crate::formats::identify::Format>,
) -> Result<Option<ImageInfo>> {
    if !route.map_or_else(
        || owns(path),
        |saved| saved == crate::formats::identify::Format::Tga,
    ) {
        return Ok(None);
    }
    let data = image_head(path).map_err(|error| image_error("open", path, error))?;
    // A targa has no magic number to sniff, so a file whose header does not
    // parse is declined rather than refused, and something else may read it.
    let Ok(header) = header(&data) else {
        return Ok(None);
    };
    Ok(Some(ImageInfo {
        route: None,
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type: header.color_type(),
        original_color_type: header.source(),
        has_icc_profile: false,
        icc_profile: None,
        cicp: None,
        chroma_location: None,
        // A targa states an origin in the image descriptor rather than an
        // orientation, and the reader applies it, so the picture handed out is
        // already the way up the file means it.
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: header.format(),
    }))
}

/// A window the header of a targa is read through.
///
/// The header is fixed at eighteen bytes plus the image id the file states, so
/// this is far past any real one; a file whose header is larger is refused by the
/// parse rather than read further.
const HEAD_WINDOW: usize = 64 * 1024;

/// Opens `path` and reads its header, leaving the reader at the raster.
///
/// Answers `None` for every shape the stream does not cover, so the caller falls
/// back to [`decode`].
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or its header cannot be
/// parsed.
fn prepare(path: &Path) -> Result<Option<(Header, BufReader<File>)>> {
    let mut reader = BufReader::new(File::open(path).map_err(|e| image_error("open", path, e))?);
    let mut window = vec![0u8; HEAD_WINDOW];
    let read = reader
        .read(&mut window)
        .map_err(|e| image_error("open", path, e))?;
    window.truncate(read);
    let header = header(&window).map_err(|e| image_error("decode", path, e))?;
    if header.encoded
        || header.color_map.is_some()
        || header.raw_bytes_per_pixel != 3
        || header.color_type != ColorType::Rgb8
        || header.descriptor & TOP_TO_BOTTOM == 0
        || header.descriptor & RIGHT_TO_LEFT != 0
    {
        return Ok(None);
    }
    reader
        .seek(SeekFrom::Start(
            u64::try_from(header.data_offset).unwrap_or(0),
        ))
        .map_err(|e| image_error("open", path, e))?;
    Ok(Some((header, reader)))
}

/// A targa whose rows are already the frame's rows.
///
/// The shape is narrow on purpose: nothing run-length encoded, nothing to move
/// in either direction, and three bytes a pixel in the decoded layout, so the
/// file's row is the frame's row with blue and red exchanged and nothing else.
/// The corpus file is exactly this. Everything else -- a palette, a stored
/// sixteen bit word, a bottom-up image, an encoded one -- keeps the buffered
/// path, which is the rule that a stream is only answered with when it can fill
/// every frame of a call.
#[derive(Debug)]
pub struct Rows {
    path: std::path::PathBuf,
    /// What the header states, kept so a read does not parse it again.
    header: Option<Header>,
    /// The reader the preparation opened, positioned at the first byte of the
    /// raster. `None` is a stream that came from [`RowStream::duplicate`], which
    /// prepares itself on its way into a fill: duplicating cannot report the
    /// failure that opening and parsing can.
    raster: Option<BufReader<File>>,
}

impl Rows {
    /// The header and the reader positioned at the raster, prepared here when
    /// this stream is a duplicate that has not read anything yet.
    fn raster(&mut self) -> Result<(&Header, &mut BufReader<File>)> {
        if self.raster.is_none() {
            let Some((header, reader)) = prepare(&self.path)? else {
                return Err(ImgSeqError::new(format!(
                    "'{}' is no longer a targa this reader walks a row at a time",
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
        let row_bytes = header.width as usize * 3;
        let height = header.height as usize;
        let mut line = vec![0u8; row_bytes];
        for row in 0..height {
            reader
                .read_exact(&mut line)
                .map_err(|_| ImgSeqError::new(format!("the targa ends in row {row}")))?;
            sink.place_bgr8(&line, row)
                .ok_or_else(|| ImgSeqError::new("the frame holds no three colour planes"))?;
        }
        // A stream is read once. Letting the reader go here means a second fill
        // starts from the raster again rather than from where this one stopped.
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

/// Prepares a targa whose rows will be written straight into the frames.
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
/// Decodes a targa into one interleaved buffer.
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
    // An uncompressed targa's bytes are already what `expand` reads, so the copy
    // into a `stored` buffer is one the picture does not need: it was 0.70 ms of
    // 3.95 on a 3.24 MB file, and `expand` copies the same bytes again straight
    // after. The copy is only skipped when nothing has to move first, because the
    // directions act on the stored stride.
    let mut buffer = if !header.encoded
        && header.descriptor & (TOP_TO_BOTTOM | RIGHT_TO_LEFT) == TOP_TO_BOTTOM
    {
        let body = data
            .get(header.data_offset..)
            .ok_or_else(|| ImgSeqError::new("the targa is truncated"))?;
        expand(&header, body)
    } else {
        let mut stored =
            stored_pixels(&header, &data).map_err(|e| image_error("decode", &info.path, e))?;
        // The directions move stored bytes, so they run before any expansion.
        fix_orientation(&header, &mut stored);
        expand(&header, &stored)
    }
    .map_err(|e| image_error("decode", &info.path, e))?;
    reverse_encoding(&header, &mut buffer);
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

    /// The preparation leaves the reader at the raster, and the rows it reads are
    /// that raster and nothing else. `tga-rgb24-topdown.tga` is the one fixture
    /// whose shape the row sink takes.
    #[test]
    fn the_row_sink_reads_the_raster_a_row_at_a_time() {
        let path = fixture("tga-rgb24-topdown.tga");
        let (header, mut reader) = prepare(&path)
            .expect("the fixture is readable")
            .expect("a top-down 24 bit targa is a row sink");
        let mut line = vec![0u8; header.width as usize * 3];
        let mut rows = Vec::new();
        for _ in 0..header.height {
            reader.read_exact(&mut line).expect("a row of the raster");
            rows.extend_from_slice(&line);
        }
        let file = std::fs::read(&path).expect("the fixture");
        assert_eq!(rows, file[header.data_offset..], "the rows are the raster");
    }

    /// Every fixture, and the layout its header asks for. This is the table the
    /// port has to keep, and the fourth row is the one the plan calls out.
    #[test]
    fn the_fixtures_state_the_layouts_the_plan_names() {
        for (name, width, height, color_type, source) in [
            (
                "tga-rgb24.tga",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            // A thirty-two bit image with zero attribute bits is still r,g,b,a.
            (
                "tga-rgb32-attr0.tga",
                37,
                23,
                ColorType::Rgba8,
                SourceColorType::Rgba8,
            ),
            (
                "tga-rgb32-attr8.tga",
                37,
                23,
                ColorType::Rgba8,
                SourceColorType::Rgba8,
            ),
            (
                "tga-rgb16.tga",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb5x1,
            ),
            ("tga-gray8.tga", 37, 23, ColorType::L8, SourceColorType::L8),
            (
                "tga-gray8-alpha.tga",
                37,
                23,
                ColorType::La8,
                SourceColorType::La8,
            ),
            (
                "tga-mapped8.tga",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            (
                "tga-rgb24-rle.tga",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            (
                "tga-gray8-rle.tga",
                37,
                23,
                ColorType::L8,
                SourceColorType::L8,
            ),
            (
                "tga-mapped8-rle.tga",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            (
                "tga-rgb24-topdown.tga",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            (
                "tga-rgb24-rightleft.tga",
                37,
                23,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
        ] {
            let info = image_info(&fixture(name), true, None)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .unwrap_or_else(|| panic!("{name} is taken over"));
            assert_eq!((info.width, info.height), (width, height), "{name}");
            assert_eq!(info.color_type, color_type, "{name}");
            assert_eq!(info.original_color_type, source, "{name}");
            assert_eq!(info.transform, Transform::IDENTITY, "{name}");
        }
    }

    /// The rule that separates this format from the bitmap one: the same fourth
    /// byte is alpha here and is dropped there.
    #[test]
    fn a_thirty_two_bit_image_keeps_its_fourth_byte_without_stating_alpha() {
        let info = image_info(&fixture("tga-rgb32-attr0.tga"), true, None)
            .expect("read")
            .expect("taken over");
        assert_eq!(info.color_type, ColorType::Rgba8);
        let decoded = decode(&info).expect("the file decodes");
        let Pixels::Interleaved { buffer, .. } = decoded.pixels else {
            panic!("a targa hands out one interleaved buffer");
        };
        assert_eq!(buffer.len(), 37 * 23 * 4);
        // The picture states 64 on the left half and 255 on the right, so an
        // alpha that had been dropped and replaced would not show both.
        let alphas: std::collections::BTreeSet<u8> =
            buffer.as_chunks::<4>().0.iter().map(|px| px[3]).collect();
        assert!(alphas.contains(&64) && alphas.contains(&255), "{alphas:?}");
    }

    /// The run-length forms decode to the same picture as the raw ones, which is
    /// what makes the encoder and the decoder agree rather than merely run.
    #[test]
    fn the_run_length_forms_match_their_raw_equivalents() {
        for (raw, encoded) in [
            ("tga-rgb24.tga", "tga-rgb24-rle.tga"),
            ("tga-gray8.tga", "tga-gray8-rle.tga"),
            ("tga-mapped8.tga", "tga-mapped8-rle.tga"),
        ] {
            let plain = decode(
                &image_info(&fixture(raw), true, None)
                    .expect("read")
                    .expect("taken over"),
            )
            .expect("decodes");
            let packed = decode(
                &image_info(&fixture(encoded), true, None)
                    .expect("read")
                    .expect("taken over"),
            )
            .expect("decodes");
            let (Pixels::Interleaved { buffer: a, .. }, Pixels::Interleaved { buffer: b, .. }) =
                (plain.pixels, packed.pixels)
            else {
                panic!("a targa hands out one interleaved buffer");
            };
            assert_eq!(a, b, "{raw} against {encoded}");
        }
    }

    /// The two descriptor directions are applied, so the picture is the way up
    /// the file means it rather than the way it is stored.
    #[test]
    fn the_descriptor_directions_are_applied() {
        let plain = decode(
            &image_info(&fixture("tga-rgb24.tga"), true, None)
                .expect("read")
                .expect("taken over"),
        )
        .expect("decodes");
        let Pixels::Interleaved { buffer: a, .. } = plain.pixels else {
            panic!("interleaved");
        };
        for name in ["tga-rgb24-topdown.tga", "tga-rgb24-rightleft.tga"] {
            // Stored the other way round and read back the same way up.
            let turned = decode(
                &image_info(&fixture(name), true, None)
                    .expect("read")
                    .expect("taken over"),
            )
            .expect("decodes");
            let Pixels::Interleaved { buffer: b, .. } = turned.pixels else {
                panic!("interleaved");
            };
            assert_eq!(a.len(), b.len(), "{name}");
            // The top-left pixel of each agrees, which it would not if the
            // direction had been ignored and the storage order handed out.
            assert_eq!(&a[..3], &b[..3], "{name}");
        }
    }

    /// A file that is not a targa is declined; one that names an image type this
    /// reader does not implement is refused by name.
    #[test]
    fn an_unported_image_type_is_refused_by_name() {
        let mut zero = vec![0u8; HEADER];
        zero[2] = 0; // No image data.
        zero[12] = 4;
        zero[14] = 4;
        zero[16] = 24;
        let error = header(&zero).expect_err("type zero is refused");
        assert!(error.to_string().contains("image type 0"), "{error}");

        assert!(!owns(Path::new("a.png")));
        assert!(owns(Path::new("a.tga")));
        assert!(owns(Path::new("a.TGA")));
        assert!(
            image_info(&fixture("cicp-rgb8.png"), true, None)
                .expect("a png is not ours")
                .is_none()
        );
    }
}
