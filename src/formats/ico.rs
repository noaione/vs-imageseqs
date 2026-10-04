//! Windows icons, read by this tree's own port of the `image` decoder.
//!
//! The port is [`image 0.25.10`'s `src/codecs/ico/decoder.rs`][upstream]. An
//! icon is a directory of entries, and this module picks one and hands its
//! payload to a reader: a PNG payload is decoded by the `png` crate, and a DIB
//! payload by [`crate::formats::bmp`], which the icon format reuses with two
//! adjustments that are the whole of what makes an icon different from a
//! bitmap.
//!
//! [upstream]: https://github.com/image-rs/image/blob/v0.25.10/src/codecs/ico/decoder.rs
//!
//! Five behaviours are inherited rather than invented:
//!
//! - **Entries are scored by `(bits_per_pixel, area)`, and a tie keeps the last
//!   entry.** Bit depth dominates size, and the search starts from the final
//!   entry and replaces the incumbent only on a *strictly* greater score. See
//!   [`best_entry`].
//! - **The payload is sniffed, not declared.** The first eight bytes at an
//!   entry's offset are compared with the PNG signature; anything else is a
//!   bare DIB with no `BM` file header.
//! - **A DIB's height is doubled and halved again.** The header states twice
//!   the real height to account for the AND mask, whether or not one is
//!   present, so a 32 by 32 icon holds a DIB that says 64.
//! - **A DIB payload is forced to carry alpha.** The bitmap reader is told to
//!   add an alpha channel before it parses the header, so a thirty-two bit
//!   `BI_RGB` payload keeps its fourth byte. This is the *opposite* of the rule
//!   for a `.bmp` file, and the same bytes read either way.
//! - **The AND mask can only clear alpha.** A set mask bit makes a pixel fully
//!   transparent; a clear one leaves whatever alpha the payload already had.
//!
//! A DIB payload that is not thirty-two bits deep is refused rather than
//! guessed at. The corpus has one, in the multi-entry icon, but it is never
//! selected -- and a depth this reader has no alpha for would otherwise be
//! handed out as a fully transparent picture.

use std::path::Path;

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    formats::bmp,
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// The eight bytes that start a PNG.
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Bytes of the icon directory that precede the entries.
const DIRECTORY: usize = 6;

/// Bytes of one directory entry.
const ENTRY: usize = 16;

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    // The icon magic is four bytes that a Targa header shares, so content cannot
    // separate the two and this decides by name; see `identify::has_signature`.
    crate::formats::identify::from_extension(path) == Some(crate::formats::identify::Format::Ico)
}

/// One entry of the icon directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The stored width, where zero means 256.
    pub width: u8,
    /// The stored height, where zero means 256.
    pub height: u8,
    /// The depth the directory claims. It is what the search scores on, and it
    /// is not necessarily what the payload holds.
    pub bits_per_pixel: u16,
    /// How many bytes the payload occupies.
    pub length: u32,
    /// Where the payload starts.
    pub offset: u32,
}

impl Entry {
    /// The width, with zero standing for 256.
    #[must_use]
    pub const fn real_width(self) -> u32 {
        if self.width == 0 {
            256
        } else {
            self.width as u32
        }
    }

    /// The height, with zero standing for 256.
    #[must_use]
    pub const fn real_height(self) -> u32 {
        if self.height == 0 {
            256
        } else {
            self.height as u32
        }
    }

    /// Whether the payload's own size agrees with what the directory claims.
    ///
    /// Both sides are clamped at 256, because a directory entry stores its size
    /// in one byte and 256 does not fit.
    #[must_use]
    pub fn matches(self, width: u32, height: u32) -> bool {
        self.real_width() == width.min(256) && self.real_height() == height.min(256)
    }
}

/// Reads the directory, and reports what each entry claims.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the directory is truncated or states no
/// entries.
pub fn entries(data: &[u8]) -> Result<Vec<Entry>> {
    let header = data
        .get(..DIRECTORY)
        .ok_or_else(|| ImgSeqError::new("the icon is truncated"))?;
    let count = u16::from_le_bytes([header[4], header[5]]) as usize;
    if count == 0 {
        return Err(ImgSeqError::new("the icon holds no entries"));
    }
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let at = DIRECTORY + index * ENTRY;
        let entry = data
            .get(at..at + ENTRY)
            .ok_or_else(|| ImgSeqError::new("the icon directory is truncated"))?;
        let bits_per_pixel = u16::from_le_bytes([entry[6], entry[7]]);
        // 256 is the largest an entry can state, and anything larger would be a
        // hot spot rather than a depth: this reader takes no cursor files.
        if bits_per_pixel > 256 {
            return Err(ImgSeqError::new(
                "an icon entry states a bit depth no image has",
            ));
        }
        out.push(Entry {
            width: entry[0],
            height: entry[1],
            bits_per_pixel,
            length: u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]),
            offset: u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]),
        });
    }
    Ok(out)
}

/// The entry an icon's picture comes from: the deepest, then the largest.
///
/// The search starts from the **last** entry and replaces the incumbent only on
/// a strictly greater score, so a tie keeps the later entry.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the directory is empty.
pub fn best_entry(entries: &[Entry]) -> Result<Entry> {
    let (last, rest) = entries
        .split_last()
        .ok_or_else(|| ImgSeqError::new("the icon holds no entries"))?;
    let mut best = *last;
    let mut best_score = (best.bits_per_pixel, best.real_width() * best.real_height());
    for entry in rest {
        let score = (
            entry.bits_per_pixel,
            entry.real_width() * entry.real_height(),
        );
        if score > best_score {
            best = *entry;
            best_score = score;
        }
    }
    Ok(best)
}

/// The payload of `entry`, checked against the file.
fn payload<'a>(data: &'a [u8], entry: Entry, path: &Path) -> Result<&'a [u8]> {
    let start = entry.offset as usize;
    let end = start
        .checked_add(entry.length as usize)
        .ok_or_else(|| ImgSeqError::new("an icon entry is too large"))?;
    data.get(start..end).ok_or_else(|| {
        image_error(
            "decode",
            path,
            "the icon is shorter than its directory says",
        )
    })
}

/// What an icon states, when this module reads the file.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info(path: &Path, _apply_rotation: bool) -> Result<Option<ImageInfo>> {
    if !owns(path) {
        return Ok(None);
    }
    // The whole file, not a header: an icon's directory indexes payloads anywhere
    // in it and `entries` checks every offset against the buffer it was given, so a
    // truncated head reads as an icon shorter than its directory says. An icon is at
    // most 256x256, so there is nothing to save by reading less.
    let data = std::fs::read(path).map_err(|error| image_error("open", path, error))?;
    // An icon directory starts with a zero word and a type of one or two. A
    // file that does not is not an icon however it is named.
    let reserved = u16::from_le_bytes([*data.first().unwrap_or(&1), *data.get(1).unwrap_or(&0)]);
    if reserved != 0 || data.len() < DIRECTORY {
        return Ok(None);
    }
    let entries = entries(&data).map_err(|error| image_error("identify", path, error))?;
    let entry = best_entry(&entries).map_err(|error| image_error("identify", path, error))?;
    let payload = payload(&data, entry, path)?;

    // The payload decides the size, not the directory: a directory entry stores
    // its dimensions in one byte, and the picture is what the file really
    // holds.
    let (width, height) = if payload.starts_with(PNG_SIGNATURE) {
        png_dimensions(payload, path)?
    } else {
        let header = dib_header(payload, path)?;
        (header.width, header.height)
    };
    if !entry.matches(width, height) {
        return Err(image_error(
            "identify",
            path,
            format!(
                "the directory says {}x{} and the payload holds {width}x{height}",
                entry.real_width(),
                entry.real_height()
            ),
        ));
    }

    Ok(Some(ImageInfo {
        path: path.to_path_buf(),
        width,
        height,
        // Every icon this reader takes is handed out as r,g,b,a: a PNG payload
        // is required to be, and a DIB payload is forced to carry alpha.
        color_type: ColorType::Rgba8,
        original_color_type: SourceColorType::Rgba8,
        has_icc_profile: false,
        icc_profile: None,
        cicp: None,
        chroma_location: None,
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: PixelFormat::Rgb8,
    }))
}

/// The size a PNG payload states.
fn png_dimensions(payload: &[u8], path: &Path) -> Result<(u32, u32)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(payload));
    let reader = decoder
        .read_info()
        .map_err(|error| image_error("identify", path, error))?;
    let info = reader.info();
    Ok((info.width, info.height))
}

/// The header of a DIB payload, with the two adjustments an icon needs.
fn dib_header(payload: &[u8], path: &Path) -> Result<bmp::Header> {
    bmp::header_ico(payload, 0).map_err(|error| image_error("identify", path, error))
}

/// Decodes an icon into one interleaved buffer.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read, or when its payload is
/// a subtype this reader does not implement.
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = std::time::Instant::now();
    let data = std::fs::read(&info.path).map_err(|error| image_error("open", &info.path, error))?;
    let open = open_started.elapsed();

    let entries = entries(&data).map_err(|error| image_error("decode", &info.path, error))?;
    let entry = best_entry(&entries).map_err(|error| image_error("decode", &info.path, error))?;
    let payload = payload(&data, entry, &info.path)?;

    let read_started = std::time::Instant::now();
    let buffer = if payload.starts_with(PNG_SIGNATURE) {
        decode_png(payload, &info.path)?
    } else {
        decode_dib(payload, &info.path)?
    };
    let read = read_started.elapsed();

    Ok(DecodedImage {
        width: info.width,
        height: info.height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Interleaved {
            color_type: ColorType::Rgba8,
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

/// Decodes a PNG payload, which an icon requires to be r,g,b,a.
fn decode_png(payload: &[u8], path: &Path) -> Result<Vec<u8>> {
    let decoder = png::Decoder::new(std::io::Cursor::new(payload));
    let mut reader = decoder
        .read_info()
        .map_err(|error| image_error("decode", path, error))?;
    let mut buffer = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| image_error("decode", path, error))?;
    buffer.truncate(info.buffer_size());
    // The icon format allows only r,g,b,a payloads, and the AND mask a DIB
    // carries has no counterpart here: a payload that is not already four
    // channels would otherwise be handed out with an alpha nobody stated.
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(image_error(
            "decode",
            path,
            format!(
                "a png payload is {:?} at {:?} bits, and an icon allows only r,g,b,a at eight",
                info.color_type, info.bit_depth
            ),
        ));
    }
    Ok(buffer)
}

/// Decodes a DIB payload, then applies the AND mask that follows it.
fn decode_dib(payload: &[u8], path: &Path) -> Result<Vec<u8>> {
    let header = bmp::header_ico(payload, 0).map_err(|error| image_error("decode", path, error))?;
    let mut buffer =
        bmp::pixels(&header, payload).map_err(|error| image_error("decode", path, error))?;

    // The mask follows the pixel rows, one bit a pixel, padded to four bytes a
    // row, and only clears alpha: a set bit is fully transparent and a clear
    // one leaves what the payload had.
    let width = header.width as usize;
    let height = header.height as usize;
    // Two different row sizes, and confusing them reads pixels as mask bits.
    // The pixel rows are four bytes a pixel -- this reader takes only a
    // thirty-two bit payload -- and the mask rows are one *bit* a pixel padded
    // to four bytes, which is sixteen times narrower.
    let pixel_row_bytes = width * 4;
    let mask_row_bytes = width.div_ceil(32) * 4;
    let mask_start = header
        .data_offset
        .checked_add(pixel_row_bytes.saturating_mul(height))
        .ok_or_else(|| image_error("decode", path, "the icon is too large"))?;
    // The mask is required by the format and absent in the wild, so a payload
    // that simply ends after its pixels is accepted rather than refused.
    let available = payload.len().saturating_sub(mask_start);
    if available >= mask_row_bytes * height {
        let mask = &payload[mask_start..mask_start + mask_row_bytes * height];
        for y in 0..height {
            let row = &mask[y * mask_row_bytes..(y + 1) * mask_row_bytes];
            for x in 0..width {
                let byte = row[x / 8];
                if byte & (0x80 >> (x % 8)) != 0 {
                    // The rows run bottom to top, like the pixels.
                    let target = (height - y - 1) * width + x;
                    buffer[target * 4 + 3] = 0;
                }
            }
        }
    }
    Ok(buffer)
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

    /// The committed fixtures, and the entry each one selects. The multi-entry
    /// icon is the case that separates bit depth from area.
    #[test]
    fn the_selected_entry_is_the_deepest_then_the_largest() {
        for (name, width, height, entries) in [
            ("ico-dib32.ico", 32, 32, 1),
            ("ico-png.ico", 32, 32, 1),
            ("ico-multi.ico", 48, 48, 3),
        ] {
            let data = std::fs::read(fixture(name)).expect("the fixture is read");
            let all = entries_of(&data);
            assert_eq!(all.len(), entries, "{name}");
            let best = best_entry(&all).expect("one is selected");
            assert_eq!(
                (best.real_width(), best.real_height()),
                (width, height),
                "{name}"
            );
            let info = image_info(&fixture(name), true)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .unwrap_or_else(|| panic!("{name} is taken over"));
            assert_eq!((info.width, info.height), (width, height), "{name}");
            assert_eq!(info.color_type, ColorType::Rgba8, "{name}");
        }
    }

    fn entries_of(data: &[u8]) -> Vec<Entry> {
        entries(data).expect("the directory reads")
    }

    /// Depth dominates area, and a tie keeps the last entry.
    #[test]
    fn a_deeper_entry_wins_over_a_larger_one_and_a_tie_keeps_the_last() {
        let entry = |width: u8, height: u8, bits_per_pixel: u16| Entry {
            width,
            height,
            bits_per_pixel,
            length: 0,
            offset: 0,
        };
        // A deep but tiny entry beats a shallow but huge one.
        let deep_first = vec![entry(16, 16, 32), entry(48, 48, 8)];
        assert_eq!(best_entry(&deep_first).expect("selected").width, 16);
        // Equal depth, so the larger area wins.
        let larger_last = vec![entry(16, 16, 32), entry(48, 48, 32)];
        assert_eq!(best_entry(&larger_last).expect("selected").width, 48);
        // Equal depth and equal area: the *last* one is kept, because the
        // search starts there and only a strict improvement replaces it.
        let tie = vec![entry(32, 32, 32), entry(32, 32, 32)];
        let selected = best_entry(&tie).expect("selected");
        assert_eq!(selected, tie[1]);
        // Zero states 256 on both axes.
        assert_eq!(entry(0, 0, 32).real_width(), 256);
        assert_eq!(entry(0, 0, 32).real_height(), 256);
    }

    /// Every fixture decodes to four channels of the size the directory says,
    /// and the PNG payload is the one the research corpus did not have.
    #[test]
    fn the_payloads_decode_to_the_sizes_the_directory_states() {
        for (name, width, height) in [
            ("ico-dib32.ico", 32u32, 32u32),
            ("ico-png.ico", 32, 32),
            ("ico-multi.ico", 48, 48),
        ] {
            let info = image_info(&fixture(name), true)
                .expect("read")
                .expect("taken over");
            let decoded = decode(&info).unwrap_or_else(|error| panic!("{name}: {error}"));
            let Pixels::Interleaved {
                color_type, buffer, ..
            } = decoded.pixels
            else {
                panic!("an icon hands out one interleaved buffer");
            };
            assert_eq!(color_type, ColorType::Rgba8, "{name}");
            assert_eq!(buffer.len(), (width * height * 4) as usize, "{name}");
            assert_eq!((decoded.width, decoded.height), (width, height), "{name}");
        }
    }

    /// The committed icon's AND mask is all zeros, so its alpha is exactly the
    /// fourth byte the payload states. This is the case the baseline records:
    /// `ico-dib32.ico` and `ico-png.ico` hold the same picture and must agree
    /// on where it is transparent.
    #[test]
    fn a_clear_mask_leaves_the_payload_alpha_alone() {
        let data = std::fs::read(fixture("ico-dib32.ico")).expect("the fixture is read");
        let entry = best_entry(&entries(&data).expect("read")).expect("selected");
        let payload = payload(&data, entry, Path::new("ico-dib32.ico")).expect("the payload");
        let header = bmp::header_ico(payload, 0).expect("the header reads");
        let decoded = decode_dib(payload, Path::new("ico-dib32.ico")).expect("decodes");

        let pixel_row = header.width as usize * 4;
        let mask_start = header.data_offset + pixel_row * header.height as usize;
        let mask_row = (header.width as usize).div_ceil(32) * 4;
        let mask = &payload[mask_start..mask_start + mask_row * header.height as usize];
        assert!(
            mask.iter().all(|&byte| byte == 0),
            "this fixture's mask is all zeros, so nothing is cleared"
        );

        // Every pixel's alpha is the payload's own fourth byte, read bottom-up.
        let width = header.width as usize;
        for y in 0..header.height as usize {
            for x in 0..width {
                let stored = payload
                    [header.data_offset + (header.height as usize - 1 - y) * pixel_row + x * 4 + 3];
                let decoded_alpha = decoded[(y * width + x) * 4 + 3];
                assert_eq!(decoded_alpha, stored, "pixel {x},{y}");
            }
        }
    }

    /// A set mask bit clears alpha and nothing else, which is the rule the
    /// fixture above cannot show because its mask is empty. The bit is set by
    /// hand in the payload's own mask area.
    #[test]
    fn a_set_mask_bit_clears_alpha_and_leaves_the_colour() {
        let data = std::fs::read(fixture("ico-dib32.ico")).expect("the fixture is read");
        let entry = best_entry(&entries(&data).expect("read")).expect("selected");
        let payload = payload(&data, entry, Path::new("ico-dib32.ico")).expect("the payload");
        let header = bmp::header_ico(payload, 0).expect("the header reads");
        let pixel_row = header.width as usize * 4;
        let mask_start = header.data_offset + pixel_row * header.height as usize;

        let plain = decode_dib(payload, Path::new("ico-dib32.ico")).expect("decodes");

        // The top-left pixel is the last mask row's first bit, because both
        // the pixels and the mask run bottom to top.
        let mut marked = payload.to_vec();
        let last_row =
            mask_start + (header.height as usize - 1) * ((header.width as usize).div_ceil(32) * 4);
        marked[last_row] |= 0x80;
        let cleared = decode_dib(&marked, Path::new("ico-dib32.ico")).expect("decodes");

        assert_eq!(cleared[3], 0, "the marked pixel is transparent");
        assert_eq!(cleared[..3], plain[..3], "and keeps its colour");
        assert_eq!(cleared[4..], plain[4..], "and no other pixel moved");
    }

    /// A file that is not an icon is declined.
    #[test]
    fn only_an_icon_is_taken_over() {
        assert!(owns(Path::new("a.ico")));
        assert!(owns(Path::new("a.ICO")));
        assert!(!owns(Path::new("a.png")));
        assert!(
            image_info(&fixture("cicp-rgb8.png"), true)
                .expect("a png is not ours")
                .is_none()
        );
    }
}
