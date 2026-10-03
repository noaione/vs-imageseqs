//! Container reads for a png that the `image` decoder has no accessor for.
//!
//! `image` hands out the pixels of a png, its size and its ICC profile, but not
//! the `cICP` chunk, which is where a png states its own colour in the same
//! H.273 code points the other containers use. The chunk is a handful of bytes
//! in the metadata at the front of the file, so it is read from the file itself
//! rather than by decoding the picture; see
//! `docs/improvements/08-color-metadata.md`.
//!
//! This module reads the `cICP` chunk of every png, and it walks the rows of a
//! png the `image` decoder would otherwise materialise whole: see [`stream`]. It
//! places the samples itself and leaves everything it will not walk to the
//! `image` png decoder.

use std::{
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use image::ColorType;

use crate::{
    color::Cicp,
    decoder::{DecodeTimings, ImageInfo, Pixels, RowSink, RowStream, image_error},
    error::{ImgSeqError, Result},
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
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
}

/// The colour description a png states with a `cICP` chunk, or `None` when it
/// has none and when it is not a png at all.
pub fn cicp(path: &Path) -> Option<Cicp> {
    if !has_png_extension(path) {
        return None;
    }
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

fn has_png_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
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

/// What the `png` crate has to report for this walk to be the same picture
/// `image` would hand over.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Walkable {
    width: u32,
    height: u32,
    output: (png::ColorType, png::BitDepth),
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
/// so this is a header parse and not a decode; [`Rows::fill`] opens the file a
/// second time to read the rows themselves.
fn walkable(path: &Path) -> Option<Walkable> {
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
    Some(Walkable {
        width: header.width,
        height: header.height,
        output: reader.output_color_type(),
    })
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
#[derive(Debug)]
struct Rows {
    path: PathBuf,
    width: u32,
    height: u32,
    color_type: ColorType,
    /// Whether the rows carry an alpha channel, which is what says whether the
    /// alpha clip's plane has to be made opaque before the walk runs.
    has_alpha: bool,
}

/// The decode a png this module can walk is answered with, or `None` for one
/// that goes back to `image`.
///
/// Everything refused here is refused for the whole file, not for one frame, so
/// a sequence of them decides once per path.
pub fn stream(info: &ImageInfo) -> Option<Pixels> {
    if !owns(&info.path) || info.transform != Transform::IDENTITY {
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
    let expected = expanded_type(info.color_type)?;
    let header = walkable(&info.path)?;
    // The probe and this walk have to be describing the same picture, or the
    // frame the clip sized from the probe is not the frame these rows fit.
    if (header.width, header.height) != (info.width, info.height) || header.output != expected {
        return None;
    }
    Some(Pixels::Stream(Box::new(Rows {
        path: info.path.clone(),
        width: info.width,
        height: info.height,
        color_type: info.color_type,
        has_alpha: layout.channels != layout.colour_channels,
    })))
}

/// Places one decoded row into the planes of the frames of the call.
struct Placer<'a> {
    layout: Layout,
    sink: RowSink<'a>,
}

impl<'a> Placer<'a> {
    /// Checks the frames against the colour type before any row is written.
    ///
    /// The frame is built from the probe's answer and this walk from the
    /// header's, so the two are checked against each other here rather than
    /// trusted: a mismatch is a decode that refuses instead of a frame with a
    /// row in the wrong place.
    fn new(layout: Layout, sink: RowSink<'a>, width: u32, path: &Path) -> Result<Self> {
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
        Ok(Self { layout, sink })
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
        for (pixel, ((red_byte, green_byte), blue_byte)) in
            data.as_chunks::<3>().0.iter().zip(planes)
        {
            *red_byte = pixel[0];
            *green_byte = pixel[1];
            *blue_byte = pixel[2];
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
        let file =
            File::open(&self.path).map_err(|error| image_error("open", &self.path, error))?;
        let mut decoder = png::Decoder::new(BufReader::new(file));
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder
            .read_info()
            .map_err(|error| image_error("decode", &self.path, error))?;
        let open = open_started.elapsed();

        // The file was checked before this ran, so these reads are the same
        // header read again rather than a decision; a file that changed between
        // the two is refused rather than written into a frame it does not fit.
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
        let expected = expanded_type(self.color_type).ok_or_else(|| {
            ImgSeqError::new(format!(
                "png '{}' has a colour type this walk does not place",
                self.path.display()
            ))
        })?;
        if geometry != (self.width, self.height, false, false) || output != expected {
            return Err(ImgSeqError::new(format!(
                "png '{}' changed after probing",
                self.path.display()
            )));
        }

        let layout =
            Layout::of(self.color_type).expect("a colour type this walk places has a layout");
        let mut placer = Placer::new(layout, sink, self.width, &self.path)?;
        let read_started = Instant::now();
        let mut row = 0usize;
        while let Some(line) = reader
            .next_row()
            .map_err(|error| image_error("decode", &self.path, error))?
        {
            placer.place(line.data(), row)?;
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
    fn only_png_extensions_are_read() {
        for path in ["a.png", "b.PNG"] {
            assert!(has_png_extension(Path::new(path)), "{path}");
        }
        for path in ["a.jpg", "b.apng", "c", "d.pngx"] {
            assert!(!has_png_extension(Path::new(path)), "{path}");
        }
    }
}
