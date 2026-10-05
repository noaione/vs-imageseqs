//! Container reads for a png that the `image` decoder has no accessor for, and
//! the probe that reads a png without going through that decoder at all.
//!
//! `image` hands out the pixels of a png, its size and its ICC profile, but not
//! the `cICP` chunk, which is where a png states its own colour in the same
//! H.273 code points the other containers use. The chunk is a handful of bytes
//! in the metadata at the front of the file, so it is read from the file itself
//! rather than by decoding the picture; see
//! `docs/improvements/08-color-metadata.md`.
//!
//! This module reads the `cICP` chunk of every png, it walks the rows of a png
//! the `image` decoder would otherwise materialise whole (see [`stream`]), and
//! it answers what a probe asks about a png ([`image_info`]) from the `png`
//! crate directly. It leaves everything it will not walk to the `image` png
//! decoder, which is the same crate underneath with the same transformation
//! set, so the two cannot disagree about the picture they describe.

use std::{
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::layout::ColorType;

use crate::{
    color::Cicp,
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, RowSink, RowStream, image_error},
    error::{ImgSeqError, Result},
    exif::orientation_of,
    formats::identify::{self, Format},
    layout::{Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// The bytes every png starts with, which is also how a file that is not one is
/// declined without reading further.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Bytes of the checksum that follows every chunk payload.
const CRC_LENGTH: usize = 4;

/// How far into a file the chunks are read. Every writer that writes a `cICP`
/// writes it among the chunks that come before the image data, and this is far
/// past the metadata any real file holds.
const CHUNK_LIMIT: usize = 1024 * 1024;

/// Whether `path` names a png.
///
/// The extension is what selects this module, the same way it selects the
/// still-image `cICp` reader and the animation adapter above it.
#[must_use]
pub fn owns(path: &Path) -> bool {
    // Content first: a file whose bytes say it is something else is that
    // something else however it is named, and the extension is the hint a
    // format with no signature of its own has to fall back on. See
    // [`identify::owns`](crate::formats::identify::owns).
    crate::formats::identify::owns(crate::formats::identify::Format::Png, path)
}

/// The `cICP` chunk of a file this module has already established is a png,
/// which is what keeps [`image_info`] from asking the ownership question a
/// second time and with a second read.
fn cicp_of_file(path: &Path) -> Option<Cicp> {
    cicp_from(File::open(path).ok()?)
}

/// Reads chunks until the image data starts, and answers what a `cICP` among
/// them states.
///
/// A `cICP` written after the image data is not what a png states about its
/// samples, so the walk stops there rather than reading the whole file. A
/// truncated or malformed chunk ends it too, which leaves that file with the
/// properties its decoded color type alone describes.
fn cicp_from(mut file: impl Read) -> Option<Cicp> {
    let mut signature = [0; 8];
    file.read_exact(&mut signature).ok()?;
    if signature != SIGNATURE {
        return None;
    }
    let mut read = signature.len();
    loop {
        let mut header = [0; 8];
        file.read_exact(&mut header).ok()?;
        let length = usize::try_from(u32::from_be_bytes(header[..4].try_into().ok()?)).ok()?;
        let kind = &header[4..];
        if kind == b"IDAT" || kind == b"IEND" {
            return None;
        }
        read = read.checked_add(header.len() + length + CRC_LENGTH)?;
        if read > CHUNK_LIMIT {
            return None;
        }
        let mut payload = vec![0; length];
        file.read_exact(&mut payload).ok()?;
        file.read_exact(&mut [0; CRC_LENGTH]).ok()?;
        if kind == b"cICP" {
            return cicp_chunk(&payload);
        }
    }
}

/// The colour description a `cICP` chunk payload states.
///
/// The payload is four bytes: the primaries, transfer and matrix code points,
/// then the video full range flag, each of them the parameter of the same name
/// in h.273. The code points are single bytes here, unlike the 16 bit fields an
/// `nclx` box holds, so a file that states one this module has no property for
/// states it as it is: the mapping in [`Cicp`] is what turns a code point into a
/// property, or leaves it unset.
///
/// The flag is a whole byte that conforming files write as `0` or `1` (the png
/// specification's own examples are `09 12 00 01` and `01 01 00 00`), so it is
/// read as a flag rather than as the top bit of a bit field the way an `nclx`
/// box stores it. Any nonzero byte is the full range, which reads a file that
/// carried the `nclx` convention into this chunk as the writer meant it.
fn cicp_chunk(payload: &[u8]) -> Option<Cicp> {
    let [primaries, transfer, matrix, full_range, ..] = payload else {
        return None;
    };
    Some(Cicp {
        primaries: *primaries,
        transfer: *transfer,
        matrix: *matrix,
        full_range: *full_range != 0,
    })
}

/// Probes `path` without decoding its picture, from the png crate itself.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or its header cannot be
/// parsed.
pub fn image_info(
    path: &Path,
    apply_rotation: bool,
    route: Option<crate::formats::identify::Format>,
) -> Result<Option<ImageInfo>> {
    // The content decides, not the name. This asked the extension, which is the
    // one thing plan 34 is about: the decode next door answered from the bytes, so
    // a renamed page was *described* by one reader and *decoded* by another, and
    // that only ever worked while the generic decoder could describe it too.
    if !route.map_or_else(
        || owns(path),
        |saved| saved == crate::formats::identify::Format::Png,
    ) {
        return Ok(None);
    }
    let file = File::open(path).map_err(|error| image_error("open", path, error))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    // The one transformation `image`'s own png decoder sets, so the layout this
    // probe reports is the layout that decoder produces. It is what widens a
    // palette index to r,g,b, turns a `tRNS` into an alpha channel, and takes a
    // one, two or four bit grey page up to eight bits.
    decoder.set_transformations(png::Transformations::EXPAND);
    let reader = decoder
        .read_info()
        .map_err(|error| image_error("create decoder for", path, error))?;
    let header = reader.info();
    let color_type = expanded_color_type(reader.output_color_type(), path)?;
    let orientation = header
        .exif_metadata
        .as_deref()
        .and_then(orientation_of)
        .unwrap_or(Orientation::NoTransforms);

    Ok(Some(ImageInfo {
        route: None,
        subimage: None,
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type,
        // The `image` png decoder does not override this, so the label a png
        // reports is the name of the layout its samples decode to. That is
        // why a palette page reads `Rgb8` rather than naming its indices.
        original_color_type: SourceColorType::from(color_type),
        has_icc_profile: header.icc_profile.is_some(),
        icc_profile: header
            .icc_profile
            .as_ref()
            .map(|profile| std::sync::Arc::from(profile.as_ref())),
        cicp: cicp_of_file(path),
        // A png states no chroma sample position: it is grey or r,g,b.
        chroma_location: None,
        orientation,
        transform: if apply_rotation {
            Transform::from_orientation(orientation)
        } else {
            Transform::IDENTITY
        },
        format: PixelFormat::from_color_type(color_type).ok_or_else(|| {
            ImgSeqError::new(format!(
                "image '{}' has a colour type this plugin has no format for",
                path.display()
            ))
        })?,
    }))
}

/// The layout the png crate's expanded output is written as.
///
/// The eight arms are the eight layouts `EXPAND` can produce, and they are the
/// same eight and the same names `image`'s png decoder maps them to. The narrow
/// depths are absent because `EXPAND` has already widened them, so an arm for
/// one would be unreachable rather than a fallback.
fn expanded_color_type(output: (png::ColorType, png::BitDepth), path: &Path) -> Result<ColorType> {
    Ok(match output {
        (png::ColorType::Grayscale, png::BitDepth::Eight) => ColorType::L8,
        (png::ColorType::Grayscale, png::BitDepth::Sixteen) => ColorType::L16,
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => ColorType::La8,
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Sixteen) => ColorType::La16,
        (png::ColorType::Rgb, png::BitDepth::Eight) => ColorType::Rgb8,
        (png::ColorType::Rgb, png::BitDepth::Sixteen) => ColorType::Rgb16,
        (png::ColorType::Rgba, png::BitDepth::Eight) => ColorType::Rgba8,
        (png::ColorType::Rgba, png::BitDepth::Sixteen) => ColorType::Rgba16,
        (color, depth) => {
            return Err(image_error(
                "create decoder for",
                path,
                format!("a {color:?} page of {depth:?} bits is not a layout this plugin reads"),
            ));
        }
    })
}

// ---------------------------------------------------------------------------
// the row walk
// ---------------------------------------------------------------------------

/// Channels, sample width and colour channels of one colour type.
///
/// The colour channels come first and the alpha channel, when there is one, is
/// the last: `Transformations::EXPAND` turns a palette or a `tRNS` into exactly
/// these six layouts and nothing else reaches here.
#[derive(Clone, Copy, Debug)]
struct Layout {
    /// Samples per pixel, the alpha channel included.
    channels: usize,
    /// Bytes per sample, which is one or two.
    sample_bytes: usize,
    /// Leading samples that are colour rather than alpha.
    colour_channels: usize,
}

impl Layout {
    /// The layout `color_type` decodes to, or `None` for a type this walk does
    /// not place.
    fn of(color_type: ColorType) -> Option<Self> {
        let (channels, sample_bytes, colour_channels) = match color_type {
            ColorType::L8 => (1, 1, 1),
            ColorType::L16 => (1, 2, 1),
            ColorType::La8 => (2, 1, 1),
            ColorType::La16 => (2, 2, 1),
            ColorType::Rgb8 => (3, 1, 3),
            ColorType::Rgb16 => (3, 2, 3),
            ColorType::Rgba8 => (4, 1, 3),
            ColorType::Rgba16 => (4, 2, 3),
            _ => return None,
        };
        Some(Self {
            channels,
            sample_bytes,
            colour_channels,
        })
    }
}

/// Where the samples of one row come from, and what the walk does with them.
///
/// The colour type `png` has to produce is part of this rather than a separate
/// check, because the two sources ask the decoder for two different things.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Source {
    /// The decoder expanded the row into the colour type the frame is, which
    /// is what `image` asks for too, so the walk only has to place it.
    Expanded((png::ColorType, png::BitDepth)),
    /// The row holds palette indices and the table is expanded by the walk.
    ///
    /// `EXPAND` would turn each index into three bytes in a scratch buffer
    /// that the walk reads back; a lookup per pixel into the table is one pass
    /// over the indices instead of two passes over three times their size. The
    /// measurement is in `docs/improvements/22-png-decode-path.md`.
    Indices(Vec<u8>),
}

/// What the `png` crate has to report for this walk to be the same picture
/// `image` would hand over, and what a row of it is.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Walkable {
    width: u32,
    height: u32,
    source: Source,
}

/// An eight bit index can name any of these, so the table is widened to all of
/// them.
const PALETTE_BYTES: usize = 256 * 3;

/// The palette widened to every entry an eight bit index can name.
///
/// A conforming file never names an entry past its own table. One that does is
/// still not a file a release build may abort on, and an index with no check
/// would read past the table, so the last entry is repeated to the end of the
/// range instead: a table lookup is then bounded for every possible index and
/// costs nothing to make so.
fn padded_palette(palette: &[u8]) -> Vec<u8> {
    let last = palette.len().saturating_sub(3);
    let entry = [
        palette.get(last).copied().unwrap_or(0),
        palette.get(last + 1).copied().unwrap_or(0),
        palette.get(last + 2).copied().unwrap_or(0),
    ];
    let mut out = Vec::with_capacity(PALETTE_BYTES);
    out.extend_from_slice(palette);
    while out.len() < PALETTE_BYTES {
        out.extend_from_slice(&entry);
    }
    out.truncate(PALETTE_BYTES);
    out
}

/// The colour type and depth `png` has to produce for `color_type`, which is
/// what `image` asks it for: [`png::Transformations::EXPAND`] is the only
/// transformation either of them sets.
fn expanded_type(color_type: ColorType) -> Option<(png::ColorType, png::BitDepth)> {
    Some(match color_type {
        ColorType::L8 => (png::ColorType::Grayscale, png::BitDepth::Eight),
        ColorType::L16 => (png::ColorType::Grayscale, png::BitDepth::Sixteen),
        ColorType::La8 => (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight),
        ColorType::La16 => (png::ColorType::GrayscaleAlpha, png::BitDepth::Sixteen),
        ColorType::Rgb8 => (png::ColorType::Rgb, png::BitDepth::Eight),
        ColorType::Rgb16 => (png::ColorType::Rgb, png::BitDepth::Sixteen),
        ColorType::Rgba8 => (png::ColorType::Rgba, png::BitDepth::Eight),
        ColorType::Rgba16 => (png::ColorType::Rgba, png::BitDepth::Sixteen),
        _ => return None,
    })
}

/// The header of a png this module will walk, or `None` for one it will not.
///
/// The reads here are the metadata at the front of the file and no image data,
/// so this is a header parse and not a decode. The reader it opens comes back
/// with the answer, because it is already positioned where the rows start and
/// [`Rows::fill`] would otherwise open the file a second time and parse the
/// header again to reach the same place.
fn walkable(path: &Path, info: &ImageInfo) -> Option<(Walkable, png::Reader<BufReader<File>>)> {
    let file = File::open(path).ok()?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    // The same transformation `image`'s own png decoder sets, so the samples
    // this walk places are the samples that decoder would have produced.
    decoder.set_transformations(png::Transformations::EXPAND);
    let reader = decoder.read_info().ok()?;
    let header = reader.info();
    // An interlaced png arrives one Adam7 pass at a time rather than one
    // picture row at a time, and an animated one holds more than the single
    // picture a still decode means. Both go back to `image`.
    if header.interlaced || header.animation_control.is_some() {
        return None;
    }
    // A palette page the frame wants as r,g,b is expanded here rather than by
    // the decoder. The probe reports the colour type `EXPAND` produces, which
    // for such a file is `Rgb8`, so this is the only place that knows the file
    // holds indices at all; `fill` checks that `IDENTITY` really does hand
    // them over before it reads a row.
    if header.color_type == png::ColorType::Indexed
        && header.bit_depth == png::BitDepth::Eight
        && header.trns.is_none()
        && info.color_type == ColorType::Rgb8
        && let Some(palette) = header.palette.as_ref()
    {
        // This page is expanded here rather than by the decoder, so the reader
        // `fill` needs is one opened with `IDENTITY` -- the reader above is the
        // `EXPAND` one this decision was made with, and it would hand over rgb,
        // which `fill` refuses as a file that changed after probing. So a palette
        // page opens the file twice and every other page once.
        let file = File::open(path).ok()?;
        let mut decoder = png::Decoder::new(BufReader::new(file));
        decoder.set_transformations(png::Transformations::IDENTITY);
        let indices = decoder.read_info().ok()?;
        return Some((
            Walkable {
                width: header.width,
                height: header.height,
                source: Source::Indices(padded_palette(palette)),
            },
            indices,
        ));
    }
    // Anything else has to arrive as the colour type the frame is, or the
    // frame the clip sized from the probe is not the frame these rows fit.
    let output = reader.output_color_type();
    if output != expanded_type(info.color_type)? {
        return None;
    }
    Some((
        Walkable {
            width: header.width,
            height: header.height,
            source: Source::Expanded(output),
        },
        reader,
    ))
}

/// A decode of a png that hands every row to the frame it belongs in.
///
/// `image` decodes a png into one buffer sized for the whole picture and the
/// plugin then copies that buffer into the frame, which is two passes over
/// every byte of it. A row walk is one: the samples the decoder produces are
/// written where the frame keeps them. That is worth about a quarter of the
/// decode side on a palette page; see
/// `docs/improvements/22-png-decode-path.md`.
///
/// A png carries its alpha channel in the one stream the colour comes from, so
/// the demand of the call changes nothing here: a colour-only read reads the
/// alpha samples and drops them.
struct Rows {
    path: PathBuf,
    width: u32,
    height: u32,
    color_type: ColorType,
    /// Whether the rows carry an alpha channel, which is what says whether the
    /// alpha clip's plane has to be made opaque before the walk runs.
    has_alpha: bool,
    /// Where a row's samples come from, which is what the walk does with it.
    source: Source,
    /// The reader the walk opened, already positioned on the rows. It is taken by
    /// the first fill, and `None` on a stream that came from `duplicate`, which
    /// opens its own on the way into a fill.
    reader: Option<png::Reader<BufReader<File>>>,
}

/// `png::Reader` is not `Debug`, so a row stream prints what it was told rather
/// than the crate's internals, and says whether it still holds the reader the
/// walk opened.
impl std::fmt::Debug for Rows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("png::Rows")
            .field("path", &self.path)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("color_type", &self.color_type)
            .field("has_alpha", &self.has_alpha)
            .field("source", &self.source)
            .field("holds_reader", &self.reader.is_some())
            .finish()
    }
}

/// The decode a png this module can walk is answered with, or `None` for one
/// that goes back to `image`.
///
/// Everything refused here is refused for the whole file, not for one frame, so
/// a sequence of them decides once per path.
#[inline(never)]
pub fn stream(info: &ImageInfo) -> Option<Pixels> {
    if !identify::route_agrees(info.route, Format::Png, &info.path)
        || info.transform != Transform::IDENTITY
    {
        return None;
    }
    // Only the four formats a png's own samples land in. A format that names a
    // narrower depth than its word is not one of them, because a png states no
    // depth for the writer to move the samples down to.
    if !matches!(
        info.format,
        PixelFormat::Gray8 | PixelFormat::Gray16 | PixelFormat::Rgb8 | PixelFormat::Rgb16
    ) {
        return None;
    }
    // The colour type has to be one this walk has a layout for, which is the
    // same set `expanded_type` answers for.
    let layout = Layout::of(info.color_type)?;
    let (header, reader) = walkable(&info.path, info)?;
    // The probe and this walk have to be describing the same picture, or the
    // frame the clip sized from the probe is not the frame these rows fit. The
    // colour type each source needs is checked by `walkable`, which is the only
    // place that has both answers.
    if (header.width, header.height) != (info.width, info.height) {
        return None;
    }
    Some(Pixels::Stream(Box::new(Rows {
        path: info.path.clone(),
        width: info.width,
        height: info.height,
        color_type: info.color_type,
        has_alpha: layout.channels != layout.colour_channels,
        source: header.source,
        reader: Some(reader),
    })))
}

/// Decodes an interlaced png whole, which is the one shape the row walk refuses.
///
/// Adam7 hands the file over one *pass* at a time rather than one picture row at a
/// time -- [`png::Reader::next_row`] is documented as "discarding `InterlaceInfo`"
/// and does exactly that -- so a pass row cannot be placed in the frame as it is
/// read, and [`walkable`] declines the file. The crate's own whole-frame read
/// expands the passes, and it is the same call the `image` png decoder makes with
/// the same `EXPAND` transformation, so the samples here are the samples that
/// decoder produced; only the buffer is this module's.
///
/// Returns `None` for a png this is not about, which leaves it either to [`stream`]
/// or to the generic decoder.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is interlaced and cannot be decoded.
#[inline(never)]
pub fn decode(info: &ImageInfo) -> Result<Option<DecodedImage>> {
    if !identify::route_agrees(info.route, Format::Png, &info.path) {
        return Ok(None);
    }
    let open_started = Instant::now();
    let file = File::open(&info.path).map_err(|error| image_error("open", &info.path, error))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder
        .read_info()
        .map_err(|error| image_error("create decoder for", &info.path, error))?;
    // The walk places a row in the frame as it reads it, so it needs the picture
    // the file holds to be the picture the frame is: no Adam7 passes, nothing to
    // rearrange afterwards, and no animation control chunk, because the walk reads
    // one picture and an animated container states more than one. A file with any
    // of those three is this path's, and this is the same whole-frame read the
    // generic decoder made for it -- the buffer below carries `info.transform`,
    // which is the rearrangement that buffer got. Everything the walk refuses for
    // any *other* reason is still not this path's and is left alone.
    if !reader.info().interlaced
        && info.transform == Transform::IDENTITY
        && reader.info().animation_control.is_none()
    {
        return Ok(None);
    }
    let color_type = expanded_color_type(reader.output_color_type(), &info.path)?;
    // The probe and this read have to be describing the same picture, or the frame
    // the clip sized from the probe is not the frame these samples fit. A
    // disagreement is left to the generic path, which re-reads the file and
    // reports it.
    if color_type != info.color_type {
        return Ok(None);
    }
    let open = open_started.elapsed();
    let size = reader.output_buffer_size().ok_or_else(|| {
        ImgSeqError::new(format!(
            "image '{}' states a picture too large to hold",
            info.path.display()
        ))
    })?;
    let mut buffer = vec![0u8; size];
    let read_started = Instant::now();
    reader
        .next_frame(&mut buffer)
        .map_err(|error| image_error("decode", &info.path, error))?;
    let read = read_started.elapsed();
    Ok(Some(DecodedImage {
        width: info.width,
        height: info.height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Interleaved { color_type, buffer },
        timings: DecodeTimings {
            open,
            metadata: Duration::ZERO,
            buffer: Duration::ZERO,
            read,
        },
    }))
}

/// Places one decoded row into the planes of the frames of the call.
struct Placer<'a> {
    layout: Layout,
    sink: RowSink<'a>,
    /// Bytes one row of the decoder's own output holds, before placement.
    source_row_bytes: usize,
}

impl<'a> Placer<'a> {
    /// Checks the frames against the colour type before any row is written.
    ///
    /// The frame is built from the probe's answer and this walk from the
    /// header's, so the two are checked against each other here rather than
    /// trusted: a mismatch is a decode that refuses instead of a frame with a
    /// row in the wrong place.
    fn new(
        layout: Layout,
        sink: RowSink<'a>,
        width: u32,
        source_row_bytes: usize,
        path: &Path,
    ) -> Result<Self> {
        let row_bytes = usize::try_from(width)
            .map_err(|_| ImgSeqError::new("image width does not fit this platform"))?
            .saturating_mul(layout.sample_bytes);
        if sink.colour.len() != layout.colour_channels {
            return Err(ImgSeqError::new(format!(
                "png '{}' needs {} colour planes, but the frame has {}",
                path.display(),
                layout.colour_channels,
                sink.colour.len()
            )));
        }
        // The placer writes one sample per pixel of every row, so a plane
        // whose row is not that wide would be walked past its own end.
        for plane in sink.colour.iter().chain(sink.alpha.iter().flatten()) {
            if plane.row_bytes != row_bytes {
                return Err(ImgSeqError::new(format!(
                    "png '{}' decodes {row_bytes} byte rows, but a plane holds {}",
                    path.display(),
                    plane.row_bytes
                )));
            }
        }
        Ok(Self {
            layout,
            sink,
            source_row_bytes,
        })
    }

    /// Refuses a row that is not the width the planes were checked against.
    ///
    /// A short row would leave the tail of every plane holding whatever the
    /// frame arrived with, which is the same class of mistake the alpha plane
    /// needs its own fill for.
    fn check_row(&self, data: &[u8], path: &Path) -> Result<()> {
        if data.len() != self.source_row_bytes {
            return Err(ImgSeqError::new(format!(
                "png '{}' handed over a {} byte row, but its rows are {}",
                path.display(),
                data.len(),
                self.source_row_bytes
            )));
        }
        Ok(())
    }

    /// Writes one decoded row into the rows of the planes it holds.
    fn place(&mut self, data: &[u8], row: usize) -> Result<()> {
        // One eight bit channel is the row itself, and the plane it goes into
        // is the whole of it, so this is one copy.
        if self.layout.channels == 1
            && self.layout.sample_bytes == 1
            && let Some(plane) = self.sink.colour.first_mut()
        {
            plane.row(row).copy_from_slice(data);
            return Ok(());
        }
        // Three eight bit colour channels are one walk over the row: each of
        // the three planes is a gather with a stride of three, so walking the
        // row once per plane reads it three times and multiplies for every
        // byte it writes. This is the shape a palette page arrives in, which is
        // most of a manga set.
        //
        // A row whose four channels include alpha is not this shape: its
        // fourth byte would be read as the next pixel's red.
        if self.layout.sample_bytes == 1
            && self.layout.channels == 3
            && self.layout.colour_channels == 3
            && self.sink.colour.len() == 3
        {
            return self.place_rgb(data, row);
        }
        self.place_by_channel(data, row)
    }

    /// Writes a three channel row into the three colour planes, reading it
    /// once.
    fn place_rgb(&mut self, data: &[u8], row: usize) -> Result<()> {
        self.sink
            .place_rgb8(data, row)
            .ok_or_else(missing_colour_planes)
    }

    /// Writes one row of palette indices into the three colour planes.
    ///
    /// `EXPAND` would have turned each index into three bytes in the decoder's
    /// scratch buffer for this walk to read back; a lookup into the table is
    /// one pass over the indices instead, and the table is small enough to sit
    /// in cache. The measurement is in
    /// `docs/improvements/22-png-decode-path.md`.
    ///
    /// The table is padded to every entry an index can name, so the lookup
    /// needs no bound of its own.
    fn place_indices(&mut self, data: &[u8], row: usize, palette: &[u8]) -> Result<()> {
        let (red, rest) = self
            .sink
            .colour
            .split_first_mut()
            .ok_or_else(missing_colour_planes)?;
        let (green, rest) = rest.split_first_mut().ok_or_else(missing_colour_planes)?;
        let (blue, _) = rest.split_first_mut().ok_or_else(missing_colour_planes)?;
        let planes = red
            .row(row)
            .iter_mut()
            .zip(green.row(row).iter_mut())
            .zip(blue.row(row).iter_mut());
        for (index, ((red_byte, green_byte), blue_byte)) in data.iter().zip(planes) {
            let entry = usize::from(*index) * 3;
            *red_byte = palette[entry];
            *green_byte = palette[entry + 1];
            *blue_byte = palette[entry + 2];
        }
        Ok(())
    }

    /// Writes `data` one channel at a time, into the plane that channel
    /// belongs in.
    ///
    /// This is the general shape: one walk over the row for each plane, which
    /// is a gather when the planes are interleaved and a contiguous copy when
    /// they are not. [`Self::place_rgb`] is what the three channel eight bit
    /// case uses instead.
    fn place_by_channel(&mut self, data: &[u8], row: usize) -> Result<()> {
        let channels = self.layout.channels;
        let sample_bytes = self.layout.sample_bytes;
        let colour_channels = self.layout.colour_channels;
        for channel in 0..channels {
            let target = if channel < colour_channels {
                self.sink.colour.get_mut(channel)
            } else {
                self.sink
                    .alpha
                    .as_mut()
                    .and_then(|planes| planes.get_mut(channel - colour_channels))
            };
            // A channel the call hands out nowhere is read and dropped, which
            // is what a colour-only read of a file with alpha does.
            let Some(target) = target else {
                continue;
            };
            let out = target.row(row);
            if sample_bytes == 1 {
                for (index, byte) in out.iter_mut().enumerate() {
                    *byte = data[index * channels + channel];
                }
            } else {
                for (index, sample) in out.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    let at = (index * channels + channel) * 2;
                    // A png stores a sixteen bit sample big endian and the
                    // frame keeps the byte order of the machine, which is the
                    // swap `image` makes over the whole buffer.
                    sample.copy_from_slice(&[data[at + 1], data[at]]);
                }
            }
        }
        Ok(())
    }
}

/// The error a frame with fewer colour planes than the colour type needs is
/// refused with.
fn missing_colour_planes() -> ImgSeqError {
    ImgSeqError::new("the colour frame has fewer planes than the png needs")
}

impl RowStream for Rows {
    fn fill(&mut self, sink: RowSink<'_>) -> Result<DecodeTimings> {
        let open_started = Instant::now();
        // The walk that answered `stream` opened this file and is already holding the
        // reader the rows come out of, so the common case opens nothing and parses no
        // header a second time. A stream that came from `duplicate` has no reader of
        // its own -- a reader is a position, and duplicating cannot share one -- so it
        // opens the file here, the way the netpbm reader does.
        let mut reader = match self.reader.take() {
            Some(reader) => reader,
            None => {
                let file = File::open(&self.path)
                    .map_err(|error| image_error("open", &self.path, error))?;
                let mut decoder = png::Decoder::new(BufReader::new(file));
                // A palette page is expanded here, so it is read as the indices it holds;
                // everything else arrives as the colour type the frame is.
                decoder.set_transformations(match self.source {
                    Source::Expanded(_) => png::Transformations::EXPAND,
                    Source::Indices(_) => png::Transformations::IDENTITY,
                });
                decoder
                    .read_info()
                    .map_err(|error| image_error("decode", &self.path, error))?
            }
        };
        let open = open_started.elapsed();

        // Whether the reader came from the walk or was opened here, these reads
        // are the same header rather than a decision; a file that changed
        // between the walk and this fill is refused rather than written into a
        // frame it does not fit.
        let metadata_started = Instant::now();
        let header = reader.info();
        let geometry = (
            header.width,
            header.height,
            header.interlaced,
            header.animation_control.is_some(),
        );
        let output = reader.output_color_type();
        let metadata = metadata_started.elapsed();
        let expected = match &self.source {
            Source::Expanded(expected) => *expected,
            // `IDENTITY` hands a palette page's own indices over, and a `tRNS`
            // is what `walkable` refused, so this is the pair to see.
            Source::Indices(_) => (png::ColorType::Indexed, png::BitDepth::Eight),
        };
        if geometry != (self.width, self.height, false, false) || output != expected {
            return Err(ImgSeqError::new(format!(
                "png '{}' changed after probing",
                self.path.display()
            )));
        }

        let layout =
            Layout::of(self.color_type).expect("a colour type this walk places has a layout");
        // A row is the frame's own samples when the decoder expanded it, and one
        // index per pixel when the walk does.
        let source_row_bytes = match &self.source {
            Source::Indices(_) => usize::try_from(self.width).unwrap_or(usize::MAX),
            Source::Expanded(_) => usize::try_from(self.width)
                .unwrap_or(usize::MAX)
                .saturating_mul(layout.channels)
                .saturating_mul(layout.sample_bytes),
        };
        let mut placer = Placer::new(layout, sink, self.width, source_row_bytes, &self.path)?;

        let read_started = Instant::now();
        let mut row = 0usize;
        while let Some(line) = reader
            .next_row()
            .map_err(|error| image_error("decode", &self.path, error))?
        {
            placer.check_row(line.data(), &self.path)?;
            match &self.source {
                Source::Indices(palette) => placer.place_indices(line.data(), row, palette)?,
                Source::Expanded(_) => placer.place(line.data(), row)?,
            }
            row += 1;
        }
        let read = read_started.elapsed();
        if row != usize::try_from(self.height).unwrap_or(usize::MAX) {
            return Err(ImgSeqError::new(format!(
                "png '{}' holds {row} rows, but its header states {}",
                self.path.display(),
                self.height
            )));
        }

        // A stream is read once. Letting the reader go means a second fill starts
        // from the header again rather than from where this one stopped.
        self.reader = None;
        // There is no buffer, so the buffer stage is nothing; the read is the
        // decode and the placement together, because they are one pass.
        Ok(DecodeTimings {
            open,
            metadata,
            buffer: Duration::ZERO,
            read,
        })
    }

    fn has_alpha(&self) -> bool {
        self.has_alpha
    }

    fn duplicate(&self) -> Box<dyn RowStream> {
        Box::new(Self {
            path: self.path.clone(),
            width: self.width,
            height: self.height,
            color_type: self.color_type,
            has_alpha: self.has_alpha,
            source: self.source.clone(),
            // A reader is a position in one file, and two streams cannot share one,
            // so the duplicate opens its own when it fills.
            reader: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A png signature, then `chunks`, each `(kind, payload)`.
    fn file(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut file = SIGNATURE.to_vec();
        for (kind, payload) in chunks {
            file.extend_from_slice(
                &u32::try_from(payload.len())
                    .expect("chunk size")
                    .to_be_bytes(),
            );
            file.extend_from_slice(*kind);
            file.extend_from_slice(payload);
            file.extend_from_slice(&[0; CRC_LENGTH]);
        }
        file
    }

    #[test]
    fn a_cicp_chunk_states_the_colour() {
        // The png specification's own example of a bt.2100 hlg full range
        // image, whose matrix is 0 because rgb is the only colour model a png
        // has.
        let bytes = file(&[(b"cICP", &[9, 18, 0, 1])]);
        let cicp = cicp_from(&bytes[..]).expect("a stated colour");
        assert_eq!(cicp.primaries, 9);
        assert_eq!(cicp.transfer, 18);
        assert_eq!(cicp.matrix, 0);
        assert!(cicp.full_range);
    }

    #[test]
    fn the_full_range_flag_is_a_byte_not_a_bit() {
        for (flags, full_range) in [(0u8, false), (1, true)] {
            let bytes = file(&[(b"cICP", &[1, 1, 0, flags])]);
            assert_eq!(
                cicp_from(&bytes[..]).expect("a stated colour").full_range,
                full_range,
                "flag {flags}"
            );
        }
        // A writer that carried the `nclx` convention into this chunk set the
        // top bit instead, and meant the same thing by it.
        let bytes = file(&[(b"cICP", &[9, 10, 0, 0x80])]);
        assert!(cicp_from(&bytes[..]).expect("a stated colour").full_range);
    }

    #[test]
    fn chunks_before_the_cicp_chunk_are_skipped() {
        let bytes = file(&[
            (b"IHDR", &[0; 13]),
            (b"sRGB", &[0]),
            (b"cICP", &[1, 13, 0, 1]),
        ]);
        assert!(cicp_from(&bytes[..]).is_some());
    }

    #[test]
    fn a_png_without_a_cicp_chunk_states_nothing() {
        let bytes = file(&[(b"IHDR", &[0; 13]), (b"IDAT", &[0; 4])]);
        assert!(cicp_from(&bytes[..]).is_none());
    }

    #[test]
    fn a_cicp_chunk_after_the_image_data_states_nothing() {
        let bytes = file(&[(b"IHDR", &[0; 13]), (b"IDAT", &[0; 4])]);
        let mut with_trailing = bytes.clone();
        with_trailing.extend_from_slice(&file(&[(b"cICP", &[1, 13, 0, 1])])[8..]);
        assert!(cicp_from(&with_trailing[..]).is_none());
    }

    #[test]
    fn a_file_that_is_not_a_png_states_nothing() {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&[0; 64]);
        assert!(cicp_from(&bytes[..]).is_none());
    }

    #[test]
    fn a_short_cicp_chunk_states_nothing() {
        let bytes = file(&[(b"cICP", &[1, 13, 0])]);
        assert!(cicp_from(&bytes[..]).is_none());
    }

    #[test]
    fn a_truncated_chunk_states_nothing() {
        let bytes = file(&[(b"IHDR", &[0; 13])]);
        let truncated = &bytes[..bytes.len() - CRC_LENGTH];
        assert!(cicp_from(truncated).is_none());
    }

    #[test]
    fn a_png_is_read_from_its_bytes_not_its_name() {
        // The extension is the fallback for a path whose bytes say nothing.
        for path in ["a.png", "b.PNG"] {
            assert!(owns(Path::new(path)), "{path}");
        }
        // `.apng` is one of this format's extensions, so it is a hint like any
        // other.
        assert!(owns(Path::new("b.apng")));
        for path in ["a.jpg", "c", "d.pngx"] {
            assert!(!owns(Path::new(path)), "{path}");
        }
        // And the content wins when there is one: a page under a name that says
        // nothing about it is still this module's. This is the case that was
        // described by the generic decoder and decoded here.
        assert!(owns(Path::new("tests/fixtures/cicp-rgb8.png")));
    }

    /// The reader the walk opened travels into the stream its fill reads from,
    /// and a duplicate brings none: a reader is a position in one file, and two
    /// streams cannot share one.
    ///
    /// This reads what the stream prints rather than reaching into the struct,
    /// because `RowStream` is `Debug` for a decode's own reasons and that print
    /// is the only view of this from outside the module. Whether the fill then
    /// writes the right picture is the validator's question, not this one's.
    #[test]
    fn the_stream_carries_the_reader_the_walk_opened() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("alpha-rgb8.png");
        let info = image_info(&path, true, None)
            .expect("the header is read")
            .expect("a png is taken over");
        let Pixels::Stream(rows) = stream(&info).expect("this png is walked") else {
            panic!("a png this module walks streams its rows");
        };
        assert!(
            format!("{rows:?}").contains("holds_reader: true"),
            "the walk's reader is in the stream: {rows:?}"
        );

        let duplicate = rows.duplicate();
        assert!(
            format!("{duplicate:?}").contains("holds_reader: false"),
            "a duplicate opens its own reader: {duplicate:?}"
        );
    }
}
