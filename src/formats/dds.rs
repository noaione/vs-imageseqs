//! DirectDraw surfaces, read by this tree's own port of the `image` decoder.
//!
//! The port is [`image 0.25.10`'s `src/codecs/dds.rs` and `src/codecs/dxt.rs`][upstream],
//! kept because both candidate crates are rejected: `ddsfile` brings a
//! proc-macro into the build graph, and `bcdec_rs` changes samples and panics on
//! a truncated block.
//!
//! [upstream]: https://github.com/image-rs/image/blob/v0.25.10/src/codecs/dds.rs
//!
//! What this reader accepts is deliberately narrow, and the narrowness is the
//! rule rather than an omission:
//!
//! - **Only DXT1, DXT3 and DXT5, or their DX10 equivalents.** The four
//!   character code names the first three; `DX10` names them again by DXGI
//!   format number, where BC1 is 70 to 72, BC2 73 to 75 and BC3 76 to 78. A
//!   code or a number outside those is refused.
//! - **The width and height must be multiples of four.** A block is four pixels
//!   square, so a surface that is not is refused before anything is read.
//! - **Mipmaps, other cube faces and other volume slices are ignored, not
//!   refused.** A header that states them is read, and only the first surface is
//!   decoded, which is what makes a cubemap or a mipped file usable at all.
//!
//! Three pieces of arithmetic are inherited, and the first is the one most
//! easily got wrong:
//!
//! - **A five or six bit channel widens by truncating division**, not by the
//!   round-to-nearest table the bitmap and targa readers share: five bits of `3`
//!   become `24` here and `25` there. See [`widen`].
//! - A DXT1 block has two modes, chosen by comparing its endpoints. Equal or
//!   descending endpoints mean three colours and a transparent black, where the
//!   other mode has four.
//! - A DXT5 alpha block has two modes too, chosen the same way, with eight
//!   interpolated levels or six and two constants.

use std::path::Path;

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error, image_head},
    error::{ImgSeqError, Result},
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// The four bytes every file starts with.
const MAGIC: &[u8; 4] = b"DDS ";

/// The size the header must state, which is the whole of it.
const HEADER_SIZE: u32 = 124;

/// Where the pixel data begins: the magic, the header and the reserved tail.
const DATA_OFFSET: usize = 4 + HEADER_SIZE as usize;

/// Bytes of the extra header a `DX10` file carries before its data.
const DX10_SIZE: usize = 20;

/// The flag bits a reader requires and the ones it tolerates.
const REQUIRED_FLAGS: u32 = 0x1 | 0x2 | 0x4 | 0x1000;
const VALID_FLAGS: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x1000 | 0x20_000 | 0x8_0000 | 0x0080_0000;

/// The pixel format flag that says the four character code is meaningful.
const PIXEL_FORMAT_FOURCC: u32 = 0x4;

/// Which block compression a file uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Variant {
    Dxt1,
    Dxt3,
    Dxt5,
}

impl Variant {
    /// The name a DXT-framed block carries, when this reader takes it.
    fn from_fourcc(fourcc: &[u8]) -> Option<Self> {
        match fourcc {
            b"DXT1" => Some(Self::Dxt1),
            b"DXT3" => Some(Self::Dxt3),
            b"DXT5" => Some(Self::Dxt5),
            _ => None,
        }
    }

    /// The variant a DXGI format number names, when this reader takes it.
    ///
    /// BC1 is the DXT1 family, BC2 the DXT3 one and BC3 the DXT5 one, and each
    /// is a triple of typeless, unorm and srgb.
    fn from_dxgi(format: u32) -> Option<Self> {
        match format {
            70..=72 => Some(Self::Dxt1),
            73..=75 => Some(Self::Dxt3),
            76..=78 => Some(Self::Dxt5),
            _ => None,
        }
    }

    /// The colour type the blocks decode to.
    const fn color_type(self) -> ColorType {
        match self {
            // A DXT1 block has no alpha of its own.
            Self::Dxt1 => ColorType::Rgb8,
            Self::Dxt3 | Self::Dxt5 => ColorType::Rgba8,
        }
    }

    /// Bytes one encoded block occupies.
    const fn encoded_bytes_per_block(self) -> usize {
        match self {
            Self::Dxt1 => 8,
            Self::Dxt3 | Self::Dxt5 => 16,
        }
    }
}

/// What one surface's header states.
#[derive(Debug)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    variant: Variant,
    /// Where the blocks begin, which is past the DX10 header when there is one.
    data_offset: usize,
}

impl Header {
    /// The colour type a frame is written from.
    #[must_use]
    pub const fn color_type(&self) -> ColorType {
        self.variant.color_type()
    }

    /// The label property's value, which is the decoded layout here.
    #[must_use]
    pub const fn source(&self) -> SourceColorType {
        match self.variant {
            Variant::Dxt1 => SourceColorType::Rgb8,
            Variant::Dxt3 | Variant::Dxt5 => SourceColorType::Rgba8,
        }
    }

    /// The format a frame is written from, which is eight bits a channel.
    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        PixelFormat::Rgb8
    }
}

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    // Content first: a file whose bytes say it is something else is that
    // something else however it is named, and the extension is the hint a
    // format with no signature of its own has to fall back on. See
    // [`identify::owns`](crate::formats::identify::owns).
    crate::formats::identify::owns(crate::formats::identify::Format::Dds, path)
}

/// Reads a little-endian `u32` at `offset`.
fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Parses the header of a surface.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is not a surface, when a header field
/// is outside what the format allows, or when the variant or the size is one
/// this reader does not take.
pub fn header(data: &[u8]) -> Result<Header> {
    if data.get(..4) != Some(MAGIC) {
        return Err(ImgSeqError::new("not a DirectDraw surface: no magic"));
    }
    let size = u32_at(data, 4).ok_or_else(truncated)?;
    if size != HEADER_SIZE {
        return Err(ImgSeqError::new(format!(
            "the header states {size} bytes where the format fixes {HEADER_SIZE}"
        )));
    }
    let flags = u32_at(data, 8).ok_or_else(truncated)?;
    if flags & (REQUIRED_FLAGS | !VALID_FLAGS) != REQUIRED_FLAGS {
        return Err(ImgSeqError::new(format!(
            "the header flags 0x{flags:x} are not the ones a surface requires"
        )));
    }
    let height = u32_at(data, 12).ok_or_else(truncated)?;
    let width = u32_at(data, 16).ok_or_else(truncated)?;
    if width == 0 || height == 0 {
        return Err(ImgSeqError::new("the header states no pixels"));
    }

    // The pixel format: its own size, its flags, and the code that names the
    // compression. Everything after the code is for an uncompressed layout this
    // reader does not take.
    let format_size = u32_at(data, 76).ok_or_else(truncated)?;
    if format_size != 32 {
        return Err(ImgSeqError::new(format!(
            "the pixel format states {format_size} bytes where the format fixes 32"
        )));
    }
    let format_flags = u32_at(data, 80).ok_or_else(truncated)?;
    if format_flags & PIXEL_FORMAT_FOURCC == 0 {
        return Err(ImgSeqError::new(
            "only a compressed surface is supported, and this one states a mask layout",
        ));
    }
    let fourcc = data.get(84..88).ok_or_else(truncated)?.to_vec();

    let (variant, data_offset) = if fourcc == b"DX10" {
        let dx10 = DATA_OFFSET;
        let dxgi = u32_at(data, dx10).ok_or_else(truncated)?;
        // The dimension and array size are validated rather than used: this
        // reader takes the first surface whatever shape the file promises. A
        // three dimensional texture with more than one slice is refused because
        // the slices cannot be expressed.
        let dimension = u32_at(data, dx10 + 4).ok_or_else(truncated)?;
        if !(2..=4).contains(&dimension) {
            return Err(ImgSeqError::new(format!(
                "the resource dimension {dimension} is not one the format defines"
            )));
        }
        let misc = u32_at(data, dx10 + 8).ok_or_else(truncated)?;
        if misc != 0 && misc != 0x4 {
            return Err(ImgSeqError::new(
                "the resource carries a flag the format does not define",
            ));
        }
        let array = u32_at(data, dx10 + 12).ok_or_else(truncated)?;
        if dimension == 4 && array != 1 {
            return Err(ImgSeqError::new(
                "a three dimensional surface must state one slice",
            ));
        }
        let variant = Variant::from_dxgi(dxgi).ok_or_else(|| {
            ImgSeqError::new(format!(
                "DXGI format {dxgi} is not one this reader implements"
            ))
        })?;
        (variant, DATA_OFFSET + DX10_SIZE)
    } else {
        let variant = Variant::from_fourcc(&fourcc).ok_or_else(|| {
            ImgSeqError::new(format!(
                "the four character code {:?} is not one this reader implements",
                String::from_utf8_lossy(&fourcc)
            ))
        })?;
        (variant, DATA_OFFSET)
    };

    // A block is four pixels square, so a surface that is not a multiple of four
    // cannot be read at all. The upstream reader refuses this at construction
    // rather than while decoding, and so does this one, which is what keeps the
    // probe and the decode from disagreeing.
    if !width.is_multiple_of(4) || !height.is_multiple_of(4) {
        return Err(ImgSeqError::new(format!(
            "a {width}x{height} surface is not a whole number of four by four blocks"
        )));
    }

    Ok(Header {
        width,
        height,
        variant,
        data_offset,
    })
}

fn truncated() -> ImgSeqError {
    ImgSeqError::new("the surface is truncated")
}

/// Widens a narrow channel to eight bits by truncating division.
///
/// This is *not* the round-to-nearest table the bitmap and targa readers share.
/// Five bits of `3` are `3 * 255 / 31`, which is `24`, where the other readers
/// give `25`. Keeping the two apart is the whole of what the plan means by
/// "image-rs's exact expansion".
const fn widen(value: u16, max: u16) -> u8 {
    (value * 0xFF / max) as u8
}

/// The three colours a five-six-five word stands for.
const fn from_565(value: u16) -> [u8; 3] {
    [
        widen((value >> 11) & 0x1F, 0x1F),
        widen((value >> 5) & 0x3F, 0x3F),
        widen(value & 0x1F, 0x1F),
    ]
}

/// The eight levels a DXT5 alpha block interpolates between.
///
/// The two endpoints choose the mode: descending ones interpolate six levels
/// between them, and equal or ascending ones interpolate four and spend the last
/// two entries on full transparency and full opacity.
fn alpha_levels(alpha0: u8, alpha1: u8) -> [u8; 8] {
    let mut table = [alpha0, alpha1, 0, 0, 0, 0, 0, 0xFF];
    if alpha0 > alpha1 {
        for (index, slot) in table.iter_mut().enumerate().take(8).skip(2) {
            let i = index as u16;
            *slot = (((8 - i) * u16::from(alpha0) + (i - 1) * u16::from(alpha1)) / 7) as u8;
        }
    } else {
        for (index, slot) in table.iter_mut().enumerate().take(6).skip(2) {
            let i = index as u16;
            *slot = (((6 - i) * u16::from(alpha0) + (i - 1) * u16::from(alpha1)) / 5) as u8;
        }
    }
    table
}

/// Decodes one eight byte DXT colour block into sixteen colours.
fn colour_block(source: &[u8], is_dxt1: bool) -> [[u8; 3]; 16] {
    let color0 = u16::from_le_bytes([source[0], source[1]]);
    let color1 = u16::from_le_bytes([source[2], source[3]]);
    let table = u32::from_le_bytes([source[4], source[5], source[6], source[7]]);

    let first = from_565(color0);
    let second = from_565(color1);
    let mut colours = [[0u8; 3]; 4];
    colours[0] = first;
    colours[1] = second;

    // A DXT1 block with endpoints that do not ascend has three colours and a
    // transparent black; every other case has four.
    if color0 > color1 || !is_dxt1 {
        for channel in 0..3 {
            colours[2][channel] =
                ((u16::from(first[channel]) * 2 + u16::from(second[channel]) + 1) / 3) as u8;
            colours[3][channel] =
                ((u16::from(first[channel]) + u16::from(second[channel]) * 2 + 1) / 3) as u8;
        }
    } else {
        for channel in 0..3 {
            colours[2][channel] =
                (u16::from(first[channel]) + u16::from(second[channel])).div_ceil(2) as u8;
        }
    }

    let mut out = [[0u8; 3]; 16];
    for (index, pixel) in out.iter_mut().enumerate() {
        *pixel = colours[((table >> (index * 2)) & 3) as usize];
    }
    out
}

/// Decodes one sixteen byte DXT3 block into sixteen pixels.
fn dxt3_block(source: &[u8]) -> [[u8; 4]; 16] {
    let mut packed = 0u64;
    for (shift, byte) in source[..8].iter().enumerate() {
        packed |= u64::from(*byte) << (shift * 8);
    }
    let colours = colour_block(&source[8..16], false);
    let mut out = [[0u8; 4]; 16];
    for (index, pixel) in out.iter_mut().enumerate() {
        let alpha = ((packed >> (index * 4)) & 0xF) as u8;
        // Four bits widen to eight by repeating them, which is exact.
        *pixel = [
            colours[index][0],
            colours[index][1],
            colours[index][2],
            alpha * 0x11,
        ];
    }
    out
}

/// Decodes one sixteen byte DXT5 block into sixteen pixels.
fn dxt5_block(source: &[u8]) -> [[u8; 4]; 16] {
    let mut packed = 0u64;
    for (shift, byte) in source[2..8].iter().enumerate() {
        packed |= u64::from(*byte) << (shift * 8);
    }
    let levels = alpha_levels(source[0], source[1]);
    let colours = colour_block(&source[8..16], false);
    let mut out = [[0u8; 4]; 16];
    for (index, pixel) in out.iter_mut().enumerate() {
        let alpha = levels[((packed >> (index * 3)) & 7) as usize];
        *pixel = [
            colours[index][0],
            colours[index][1],
            colours[index][2],
            alpha,
        ];
    }
    out
}

/// Decodes every block of a surface into one interleaved buffer.
fn blocks(header: &Header, data: &[u8]) -> Result<Vec<u8>> {
    let width = header.width as usize;
    let height = header.height as usize;
    let channels = match header.variant {
        Variant::Dxt1 => 3,
        Variant::Dxt3 | Variant::Dxt5 => 4,
    };
    let per_block = header.variant.encoded_bytes_per_block();
    let across = width / 4;
    let down = height / 4;
    let needed = across
        .checked_mul(down)
        .and_then(|count| count.checked_mul(per_block))
        .ok_or_else(|| ImgSeqError::new("the surface is too large"))?;
    let body = data
        .get(header.data_offset..header.data_offset + needed)
        .ok_or_else(|| {
            ImgSeqError::new(format!(
                "the file holds {} bytes of blocks where the header states {needed}",
                data.len().saturating_sub(header.data_offset)
            ))
        })?;

    let mut out = vec![0u8; width * height * channels];
    // The variant is matched **once**, outside the block loops. It cannot change
    // between blocks, and matching it inside made every block carry a branch and
    // left each arm's helpers unable to be inlined into a loop that only ever
    // runs for one of them.
    match header.variant {
        Variant::Dxt1 => {
            for block_y in 0..down {
                for block_x in 0..across {
                    let at = (block_y * across + block_x) * per_block;
                    let pixels = colour_block(&body[at..at + per_block], true);
                    for (index, pixel) in pixels.iter().enumerate() {
                        let (row, column) = (index / 4, index % 4);
                        let target = ((block_y * 4 + row) * width + block_x * 4 + column) * 3;
                        out[target..target + 3].copy_from_slice(pixel);
                    }
                }
            }
        }
        Variant::Dxt3 => {
            for block_y in 0..down {
                for block_x in 0..across {
                    let at = (block_y * across + block_x) * per_block;
                    let pixels = dxt3_block(&body[at..at + per_block]);
                    for (index, pixel) in pixels.iter().enumerate() {
                        let (row, column) = (index / 4, index % 4);
                        let target = ((block_y * 4 + row) * width + block_x * 4 + column) * 4;
                        out[target..target + 4].copy_from_slice(pixel);
                    }
                }
            }
        }
        Variant::Dxt5 => {
            for block_y in 0..down {
                for block_x in 0..across {
                    let at = (block_y * across + block_x) * per_block;
                    let pixels = dxt5_block(&body[at..at + per_block]);
                    for (index, pixel) in pixels.iter().enumerate() {
                        let (row, column) = (index / 4, index % 4);
                        let target = ((block_y * 4 + row) * width + block_x * 4 + column) * 4;
                        out[target..target + 4].copy_from_slice(pixel);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// What a surface states, when this module reads the file.
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
        |saved| saved == crate::formats::identify::Format::Dds,
    ) {
        return Ok(None);
    }
    let data = image_head(path).map_err(|error| image_error("open", path, error))?;
    // A `.dds` that does not start with the magic is not one however it is
    // named, so it is declined rather than refused and something else may read
    // it.
    if data.get(..4) != Some(MAGIC) {
        return Ok(None);
    }
    let header = header(&data).map_err(|error| image_error("identify", path, error))?;
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
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: header.format(),
    }))
}

/// Decodes a surface into one interleaved buffer.
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
        blocks(&header, &data).map_err(|error| image_error("decode", &info.path, error))?;
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

    /// The widening truncates, which is what separates this reader's arithmetic
    /// from the bitmap and targa ones. Five bits of three are the case that
    /// tells them apart: 24 here, 25 there.
    #[test]
    fn a_narrow_channel_widens_by_truncating_division() {
        assert_eq!(widen(3, 0x1F), 24);
        assert_eq!(crate::formats::bmp::expand(3, 5), 25);
        assert_eq!(widen(0, 0x1F), 0);
        assert_eq!(widen(0x1F, 0x1F), 255);
        assert_eq!(widen(0x3F, 0x3F), 255);
        // The whole five bit range, which every DXT colour block walks.
        let table: Vec<u8> = (0..32).map(|v| widen(v, 0x1F)).collect();
        assert_eq!(
            table,
            vec![
                // Truncating, so 4 is 32 where the round-to-nearest table has 33, and
                // 12 is 98 where it has 99. This is the row that separates them.
                0, 8, 16, 24, 32, 41, 49, 57, 65, 74, 82, 90, 98, 106, 115, 123, 131, 139, 148, 156,
                164, 172, 180, 189, 197, 205, 213, 222, 230, 238, 246, 255,
            ]
        );
    }

    /// Every fixture, and the layout its header asks for. The three DX10 and
    /// ignored-bits files are the ones that pin the rule's two halves.
    #[test]
    fn the_fixtures_state_the_variants_the_plan_names() {
        for (name, width, height, color_type, source) in [
            (
                "alpha-dds.dds",
                4,
                4,
                ColorType::Rgba8,
                SourceColorType::Rgba8,
            ),
            (
                "dds-dxt1.dds",
                40,
                24,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            (
                "dds-dxt3.dds",
                40,
                24,
                ColorType::Rgba8,
                SourceColorType::Rgba8,
            ),
            (
                "dds-dxt5.dds",
                40,
                24,
                ColorType::Rgba8,
                SourceColorType::Rgba8,
            ),
            (
                "dds-dx10-bc1.dds",
                40,
                24,
                ColorType::Rgb8,
                SourceColorType::Rgb8,
            ),
            (
                "dds-dx10-bc3.dds",
                40,
                24,
                ColorType::Rgba8,
                SourceColorType::Rgba8,
            ),
            // A stated mipmap count and the cube map and volume bits change
            // nothing: the first surface is read and the rest is ignored.
            (
                "dds-dxt5-ignored.dds",
                40,
                24,
                ColorType::Rgba8,
                SourceColorType::Rgba8,
            ),
        ] {
            let info = image_info(&fixture(name), true, None)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .unwrap_or_else(|| panic!("{name} is taken over"));
            assert_eq!((info.width, info.height), (width, height), "{name}");
            assert_eq!(info.color_type, color_type, "{name}");
            assert_eq!(info.original_color_type, source, "{name}");
            assert_eq!(info.format, PixelFormat::Rgb8, "{name}");
        }
    }

    /// The two spellings of the same variant decode to the same picture, and a
    /// file that states mipmaps and the cube and volume bits decodes to it too.
    /// This is the rule's "ignored" half, and it is a comparison rather than an
    /// assertion about the header.
    #[test]
    fn the_dx10_spelling_and_the_ignored_bits_reach_the_same_picture() {
        let decode_fixture = |name: &str| {
            let info = image_info(&fixture(name), true, None)
                .expect("read")
                .expect("taken over");
            let decoded = decode(&info).unwrap_or_else(|error| panic!("{name}: {error}"));
            match decoded.pixels {
                Pixels::Interleaved { buffer, .. } => buffer,
                _ => panic!("a surface hands out one interleaved buffer"),
            }
        };
        assert_eq!(
            decode_fixture("dds-dxt1.dds"),
            decode_fixture("dds-dx10-bc1.dds"),
            "DXT1 and BC1 are the same variant"
        );
        let bc3 = decode_fixture("dds-dx10-bc3.dds");
        assert_eq!(decode_fixture("dds-dxt5.dds"), bc3, "DXT5 and BC3 agree");
        assert_eq!(
            decode_fixture("dds-dxt5-ignored.dds"),
            bc3,
            "a mipmap count and the cube and volume bits change nothing"
        );
    }

    /// A DXT1 block has no alpha, so its buffer is three bytes a pixel where the
    /// others are four. The rule is about the variant rather than the depth.
    #[test]
    fn a_dxt1_block_has_no_alpha_plane() {
        let info = image_info(&fixture("dds-dxt1.dds"), true, None)
            .expect("read")
            .expect("taken over");
        assert_eq!(crate::pixel::alpha_channel(info.color_type), None);
        let decoded = decode(&info).expect("decodes");
        let Pixels::Interleaved { buffer, .. } = decoded.pixels else {
            panic!("interleaved");
        };
        assert_eq!(buffer.len(), 40 * 24 * 3);
        // And a four channel one is four bytes a pixel.
        let info = image_info(&fixture("dds-dxt5.dds"), true, None)
            .expect("read")
            .expect("taken over");
        let decoded = decode(&info).expect("decodes");
        let Pixels::Interleaved { buffer, .. } = decoded.pixels else {
            panic!("interleaved");
        };
        assert_eq!(buffer.len(), 40 * 24 * 4);
    }

    /// A surface that is not a whole number of blocks, and one whose code names
    /// a variant this reader does not take, are both refused by name.
    #[test]
    fn an_unported_variant_or_size_is_refused_by_name() {
        let mut odd = std::fs::read(fixture("dds-dxt1.dds")).expect("the fixture is read");
        // A width that is not a multiple of four.
        odd[16..20].copy_from_slice(&37u32.to_le_bytes());
        let error = header(&odd).expect_err("a 37 wide surface is refused");
        assert!(error.to_string().contains("blocks"), "{error}");

        // A four character code that is not one of the three.
        let mut code = std::fs::read(fixture("dds-dxt1.dds")).expect("the fixture is read");
        code[84..88].copy_from_slice(b"DX20");
        let error = header(&code).expect_err("an unknown code is refused");
        assert!(error.to_string().contains("DX20"), "{error}");

        assert!(!owns(Path::new("a.png")));
        assert!(owns(Path::new("a.dds")));
        assert!(owns(Path::new("a.DDS")));
        assert!(
            image_info(&fixture("cicp-rgb8.png"), true, None)
                .expect("a png is not ours")
                .is_none()
        );
    }

    /// The two DXT5 alpha modes are both reachable, and the one a block uses is
    /// decided by its endpoints rather than by the file.
    #[test]
    fn the_alpha_modes_interpolate_the_way_the_endpoints_ask() {
        // Descending endpoints: six levels between them, no constants.
        let descending = alpha_levels(200, 40);
        assert_eq!(descending[0], 200);
        assert_eq!(descending[1], 40);
        // ((8 - 7) * 200 + (7 - 1) * 40) / 7
        assert_eq!(descending[7], 62);
        assert_ne!(descending[7], 0xFF);
        // Ascending or equal: four levels and the two constants.
        let ascending = alpha_levels(40, 200);
        assert_eq!(ascending[6], 0);
        assert_eq!(ascending[7], 0xFF);
        assert_eq!(alpha_levels(100, 100)[7], 0xFF);
    }
}
