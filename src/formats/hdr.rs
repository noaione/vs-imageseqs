//! Radiance HDR, written here rather than taken from a crate.
//!
//! The plan's row for this format is the one that says *written here*: the
//! candidate crate gets the exponent and the orientations wrong, so what
//! replaces it is this file. The header is a handful of `key=value` lines and a
//! resolution, and the raster is four bytes a pixel in one of three encodings.
//!
//! Three behaviours are the rule rather than an incidental detail:
//!
//! - **A frame is always `Rgb32F`, native-endian**, whatever the file's samples
//!   are. There is no depth to work out and no palette to expand.
//! - **`value = component * 2^(exponent - 128) / 256`.** The exponent is turned
//!   into a float by constructing its bits directly rather than by calling
//!   `powi`, and **a zero exponent byte means black** -- it maps to exactly
//!   `0.0` rather than to the smallest normal. An exponent of one is the
//!   subnormal case and is handled separately. See [`radiance`].
//! - **The resolution line's signs are honoured.** They say which way the
//!   scanlines and the pixels within them run, and the axes may be stated in
//!   either order, which is eight spellings in all. Handing out the storage
//!   order instead would turn a picture upside down or mirror it, so this is
//!   where the tree is deliberately more capable than the reader it replaces:
//!   see the note in `docs/improvements/26-remove-image-rs.md`.
//!
//! The three scanline encodings are all read: the flat form, the new
//! per-component run-length form (whose four byte header begins `2, 2`), and the
//! old repeat-marker form.

use std::path::Path;

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error, image_head},
    error::{ImgSeqError, Result},
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// File extensions that hold a radiance picture.
const EXTENSIONS: [&str; 1] = ["hdr"];

/// The two signatures a file may start with. `RGBE` is the older spelling and
/// is accepted the way every other reader accepts it.
const SIGNATURES: [&[u8]; 2] = [b"#?RADIANCE", b"#?RGBE"];

/// How the resolution line says the raster is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    /// Pixels in one stored scanline.
    stored_width: u32,
    /// Stored scanlines.
    stored_height: u32,
    /// The picture's width, which follows the axes rather than the storage.
    output_width: u32,
    /// The picture's height, for the same reason.
    output_height: u32,
    /// Whether the scanlines run bottom to top.
    flip_rows: bool,
    /// Whether the pixels within a scanline run right to left.
    flip_columns: bool,
    /// Whether the axes are stated the other way round, so the picture is
    /// transposed out of its storage order.
    transposed: bool,
}

impl Layout {
    /// The size of the picture handed out.
    const fn output(&self) -> (u32, u32) {
        (self.output_width, self.output_height)
    }
}

/// What one picture's header states.
#[derive(Debug)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    layout: Layout,
    /// Where the raster begins.
    data_offset: usize,
}

impl Header {
    /// The colour type a frame is written from, which is always the same.
    #[must_use]
    pub const fn color_type(&self) -> ColorType {
        ColorType::Rgb32F
    }

    /// The label property's value.
    #[must_use]
    pub const fn source(&self) -> SourceColorType {
        SourceColorType::Rgb32F
    }

    /// The format a frame is written from.
    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        PixelFormat::Rgb32F
    }
}

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// Reads one line, without its terminator.
fn read_line<'a>(data: &'a [u8], cursor: &mut usize) -> Option<&'a [u8]> {
    let start = *cursor;
    while let Some(&byte) = data.get(*cursor) {
        *cursor += 1;
        if byte == b'\n' {
            let end = if *cursor >= start + 2 && data[*cursor - 2] == b'\r' {
                *cursor - 2
            } else {
                *cursor - 1
            };
            return Some(&data[start..end]);
        }
    }
    None
}

/// Parses one token of the resolution line.
fn tag(value: &[u8]) -> Option<(bool, bool)> {
    // The **minus** sign is the natural reading order: `-Y` is top to bottom and
    // `-X` is left to right, so it asks for no movement at all, and it is why
    // `-Y h +X w` is the common spelling. `+Y` and `+X` are the ones that have
    // to be turned round.
    match value {
        b"-Y" => Some((false, false)),
        b"+Y" => Some((true, false)),
        b"+X" => Some((false, true)),
        b"-X" => Some((true, true)),
        _ => None,
    }
}

/// Parses the resolution line into a layout.
///
/// Either axis may be stated first. A vertical tag gives the height and a
/// horizontal one the width; when the horizontal comes first the picture is
/// stored transposed and has to be turned back on the way out.
fn resolution(line: &[u8]) -> Result<Layout> {
    let text = std::str::from_utf8(line)
        .map_err(|_| ImgSeqError::new("the resolution line is not text"))?;
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() != 4 {
        return Err(ImgSeqError::new(format!(
            "the resolution line states {} fields where four belong",
            parts.len()
        )));
    }
    let (flip_first, first_is_x) = tag(parts[0].as_bytes())
        .ok_or_else(|| ImgSeqError::new(format!("{:?} is not a resolution axis", parts[0])))?;
    let (flip_second, second_is_x) = tag(parts[2].as_bytes())
        .ok_or_else(|| ImgSeqError::new(format!("{:?} is not a resolution axis", parts[2])))?;
    if first_is_x == second_is_x {
        return Err(ImgSeqError::new(
            "the resolution line states the same axis twice",
        ));
    }
    let first: u32 = parts[1]
        .parse()
        .map_err(|_| ImgSeqError::new(format!("{:?} is not a size", parts[1])))?;
    let second: u32 = parts[3]
        .parse()
        .map_err(|_| ImgSeqError::new(format!("{:?} is not a size", parts[3])))?;
    if first == 0 || second == 0 {
        return Err(ImgSeqError::new("the resolution states no pixels"));
    }
    // The first axis is the major one: it counts the scanlines.
    // The raster is always `first` scanlines of `second` pixels, whatever the
    // axes are called. The picture's size follows the axes, so stating X first
    // does not change it -- it only means the raster is stored transposed.
    let (output_width, output_height) = if first_is_x {
        (first, second)
    } else {
        (second, first)
    };
    Ok(Layout {
        stored_width: second,
        stored_height: first,
        output_width,
        output_height,
        flip_rows: flip_first,
        flip_columns: flip_second,
        transposed: first_is_x,
    })
}

/// Parses the header, and reports where the raster begins.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is not a radiance picture, when a line
/// is malformed, or when a size is not one this reader takes.
pub fn header(data: &[u8]) -> Result<Header> {
    let mut cursor = 0usize;
    let first = read_line(data, &mut cursor).ok_or_else(truncated)?;
    if !SIGNATURES.contains(&first) {
        return Err(ImgSeqError::new(
            "not a radiance picture: no RADIANCE or RGBE signature",
        ));
    }
    // Variable lines run until the first blank one. An unknown field is skipped
    // rather than refused, which is what the format asks for and what makes a
    // file this reader has never seen still readable.
    let mut format_seen = false;
    loop {
        let line = read_line(data, &mut cursor).ok_or_else(truncated)?;
        if line.is_empty() {
            break;
        }
        if line.starts_with(b"FORMAT=") {
            let value = &line[b"FORMAT=".len()..];
            // Only the r,g,b,e spelling is taken. `xyze` is a colour space this
            // reader does not convert, so handing it out as r,g,b would be wrong.
            if value != b"32-bit_rle_rgbe" {
                return Err(ImgSeqError::new(format!(
                    "the header states the format {:?}, which is not one this reader takes",
                    String::from_utf8_lossy(value)
                )));
            }
            format_seen = true;
        }
    }
    if !format_seen {
        return Err(ImgSeqError::new("the header states no FORMAT"));
    }
    let line = read_line(data, &mut cursor).ok_or_else(truncated)?;
    let layout = resolution(line)?;
    let (width, height) = layout.output();
    Ok(Header {
        width,
        height,
        layout,
        data_offset: cursor,
    })
}

fn truncated() -> ImgSeqError {
    ImgSeqError::new("the picture is truncated")
}

/// Turns one radiance pixel into the three floats it stands for.
///
/// The exponent becomes a float by assembling the bits of `2^(e - 1)` and
/// scaling by `2^-8`, which is `2^(e - 128) / 256`. An exponent of zero is
/// **black**: the branch that handles it yields exactly `0.0`. An exponent of
/// one is the subnormal case, because `f32`'s smallest normal exponent is above
/// what `2^(1 - 128)` needs, and the shift by 22 rather than 23 lands it there.
fn radiance(component: [u8; 3], exponent: u8) -> [f32; 3] {
    let exp = f32::from_bits(if exponent > 1 {
        (u32::from(exponent) - 1) << 23
    } else {
        u32::from(exponent) << 22
    }) * 0.003_906_25;
    [
        exp * f32::from(component[0]),
        exp * f32::from(component[1]),
        exp * f32::from(component[2]),
    ]
}

/// Reads one pixel's four bytes.
fn pixel(data: &[u8], cursor: &mut usize) -> Result<[u8; 4]> {
    let bytes: [u8; 4] = data
        .get(*cursor..*cursor + 4)
        .ok_or_else(truncated)?
        .try_into()
        .map_err(|_| truncated())?;
    *cursor += 4;
    Ok(bytes)
}

/// Reads one byte.
fn byte(data: &[u8], cursor: &mut usize) -> Result<u8> {
    let value = *data.get(*cursor).ok_or_else(truncated)?;
    *cursor += 1;
    Ok(value)
}

/// Expands one component plane in the new run-length form.
///
/// A count above 128 introduces a run of `count - 128` copies of the next byte;
/// a count of 128 or less introduces that many literal bytes. The two are
/// distinguished by the count alone, which is why the header's third byte has
/// to be below 128 for the form to apply at all.
fn component(data: &[u8], cursor: &mut usize, width: usize, out: &mut [u8]) -> Result<()> {
    let mut position = 0usize;
    while position < width {
        let count = byte(data, cursor)?;
        if count <= 128 {
            let count = count as usize;
            if position + count > width {
                return Err(ImgSeqError::new("a scanline is longer than it states"));
            }
            let values = data.get(*cursor..*cursor + count).ok_or_else(truncated)?;
            out[position..position + count].copy_from_slice(values);
            *cursor += count;
            position += count;
        } else {
            let count = (count - 128) as usize;
            if position + count > width {
                return Err(ImgSeqError::new("a scanline is longer than it states"));
            }
            let value = byte(data, cursor)?;
            out[position..position + count].fill(value);
            position += count;
        }
    }
    Ok(())
}

/// Reads one scanline of `width` pixels into `out`.
///
/// The first four bytes decide the encoding, and they are the first pixel
/// themselves in the two forms that have no header.
fn scanline(data: &[u8], cursor: &mut usize, width: usize, out: &mut [[u8; 4]]) -> Result<()> {
    let first = pixel(data, cursor)?;
    if first[0] == 2 && first[1] == 2 && first[2] < 128 {
        // The new per-component form: four planes, each expanded on its own.
        let mut planes = vec![0u8; width * 4];
        for index in 0..4 {
            component(
                data,
                cursor,
                width,
                &mut planes[index * width..(index + 1) * width],
            )?;
        }
        for (index, target) in out.iter_mut().enumerate() {
            *target = [
                planes[index],
                planes[width + index],
                planes[2 * width + index],
                planes[3 * width + index],
            ];
        }
        return Ok(());
    }
    // The old forms: the first pixel is data, and a marker repeats the pixel
    // before it. A multiplier lets consecutive markers describe a longer run.
    if first[..3] == [1, 1, 1] {
        return Err(ImgSeqError::new(
            "the first pixel of a scanline is a run marker",
        ));
    }
    out[0] = first;
    let mut position = 1usize;
    let mut previous = first;
    let mut multiplier = 1usize;
    while position < width {
        let current = pixel(data, cursor)?;
        if current[..3] == [1, 1, 1] {
            let run = usize::from(current[3]) * multiplier;
            multiplier *= 256;
            if position + run > width {
                return Err(ImgSeqError::new("a scanline is longer than it states"));
            }
            out[position..position + run].fill(previous);
            position += run;
        } else {
            out[position] = current;
            position += 1;
            previous = current;
            multiplier = 1;
        }
    }
    Ok(())
}

/// Reads the raster, and lays it out the way the resolution line asks.
fn raster(header: &Header, data: &[u8]) -> Result<Vec<u8>> {
    let layout = &header.layout;
    let stored_width = layout.stored_width as usize;
    let stored_height = layout.stored_height as usize;
    let mut cursor = header.data_offset;
    // One buffer, sliced per scanline. A `Vec` of `Vec`s is one heap allocation
    // per row, which on a 900 row picture is 900 of them, and this reader was
    // measured 1.33x slower than the one it replaced before it was flattened.
    let mut scanlines = vec![[0u8; 4]; stored_width * stored_height];
    for row in 0..stored_height {
        let line = &mut scanlines[row * stored_width..(row + 1) * stored_width];
        scanline(data, &mut cursor, stored_width, line)?;
    }

    // The signs move the pixels and the transpose turns the axes round, so the
    // picture handed out is the one the resolution describes.
    let (out_width, out_height) = header.layout.output();
    let mut floats = vec![0f32; out_width as usize * out_height as usize * 3];
    for (stored_row, line) in scanlines.chunks_exact(stored_width).enumerate() {
        for (stored_column, value) in line.iter().enumerate() {
            let row = if layout.flip_rows {
                stored_height - 1 - stored_row
            } else {
                stored_row
            };
            let column = if layout.flip_columns {
                stored_width - 1 - stored_column
            } else {
                stored_column
            };
            let (x, y) = if layout.transposed {
                (row, column)
            } else {
                (column, row)
            };
            let at = (y * out_width as usize + x) * 3;
            floats[at..at + 3].copy_from_slice(&radiance([value[0], value[1], value[2]], value[3]));
        }
    }

    let mut out = vec![0u8; floats.len() * 4];
    for (slot, value) in out.as_chunks_mut::<4>().0.iter_mut().zip(floats) {
        slot.copy_from_slice(&value.to_ne_bytes());
    }
    Ok(out)
}

/// What a picture states, when this module reads the file.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info(path: &Path, _apply_rotation: bool) -> Result<Option<ImageInfo>> {
    if !owns(path) {
        return Ok(None);
    }
    let data = image_head(path).map_err(|error| image_error("open", path, error))?;
    // A `.hdr` that does not start with a signature is not one however it is
    // named, so it is declined rather than refused.
    let signature = data.get(..10);
    if !signature.is_some_and(|value| SIGNATURES.contains(&value)) {
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
        // A radiance picture states an origin rather than an orientation, and
        // the reader applies it, so the picture handed out is already the way
        // up the resolution line means it.
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: header.format(),
    }))
}

/// Decodes a picture into one interleaved buffer of native-endian floats.
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

    /// The conversion, against values whose arithmetic is checkable by hand.
    ///
    /// A zero exponent is exactly zero -- the special case the format asks for
    /// -- and an exponent of one is the subnormal one, which is small but not
    /// zero. Both would be easy to get wrong in opposite directions.
    #[test]
    fn the_exponent_converts_the_way_the_format_says() {
        // A zero exponent is black whatever the components are.
        assert_eq!(radiance([255, 255, 255], 0), [0.0, 0.0, 0.0]);
        // The neutral exponent: 2^0 / 256, so a component is itself over 256.
        assert_eq!(radiance([255, 255, 255], 128), [255.0 / 256.0; 3]);
        assert_eq!(radiance([1, 0, 0], 128), [1.0 / 256.0, 0.0, 0.0]);
        // One step up doubles.
        assert_eq!(radiance([255, 255, 255], 129), [255.0 / 128.0; 3]);
        assert_eq!(radiance([255, 255, 255], 127), [255.0 / 512.0; 3]);
        // The subnormal case is tiny but not zero, and not negative.
        let subnormal = radiance([255, 255, 255], 1)[0];
        assert!(subnormal > 0.0, "{subnormal}");
        assert!(subnormal < 1e-36, "{subnormal}");
    }

    /// The resolution line's eight spellings, and the size and directions each
    /// one asks for.
    #[test]
    fn the_resolution_line_states_the_layout() {
        let cases = [
            ("-Y 23 +X 37", 37, 23, false, false, false),
            ("+Y 23 +X 37", 37, 23, true, false, false),
            ("-Y 23 -X 37", 37, 23, false, true, false),
            ("+Y 23 -X 37", 37, 23, true, true, false),
            // The axes are stated the other way round, so the raster is stored
            // transposed; the signs still belong to the axis they name.
            ("+X 37 -Y 23", 37, 23, false, false, true),
            ("-X 37 -Y 23", 37, 23, true, false, true),
        ];
        for (line, width, height, flip_rows, flip_columns, transposed) in cases {
            let layout = resolution(line.as_bytes()).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(layout.output(), (width, height), "{line}");
            assert_eq!(layout.flip_rows, flip_rows, "{line}");
            assert_eq!(layout.flip_columns, flip_columns, "{line}");
            assert_eq!(layout.transposed, transposed, "{line}");
        }
        // Refusals: the same axis twice, and a tag that is not an axis.
        assert!(resolution(b"-Y 4 -Y 4").is_err());
        assert!(resolution(b"-Z 4 +X 4").is_err());
        assert!(resolution(b"-Y 4 +X").is_err());
    }

    /// Every fixture that the reader this replaces accepted still reads, and the
    /// three orientations it refused now read too. That second half is the
    /// deliberate difference and is written into `CHANGELOG.md`.
    #[test]
    fn every_resolution_sign_and_axis_order_reads() {
        for (name, width, height) in [
            ("hdr-flat.hdr", 7, 23),
            ("hdr-oldrle.hdr", 7, 23),
            ("hdr-rle.hdr", 37, 23),
            ("hdr-fields.hdr", 7, 23),
            ("hdr-exponents.hdr", 7, 1),
            // The four sign combinations, which the replaced reader refused
            // three of.
            ("hdr-orient-top-left.hdr", 37, 23),
            ("hdr-orient-top-right.hdr", 37, 23),
            ("hdr-orient-bottom-left.hdr", 37, 23),
            ("hdr-orient-bottom-right.hdr", 37, 23),
        ] {
            let info = image_info(&fixture(name), true)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .unwrap_or_else(|| panic!("{name} is taken over"));
            assert_eq!((info.width, info.height), (width, height), "{name}");
            assert_eq!(info.color_type, ColorType::Rgb32F, "{name}");
            assert_eq!(info.original_color_type, SourceColorType::Rgb32F, "{name}");
            assert_eq!(info.format, PixelFormat::Rgb32F, "{name}");
            let decoded = decode(&info).unwrap_or_else(|error| panic!("{name}: {error}"));
            match decoded.pixels {
                Pixels::Interleaved { buffer, .. } => {
                    assert_eq!(buffer.len(), (width * height * 3 * 4) as usize, "{name}");
                }
                _ => panic!("{name} hands out one interleaved buffer"),
            }
        }
    }

    /// The signs are applied, so four files that store the same picture in four
    /// different ways decode to the same buffer. A reader that ignored the
    /// resolution would hand out the storage order and these would differ.
    #[test]
    fn the_signs_are_applied_rather_than_ignored() {
        let read = |name: &str| -> Vec<u8> {
            let info = image_info(&fixture(name), true)
                .expect("read")
                .expect("taken over");
            match decode(&info).expect("decodes").pixels {
                Pixels::Interleaved { buffer, .. } => buffer,
                _ => panic!("interleaved"),
            }
        };
        // `hdr-rle.hdr` is the common `-Y +X` spelling and holds the picture as
        // it is meant to be seen; the other three store it turned round and say
        // so.
        let expected = read("hdr-rle.hdr");
        for name in [
            "hdr-orient-top-left.hdr",
            "hdr-orient-top-right.hdr",
            "hdr-orient-bottom-left.hdr",
            "hdr-orient-bottom-right.hdr",
        ] {
            let got = read(name);
            assert_eq!(got.len(), expected.len(), "{name}");
            assert_eq!(
                got.iter().zip(&expected).filter(|(a, b)| a == b).count(),
                expected.len(),
                "{name} is the same picture as the common orientation"
            );
        }
    }
    /// A file that is not a radiance picture is declined; one whose header
    /// states a format this reader does not take is refused by name.
    #[test]
    fn an_unknown_format_is_refused_by_name() {
        let file = b"#?RADIANCE\nFORMAT=32-bit_rle_xyze\n\n-Y 1 +X 1\n";
        let error = header(file).expect_err("xyze is accepted, so this needs another");
        assert!(error.to_string().contains("xyze"), "{error}");

        let unknown = b"#?RADIANCE\nFORMAT=8-bit_rle_rgbe\n\n-Y 1 +X 1\n";
        let error = header(unknown).expect_err("an eight bit format is refused");
        assert!(error.to_string().contains("8-bit_rle_rgbe"), "{error}");

        assert!(!owns(Path::new("a.png")));
        assert!(owns(Path::new("a.hdr")));
        assert!(owns(Path::new("a.HDR")));
        assert!(
            image_info(&fixture("cicp-rgb8.png"), true)
                .expect("a png is not ours")
                .is_none()
        );
    }
}
